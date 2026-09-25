#!/usr/bin/env node
// Moves every Pending question from one oracle-escrow instance to another.
// See docs/migration.md for the full procedure and the safety argument.
//
//   node tools/migrate/migrate.mjs plan   --source C... --target C... --admin <key> [--network testnet]
//   node tools/migrate/migrate.mjs run    --source C... --target C... --admin <key> [--batch 10]
//   node tools/migrate/migrate.mjs verify --source C... --target C... --admin <key>
//
// Safety model: every on-chain step is ONE migrate_pending() transaction,
// which moves a whole batch or nothing (src/lib.rs). This process never has
// to be trusted to finish: it only ever reads the live pending set, sends a
// batch, and re-reads. Killing it at any point leaves every question either
// Pending on the source or Pending on the target, never both, never neither.
// The journal exists for the audit trail and for `verify`, not for
// correctness — deleting it cannot cause a double move.
//
// No npm dependencies: it shells out to the `stellar` CLI, which also holds
// the admin key (an alias from `stellar keys`), so this script never sees a
// secret.

import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync } from "node:fs";

const USAGE = `usage: migrate.mjs <plan|run|verify> --source <contract> --target <contract> --admin <stellar key alias>
  [--network testnet] [--batch 10] [--journal migration-journal.jsonl] [--json]
  [--legacy-ids 1-500 | --legacy-ids-file ids.txt]   (v0.2.0 sources, which have no list_pending)

env MIGRATE_CRASH=<before-send|after-send>:<n>  kill -9 this process around the n-th batch (testing)`;

// ---------------------------------------------------------------------------
// CLI plumbing
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const [cmd, ...rest] = argv;
  const o = { cmd, network: "testnet", batch: 10, journal: "migration-journal.jsonl", json: false };
  for (let i = 0; i < rest.length; i++) {
    const k = rest[i].replace(/^--/, "");
    if (k === "json") o.json = true;
    else o[k.replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = rest[++i];
  }
  o.batch = Number(o.batch);
  if (!["plan", "run", "verify"].includes(cmd) || !o.source || !o.target || !o.admin) {
    console.error(USAGE);
    process.exit(2);
  }
  if (!(o.batch >= 1 && o.batch <= 50)) throw new Error("--batch must be 1..50");
  return o;
}

class ContractError extends Error {
  constructor(code, stderr) {
    super(`contract error #${code}`);
    this.code = code;
    this.stderr = stderr;
  }
}

const ERRORS = {
  4: "QuestionAlreadyExists", 5: "QuestionNotFound", 6: "QuestionNotPending",
  17: "MigrationNotAuthorized", 18: "TokenMismatch", 19: "InvalidMigrationTarget",
};

function stellar(args, { retries = 3 } = {}) {
  for (let attempt = 1; ; attempt++) {
    const r = spawnSync("stellar", args, { encoding: "utf8" });
    if (r.status === 0) return r.stdout.trim();
    const err = `${r.stderr}\n${r.stdout}`;
    // A malformed invocation never reached the network: not a revert, a bug.
    if (/Failed to parse argument/.test(err)) throw new Error(`stellar ${args.join(" ")}: ${err.trim()}`);
    const m = err.match(/Error\(Contract, #(\d+)\)/);
    if (m) throw new ContractError(Number(m[1]), err);
    if (/non-existent|MissingValue|function not found|does not exist/i.test(err) && args.includes("list_pending"))
      throw new ContractError("no-list_pending", err);
    // Anything else (RPC hiccup, timeout) is retried: every call here is
    // either read-only or an atomic, idempotent-by-construction batch.
    if (attempt >= retries) throw new Error(`stellar ${args.join(" ")} failed:\n${err}`);
    spawnSync("sleep", [String(2 * attempt)]);
  }
}

function parseJson(s) {
  try {
    return JSON.parse(s);
  } catch {
    return s.replace(/^"|"$/g, "");
  }
}

function makeClient(o) {
  const base = (id, send) => ["contract", "invoke", "--id", id, "--source-account", o.admin, "--network", o.network, `--send=${send}`, "--"];
  const call = (id, fn, args = {}, send = "no") => {
    const flat = Object.entries(args).flatMap(([k, v]) => [`--${k}`, typeof v === "string" ? v : JSON.stringify(v)]);
    return parseJson(stellar([...base(id, send), fn, ...flat], { retries: send === "yes" ? 1 : 3 }));
  };
  return {
    call,
    question(id, qid) {
      try {
        const q = call(id, "get_question", { question_id: String(qid) });
        return { ...q, amount: BigInt(q.amount), created_at: Number(q.created_at), timeout_ledgers: Number(q.timeout_ledgers) };
      } catch (e) {
        if (e instanceof ContractError && e.code === 5) return null;
        throw e;
      }
    },
    pendingPage(id, start, limit) {
      return call(id, "list_pending", { start, limit }).map((x) => BigInt(x));
    },
    pendingCount(id) {
      return Number(call(id, "pending_count"));
    },
    tokenOf(id) {
      return call(id, "get_token");
    },
    balance(token, who) {
      return BigInt(call(token, "balance", { id: who }));
    },
    migrationSource(id) {
      const v = call(id, "get_migration_source");
      return v === "null" || v === null ? null : v;
    },
    latestLedger() {
      const out = stellar(["ledger", "latest", "--network", o.network, "--output", "json"]);
      return Number(JSON.parse(out).sequence);
    },
  };
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/**
 * Every Pending question id on `source`. Uses list_pending() on v0.3+
 * contracts; its swap-remove index can reorder under concurrent settlement,
 * so pages are read until pending_count() is the same before and after and
 * the id set has the size it reports. v0.2.0 contracts have no index: pass
 * --legacy-ids and each candidate id is probed with get_question().
 */
function enumeratePending(o, c) {
  if (o.legacyIds || o.legacyIdsFile) return enumerateLegacy(o, c);
  for (let attempt = 0; attempt < 5; attempt++) {
    const before = c.pendingCount(o.source);
    const ids = new Set();
    for (let start = 0; start < before; start += 100) {
      for (const id of c.pendingPage(o.source, start, 100)) ids.add(id);
    }
    const after = c.pendingCount(o.source);
    if (before === after && ids.size === after) return { ids: [...ids].sort(cmp), legacy: false };
  }
  throw new Error("pending set kept changing while paging; pause settlement or retry");
}

function enumerateLegacy(o, c) {
  let candidates = [];
  if (o.legacyIdsFile) {
    candidates = readFileSync(o.legacyIdsFile, "utf8").split(/\s+/).filter(Boolean).map(BigInt);
  } else {
    const [a, b] = o.legacyIds.split("-").map(BigInt);
    for (let i = a; i <= b; i++) candidates.push(i);
  }
  const ids = candidates.filter((id) => c.question(o.source, id)?.status === "Pending");
  return { ids, legacy: true };
}

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const chunk = (xs, n) => Array.from({ length: Math.ceil(xs.length / n) }, (_, i) => xs.slice(i * n, i * n + n));
const fmtAmount = (x) => `${(Number(x) / 1e7).toFixed(7)}`;

// ---------------------------------------------------------------------------
// plan (dry run)
// ---------------------------------------------------------------------------

function plan(o, c) {
  const ledger = c.latestLedger();
  const { ids, legacy } = enumeratePending(o, c);
  const questions = ids.map((id) => ({ id, ...c.question(o.source, id) }));
  const problems = [];
  let targetSource = null;
  if (!legacy) {
    targetSource = c.migrationSource(o.target);
    if (targetSource !== o.source)
      problems.push(`target's migration source is ${targetSource ?? "unset"}; run set_migration_source(${o.source}) on the target as its admin first`);
  }
  for (const q of questions) {
    if (c.question(o.target, q.id)) problems.push(`question ${q.id} already exists on the target; migrate_pending would revert its batch`);
  }
  const total = questions.reduce((s, q) => s + q.amount, 0n);
  const report = {
    mode: legacy ? "legacy-refund" : "migrate",
    network: o.network,
    ledger,
    source: o.source,
    target: o.target,
    pending: questions.length,
    total_amount: total.toString(),
    batches: chunk(questions.map((q) => q.id.toString()), o.batch),
    questions: questions.map((q) => ({
      id: q.id.toString(),
      payer: q.payer,
      amount: q.amount.toString(),
      created_at: q.created_at,
      refund_timeout_from: q.created_at + q.timeout_ledgers,
      past_deadline: ledger >= q.created_at + q.timeout_ledgers,
    })),
    problems,
  };
  return report;
}

function printPlan(r) {
  console.log(`DRY RUN — nothing will be sent. ${r.network} @ ledger ${r.ledger}`);
  console.log(`source ${r.source}\ntarget ${r.target}`);
  if (r.mode === "legacy-refund") {
    console.log("source is a v0.2.0 contract (no migrate_pending): the only fund-safe move is an admin refund() of each");
    console.log("question back to its payer, who re-submits on the target. `run` would do exactly that.");
  }
  console.log(`\n${r.pending} pending question(s), ${fmtAmount(r.total_amount)} total, in ${r.batches.length} batch(es):`);
  for (const q of r.questions)
    console.log(`  #${q.id.padEnd(8)} ${fmtAmount(q.amount).padStart(14)}  payer ${q.payer}  refundable from ledger ${q.refund_timeout_from}${q.past_deadline ? "  (already past deadline: anyone may refund it)" : ""}`);
  r.batches.forEach((b, i) => console.log(`  batch ${i + 1}: [${b.join(", ")}]`));
  if (r.problems.length) {
    console.log("\nWOULD FAIL:");
    for (const p of r.problems) console.log(`  - ${p}`);
  } else {
    console.log("\nno problems found");
  }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

function journal(o, entry) {
  appendFileSync(o.journal, JSON.stringify({ at: new Date().toISOString(), ...entry }, (_, v) => (typeof v === "bigint" ? v.toString() : v)) + "\n");
}

function readJournal(o) {
  if (!existsSync(o.journal)) return [];
  return readFileSync(o.journal, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
}

function crashPoint() {
  const m = (process.env.MIGRATE_CRASH || "").match(/^(before-send|after-send):(\d+)$/);
  return m ? { when: m[1], n: Number(m[2]) } : null;
}

function die(msg) {
  console.log(`\n*** simulated crash: ${msg} (kill -9) ***`);
  process.kill(process.pid, "SIGKILL");
}

function run(o, c) {
  const r = plan(o, c);
  if (r.problems.length) {
    printPlan(r);
    throw new Error("refusing to run: fix the problems above (or they would just revert on-chain)");
  }
  // The first run records a baseline; re-runs after a crash keep the
  // original one so `verify` can check the whole migration, not just the
  // tail end of it.
  if (!readJournal(o).some((e) => e.kind === "baseline")) {
    const token = c.tokenOf(o.source);
    journal(o, {
      kind: "baseline", ledger: r.ledger, source: o.source, target: o.target, token,
      source_balance: c.balance(token, o.source), target_balance: c.balance(token, o.target),
      questions: r.questions,
    });
  }
  journal(o, { kind: "run-start", pending: r.pending });

  const crash = crashPoint();
  let sent = 0;
  let failures = 0;
  for (;;) {
    // Always re-read the head of the live pending set instead of walking a
    // precomputed list: whatever a crashed run (or a still-in-flight tx)
    // already moved is simply no longer there.
    const ids = r.mode === "legacy-refund" ? enumerateLegacy(o, c).ids.slice(0, o.batch) : c.pendingPage(o.source, 0, o.batch);
    if (ids.length === 0) break;
    sent++;
    journal(o, { kind: "batch-intent", n: sent, ids });
    if (crash?.when === "before-send" && crash.n === sent) die(`before sending batch ${sent}`);
    try {
      let moved;
      if (r.mode === "legacy-refund") {
        for (const id of ids) c.call(o.source, "refund", { question_id: id.toString() }, "yes");
        moved = "refunded";
      } else {
        // u64s as bare JSON numbers built from the BigInts, so ids above
        // 2^53 aren't rounded (the CLI rejects them as strings).
        moved = c.call(o.source, "migrate_pending", { question_ids: `[${ids.join(",")}]`, target: o.target }, "yes");
      }
      if (crash?.when === "after-send" && crash.n === sent) die(`after batch ${sent} landed, before journaling it`);
      journal(o, { kind: "batch-done", n: sent, ids, moved });
      console.log(`batch ${sent}: [${ids.join(", ")}] -> ${typeof moved === "string" ? moved : fmtAmount(moved)}`);
    } catch (e) {
      if (!(e instanceof ContractError)) throw e;
      // A reverted batch changed nothing. The usual cause is racing another
      // run or the backend settling a question mid-batch; re-read and go on.
      failures++;
      journal(o, { kind: "batch-failed", n: sent, ids, error: e instanceof ContractError ? ERRORS[e.code] ?? e.code : String(e.message).slice(0, 500) });
      console.log(`batch ${sent}: [${ids.join(", ")}] reverted (${e instanceof ContractError ? ERRORS[e.code] ?? e.code : e.message.split("\n")[0]}); re-reading`);
      if (failures > 5) throw new Error("too many reverted batches in a row; stopping for a human to look");
      continue;
    }
    failures = 0;
  }
  journal(o, { kind: "run-done" });
  console.log("source has no pending questions left");
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

function verify(o, c) {
  const base = readJournal(o).find((e) => e.kind === "baseline");
  if (!base) throw new Error(`no baseline in ${o.journal}; verify checks a run's journal`);
  const checks = [];
  const ok = (name, pass, detail = "") => checks.push({ name, pass, detail });

  const legacy = !!(o.legacyIds || o.legacyIdsFile);
  let moved = 0n;
  let settledElsewhere = 0n;
  for (const q of base.questions) {
    const src = c.question(o.source, q.id);
    const dst = legacy ? null : c.question(o.target, q.id);
    const srcPending = src?.status === "Pending";
    const dstExists = !!dst;
    if (src?.status === "Migrated") {
      ok(`#${q.id} moved exactly once`, !!dst && !srcPending, dst ? `target status ${dst.status}` : "MISSING ON TARGET");
      if (dst) {
        ok(`#${q.id} terms preserved`, dst.payer === q.payer && dst.amount.toString() === q.amount && dst.created_at === q.created_at && dst.created_at + dst.timeout_ledgers === q.refund_timeout_from);
        moved += BigInt(q.amount);
      }
    } else if (srcPending) {
      ok(`#${q.id} still only on source`, !dstExists, dstExists ? "ALSO ON TARGET" : "not migrated yet");
    } else {
      // Resolved/refunded on the source while the migration ran: fine, as
      // long as it didn't ALSO get imported.
      ok(`#${q.id} settled on source (${src?.status}), not duplicated`, !dstExists);
      settledElsewhere += BigInt(q.amount);
    }
  }
  if (!legacy) {
    const srcNow = c.balance(base.token, o.source);
    const tgtNow = c.balance(base.token, o.target);
    const srcDelta = BigInt(base.source_balance) - srcNow;
    const tgtDelta = tgtNow - BigInt(base.target_balance);
    ok("target received exactly what was migrated", tgtDelta === moved, `target +${fmtAmount(tgtDelta)}, migrated ${fmtAmount(moved)}`);
    ok("source released exactly what was migrated (plus any settlements)", srcDelta >= moved && srcDelta <= moved + settledElsewhere,
      `source -${fmtAmount(srcDelta)}, migrated ${fmtAmount(moved)}, settled on source ${fmtAmount(settledElsewhere)}`);
    const srcPending = new Set(enumeratePending(o, c).ids.map(String));
    const tgtPending = new Set();
    const tc = c.pendingCount(o.target);
    for (let s = 0; s < tc; s += 100) for (const id of c.pendingPage(o.target, s, 100)) tgtPending.add(String(id));
    const both = [...srcPending].filter((x) => tgtPending.has(x));
    ok("no question pending on both contracts", both.length === 0, both.join(","));
  }
  const failed = checks.filter((x) => !x.pass);
  for (const x of checks) console.log(`${x.pass ? "ok  " : "FAIL"} ${x.name}${x.detail ? `  (${x.detail})` : ""}`);
  console.log(failed.length ? `\n${failed.length} check(s) FAILED` : `\nall ${checks.length} checks passed`);
  return failed.length === 0;
}

// ---------------------------------------------------------------------------

const o = parseArgs(process.argv.slice(2));
const c = makeClient(o);
if (o.cmd === "plan") {
  const r = plan(o, c);
  if (o.json) console.log(JSON.stringify(r, null, 2));
  else printPlan(r);
  process.exit(r.problems.length ? 1 : 0);
} else if (o.cmd === "run") {
  run(o, c);
} else {
  process.exit(verify(o, c) ? 0 : 1);
}
