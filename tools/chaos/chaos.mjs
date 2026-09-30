#!/usr/bin/env node
// RPC-failure chaos testing (issue #121; see docs/RPC_CHAOS_TESTING.md).
//
//   npm --prefix tools/chaos install
//
//   # 1. Self-contained: in-process chaos proxy + a client that makes the same
//   #    RPC calls stellarClient.js makes, through the real retry.js if given.
//   node tools/chaos/chaos.mjs lifecycle --contract C... --admin-secret S... \
//        [--rpc http://localhost:8000/rpc] [--friendbot http://localhost:8000/friendbot] \
//        [--passphrase "..."] [--profile tools/chaos/profiles/blips.json] \
//        [--submit-profile ...] [--keeper-profile tools/chaos/profiles/none.json] [--seed 1] [--questions 12] \
//        [--refund-share 0.25] [--retry-module ../arbiter-backend/src/retry.js] \
//        [--attempts 2] [--attempt-timeout-ms 8000] [--base-delay-ms 500] [--out report.json]
//
//   # 2. Against the real backend: put the proxy in front of its RPC (and
//   #    Horizon), point SOROBAN_RPC_URL / HORIZON_URL at it, drive traffic.
//   node tools/chaos/chaos.mjs proxy --upstream https://soroban-testnet.stellar.org \
//        [--kind rpc|horizon] [--port 8010] [--profile ...] [--seed 1] [--log events.jsonl]
//
//   # 3. The pass/fail check for (2), or for any deployment: no question may
//   #    sit Pending past its deadline. Reads go direct, never through chaos.
//   node tools/chaos/chaos.mjs verify --contract C... --read-source G... [--rpc ...] \
//        [--ids ids.txt] [--grace-ledgers 0] [--sweep --keeper-secret S...] [--out verify.json]
//
// Exit code 1 = a question was left stuck, or the retry layer broke its own
// spec (too many attempts, backoff too short, per-attempt timeout not
// enforced). Everything else is a finding in the report, not a failure.

import {
  Account,
  Address,
  BASE_FEE,
  Contract,
  Keypair,
  TransactionBuilder,
  nativeToScVal,
  rpc,
  scValToNative,
  xdr,
} from "@stellar/stellar-sdk";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { createChaosProxy } from "./proxy.mjs";
import { checkRetrySpec, instrument, loadRetry } from "./retry.mjs";

const LOCAL_PASSPHRASE = "Standalone Network ; February 2017";
const QUESTION_NOT_FOUND = 5;
const QUESTION_NOT_PENDING = 6;
const TERMINAL = new Set(["Resolved", "Refunded", "Migrated"]);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const u64 = (n) => nativeToScVal(BigInt(n), { type: "u64" });
const i128 = (n) => nativeToScVal(BigInt(n), { type: "i128" });
const addr = (g) => new Address(typeof g === "string" ? g : g.publicKey()).toScVal();
const addrVec = (gs) => xdr.ScVal.scvVec(gs.map(addr));
const json = (o) => JSON.stringify(o, (_, v) => (typeof v === "bigint" ? v.toString() : v), 2);

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const [cmd, ...rest] = argv;
  const o = {
    cmd,
    rpc: "http://localhost:8000/rpc",
    friendbot: "http://localhost:8000/friendbot",
    passphrase: LOCAL_PASSPHRASE,
    upstream: null,
    kind: "rpc",
    port: 0,
    seed: 1,
    profile: new URL("./profiles/blips.json", import.meta.url).pathname,
    submitProfile: null,
    keeperProfile: null,
    questions: 12,
    workers: 4,
    quorum: 2,
    amount: 10_000_000,
    refundShare: 0.25,
    attempts: 2,
    attemptTimeoutMs: 8000,
    baseDelayMs: 500,
    pollMs: 1000,
    pollTimeoutMs: 30_000,
    keeperPolls: 60,
    keeperPollMs: 5000,
    graceLedgers: 0,
    sweep: false,
  };
  for (let i = 0; i < rest.length; i++) {
    const k = rest[i].replace(/^--/, "").replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    if (k === "sweep") {
      o.sweep = true;
      continue;
    }
    const v = rest[++i];
    o[k] = typeof o[k] === "number" ? Number(v) : v;
  }
  const need = {
    lifecycle: ["contract", "adminSecret"],
    proxy: ["upstream"],
    verify: ["contract", "readSource"],
  }[cmd];
  if (!need || need.some((k) => !o[k]) || (o.sweep && !o.keeperSecret)) {
    console.error(readFileSync(new URL(import.meta.url)).toString().split("\n").slice(1, 25).join("\n"));
    process.exit(2);
  }
  return o;
}

const loadProfile = (p) => (p ? JSON.parse(readFileSync(p, "utf8")) : {});

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

class ContractCallError extends Error {
  constructor(code, text) {
    super(`contract#${code}: ${String(text).slice(0, 80)}`);
    this.code = code;
  }
}

function contractCode(text) {
  const m = String(text).match(/Error\(Contract, #(\d+)\)/);
  return m ? Number(m[1]) : null;
}

/// A contract error is deterministic: retrying the same read or simulation
/// can't change it, so it isn't an RPC fault and shouldn't be retried.
const nonRetryable = (e) => e instanceof ContractCallError;

function statusOf(q) {
  const s = q?.status;
  return Array.isArray(s) ? s[0] : s;
}

// ---------------------------------------------------------------------------
// The client under test: the same RPC call sites as stellarClient.js
// ---------------------------------------------------------------------------

/// Every RPC call goes through `call(label, fn)`: retry(instrumented fn).
/// `label` names the stellarClient.js call site it stands for, so the
/// report can say which call sites the faults hit and how each recovered.
class Client {
  constructor(o, url, retry) {
    this.o = o;
    this.server = new rpc.Server(url, { allowHttp: url.startsWith("http://") });
    this.contract = new Contract(o.contract);
    this.retry = retry;
    this.attempts = [];
    this.nextCall = 0;
  }

  call(label, fn) {
    const callId = `${label}#${this.nextCall++}`;
    return this.retry(instrument(this.attempts, { callId, label }, fn), {
      attempts: this.o.attempts,
      timeoutMs: this.o.attemptTimeoutMs,
      baseDelayMs: this.o.baseDelayMs,
      nonRetryable,
    });
  }

  build(account, op) {
    return new TransactionBuilder(account, { fee: BASE_FEE, networkPassphrase: this.o.passphrase })
      .addOperation(op)
      .setTimeout(60)
      .build();
  }

  /// Read-only contract call by simulation. Throws ContractCallError for a
  /// contract error, anything else for an RPC failure.
  read(label, readSource, method, args) {
    return this.call(label, async () => {
      const tx = this.build(new Account(readSource, "0"), this.contract.call(method, ...args));
      const sim = await this.server.simulateTransaction(tx);
      if (rpc.Api.isSimulationError(sim)) {
        const code = contractCode(sim.error);
        if (code != null) throw new ContractCallError(code, sim.error);
        throw new Error(`simulation: ${String(sim.error).slice(0, 80)}`);
      }
      return scValToNative(sim.result.retval);
    });
  }

  /// Full submission path: getAccount → simulate → assemble/sign →
  /// sendTransaction → poll getTransaction. Never throws. Returns what the
  /// CLIENT believes happened; `hash` lets the verifier check what really
  /// happened on chain.
  async invoke(label, kp, method, args) {
    const pk = kp.publicKey();
    let hash = null;
    try {
      const acct = await this.call(`${label}:getAccount`, () => this.server.getAccount(pk));
      const tx = this.build(new Account(pk, acct.sequenceNumber()), this.contract.call(method, ...args));
      const sim = await this.call(`${label}:simulate`, () => this.server.simulateTransaction(tx));
      if (rpc.Api.isSimulationError(sim)) {
        const code = contractCode(sim.error);
        return { ok: false, error: code != null ? `contract#${code}` : "simulation", code };
      }
      const prepared = rpc.assembleTransaction(tx, sim).build();
      prepared.sign(kp);
      hash = prepared.hash().toString("hex");

      // Re-sending the same signed envelope is idempotent (DUPLICATE), so
      // this retry can never double-apply.
      const sent = await this.call(`${label}:send`, async () => {
        const r = await this.server.sendTransaction(prepared);
        if (r.status === "TRY_AGAIN_LATER") throw new Error("TRY_AGAIN_LATER");
        return r;
      });
      if (sent.status !== "PENDING" && sent.status !== "DUPLICATE") {
        return { ok: false, error: `send:${sent.status}`, hash };
      }

      const until = Date.now() + this.o.pollTimeoutMs;
      while (Date.now() < until) {
        const r = await this.call(`${label}:getTransaction`, () => this.server.getTransaction(hash));
        if (r.status === "SUCCESS") return { ok: true, hash, ledger: r.ledger };
        if (r.status === "FAILED") return { ok: false, error: "tx-failed", hash };
        await sleep(this.o.pollMs);
      }
      return { ok: false, error: "confirm-timeout", hash };
    } catch (e) {
      return { ok: false, error: `rpc:${String(e?.message ?? e).slice(0, 60)}`, hash };
    }
  }
}

/// Direct, chaos-free view of the chain: the source of truth for verdicts.
class Truth {
  constructor(o) {
    this.o = o;
    this.server = new rpc.Server(o.rpc, { allowHttp: o.rpc.startsWith("http://") });
    this.contract = new Contract(o.contract);
  }

  async read(readSource, method, args) {
    const tx = new TransactionBuilder(new Account(readSource, "0"), { fee: BASE_FEE, networkPassphrase: this.o.passphrase })
      .addOperation(this.contract.call(method, ...args))
      .setTimeout(60)
      .build();
    const sim = await this.server.simulateTransaction(tx);
    if (rpc.Api.isSimulationError(sim)) return { error: contractCode(sim.error) ?? sim.error };
    return { value: scValToNative(sim.result.retval) };
  }

  async question(readSource, id) {
    const r = await this.read(readSource, "get_question", [u64(id)]);
    if (r.error === QUESTION_NOT_FOUND) return null;
    if (r.error != null) throw new Error(`get_question(${id}): ${r.error}`);
    return r.value;
  }

  async landed(hash) {
    if (!hash) return false;
    const r = await this.server.getTransaction(hash);
    return r.status === "SUCCESS";
  }

  async latest() {
    return (await this.server.getLatestLedger()).sequence;
  }
}

const deadlineOf = (q) => Number(q.deadline ?? BigInt(q.created_at) + BigInt(q.timeout_ledgers));

// ---------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------

async function fund(o, kp) {
  for (let i = 0; i < 5; i++) {
    const r = await fetch(`${o.friendbot}?addr=${kp.publicKey()}`);
    if (r.ok || r.status === 400) return;
    await sleep(1000);
  }
  throw new Error(`friendbot failed for ${kp.publicKey()}`);
}

async function lifecycle(o) {
  const { name: retryName, retry } = await loadRetry(o.retryModule);
  // --submit-profile / --keeper-profile swap the faults for the payer and
  // keeper phases, so a profile can target the backend's own calls alone.
  const proxy = createChaosProxy({ upstream: o.rpc, profile: loadProfile(o.submitProfile ?? o.profile), seed: o.seed });
  const port = await proxy.listen(o.port);
  const chaosUrl = `http://127.0.0.1:${port}`;
  const client = new Client(o, chaosUrl, retry);
  const truth = new Truth(o);
  const admin = Keypair.fromSecret(o.adminSecret);
  const readSource = admin.publicKey();
  console.error(`chaos proxy on ${chaosUrl} -> ${o.rpc}; retry: ${retryName}`);

  // Setup is not under test: fund directly.
  const payers = Array.from({ length: o.questions }, () => Keypair.random());
  const workers = Array.from({ length: o.workers }, () => Keypair.random());
  const keeper = Keypair.random();
  const all = [...payers, ...workers, keeper];
  for (let i = 0; i < all.length; i += 10) await Promise.all(all.slice(i, i + 10).map((k) => fund(o, k)));

  const idBase = BigInt(Math.floor(Math.random() * 2 ** 40)) << 8n;
  const qs = payers.map((payer, i) => ({
    id: idBase + BigInt(i),
    payer,
    plan: i / o.questions < o.refundShare ? "refund" : "resolve",
    steps: [],
  }));
  const step = (q, name, r) => q.steps.push({ at: Date.now(), step: name, ...r });

  // Phase 1 (concurrent, one source account per payer): submit, then the
  // backend's payment verification — the read-after-write round 6 hit.
  await Promise.all(
    qs.map(async (q) => {
      const r = await client.invoke("submit", q.payer, "submit", [addr(q.payer), u64(q.id), i128(o.amount)]);
      step(q, "submit", r);
      if (!r.ok) return;
      try {
        const got = await client.read("verifyPayment", readSource, "get_question", [u64(q.id)]);
        q.paymentVerified = statusOf(got) === "Pending" && BigInt(got.amount) === BigInt(o.amount);
        step(q, "verifyPayment", { ok: q.paymentVerified });
      } catch (e) {
        q.paymentVerified = false;
        step(q, "verifyPayment", { ok: false, error: String(e.message).slice(0, 80) });
      }
    }),
  );

  if (o.submitProfile) proxy.setProfile(loadProfile(o.profile));

  // Phase 2 (sequential: every resolve()/refund() has the admin as source).
  // The backend only dispatches verified payments. The fail-closed policy:
  // resolve() not confirmed → refund(); a QuestionNotPending answer means
  // something already settled it, so read the status instead of retrying.
  for (const q of qs.filter((q) => q.paymentVerified)) {
    const quorum = Array.from({ length: o.quorum }, (_, j) => workers[(Number(q.id % 1000n) + j) % workers.length]);
    let settled = false;
    if (q.plan === "resolve") {
      const r = await client.invoke("resolve", admin, "resolve", [u64(q.id), addrVec(quorum), addrVec([])]);
      step(q, "resolve", r);
      settled = r.ok;
      if (!r.ok && r.code === QUESTION_NOT_PENDING) settled = true;
    }
    if (!settled) {
      const r = await client.invoke(q.plan === "resolve" ? "failClosedRefund" : "refund", admin, "refund", [u64(q.id)]);
      step(q, q.plan === "resolve" ? "failClosedRefund" : "refund", r);
      settled = r.ok || r.code === QUESTION_NOT_PENDING;
    }
    if (!settled) step(q, "backendGaveUp", { ok: false });
  }

  // Phase 3: withdraw() behind a get_owed() balance read (round 6's third
  // symptom).
  const withdrawals = [];
  await Promise.all(
    workers.map(async (w) => {
      let owed;
      try {
        owed = BigInt(await client.read("withdrawBalance", readSource, "get_owed", [addr(w)]));
      } catch (e) {
        withdrawals.push({ worker: w.publicKey(), read: "failed", error: String(e.message).slice(0, 80) });
        return;
      }
      if (owed <= 0n) return withdrawals.push({ worker: w.publicKey(), owed: "0" });
      const r = await client.invoke("withdraw", w, "withdraw", [addr(w), i128(owed)]);
      withdrawals.push({ worker: w.publicKey(), owed: owed.toString(), ...r });
    }),
  );

  // Phase 4: keeper. Wait out the deadline (read directly), then call the
  // permissionless refund_timeout() through chaos until nothing is overdue.
  if (o.keeperProfile) proxy.setProfile(loadProfile(o.keeperProfile));
  const opened = [];
  for (const q of qs) {
    const got = await truth.question(readSource, q.id);
    q.opened = !!got;
    if (got) opened.push({ q, deadline: deadlineOf(got), pending: !TERMINAL.has(statusOf(got)) });
  }
  // Only questions the backend left Pending need the deadline to pass.
  const lastDeadline = Math.max(0, ...opened.filter((x) => x.pending).map((x) => x.deadline));
  let latest = await truth.latest();
  if (latest < lastDeadline) {
    console.error(`waiting for ledger ${lastDeadline} (now ${latest}) before the keeper runs...`);
    while (latest < lastDeadline) {
      await sleep(o.keeperPollMs);
      latest = await truth.latest();
    }
  }
  for (let poll = 0; poll < o.keeperPolls; poll++) {
    let pending = 0;
    for (const { q } of opened) {
      let st;
      try {
        st = statusOf(await client.read("keeperRead", readSource, "get_question", [u64(q.id)]));
      } catch {
        pending++; // can't tell through chaos: assume it still needs work
        continue;
      }
      if (TERMINAL.has(st)) continue;
      pending++;
      const r = await client.invoke("refundTimeout", keeper, "refund_timeout", [u64(q.id)]);
      step(q, "refundTimeout", r);
    }
    if (pending === 0) break;
    await sleep(o.keeperPollMs);
  }

  // Phase 5: verdict, from the chain directly.
  proxy.setEnabled(false);
  latest = await truth.latest();
  const unseen = [];
  for (const { q } of opened) {
    const got = await truth.question(readSource, q.id);
    q.final = statusOf(got);
    q.stuck = !TERMINAL.has(q.final);
    for (const s of q.steps) {
      if (!s.ok && s.hash && (await truth.landed(s.hash))) {
        s.landedUnseen = true;
        unseen.push({ question: q.id, step: s.step, error: s.error });
      }
    }
  }
  for (const q of qs.filter((q) => !q.opened)) q.final = "never-opened";
  for (const w of withdrawals) {
    if (!w.ok && w.hash && (await truth.landed(w.hash))) {
      w.landedUnseen = true;
      unseen.push({ worker: w.worker, step: "withdraw", error: w.error });
    }
  }

  const spec = checkRetrySpec(client.attempts, {
    attempts: o.attempts,
    baseDelayMs: o.baseDelayMs,
    timeoutMs: o.attemptTimeoutMs,
  });
  const stats = proxy.stats();
  await proxy.close();

  const count = (pred) => qs.filter(pred).length;
  const stuck = qs.filter((q) => q.stuck).map((q) => q.id);
  const report = {
    mode: "lifecycle",
    rpc: o.rpc,
    contract: o.contract,
    retry: retryName,
    params: { ...o, adminSecret: undefined },
    verdict: {
      fail_closed_holds: stuck.length === 0,
      stuck,
      retry_spec_holds: spec.violations.length === 0,
      retry_spec_violations: spec.violations,
    },
    outcomes: {
      questions: qs.length,
      never_opened: count((q) => q.final === "never-opened"),
      resolved: count((q) => q.final === "Resolved"),
      refunded: count((q) => q.final === "Refunded"),
      payment_unverified_but_opened: count((q) => q.opened && !q.paymentVerified),
      backend_gave_up: count((q) => q.steps.some((s) => s.step === "backendGaveUp")),
      settled_by_keeper: count((q) => q.steps.some((s) => s.step === "refundTimeout" && s.ok)),
      fail_closed_refunds: count((q) => q.steps.some((s) => s.step === "failClosedRefund" && s.ok)),
    },
    round6: round6Comparison(client.attempts, stats, unseen, qs, withdrawals),
    retry_calls: spec.calls,
    injected: stats,
    landed_unseen: unseen,
    questions: qs.map((q) => ({ id: q.id, plan: q.plan, final: q.final, steps: q.steps })),
    withdrawals,
  };
  if (o.out) writeFileSync(o.out, json(report));
  else console.log(json(report));
  printSummary(report);
  process.exit(report.verdict.fail_closed_holds && report.verdict.retry_spec_holds ? 0 : 1);
}

/// Round 6 saw read-after-write lag hit three call sites on testnet:
/// initialize(), payment verification and withdraw()'s balance check. For
/// each call site this run exercises, this reports how often the lag or a
/// fault hit it and how it recovered: on the retry, by escalation
/// (fail-closed refund / keeper), or not at all.
function round6Comparison(attempts, stats, unseen, qs, withdrawals) {
  const bySite = {};
  const calls = new Map();
  for (const a of attempts) {
    if (!calls.has(a.callId)) calls.set(a.callId, []);
    calls.get(a.callId).push(a);
  }
  for (const recs of calls.values()) {
    const site = recs[0].label;
    const s = (bySite[site] ??= { calls: 0, first_attempt_ok: 0, recovered_on_retry: 0, exhausted: 0 });
    s.calls++;
    if (recs[0].ok) s.first_attempt_ok++;
    else if (recs.some((r) => r.ok)) s.recovered_on_retry++;
    else s.exhausted++;
  }
  return {
    lag_injected: {
      getTransaction_not_found: stats.getTransaction?.lagged ?? 0,
      getLedgerEntries_stale: stats.getLedgerEntries?.lagged ?? 0,
      getLatestLedger_behind: stats.getLatestLedger?.lagged ?? 0,
    },
    call_sites: bySite,
    symptoms: {
      "initialize()": "not exercised: done once by the sandbox/deploy script, outside the chaos path",
      "payment verification": {
        verify_calls: bySite.verifyPayment?.calls ?? 0,
        unverified_payments_left_for_keeper: qs.filter((q) => q.opened && q.paymentVerified === false).length,
      },
      "withdraw() balance check": {
        balance_reads: bySite.withdrawBalance?.calls ?? 0,
        reads_failed: withdrawals.filter((w) => w.read === "failed").length,
        withdraws_landed_unseen: unseen.filter((u) => u.step === "withdraw").length,
      },
    },
  };
}

function printSummary(r) {
  const v = r.verdict;
  console.error(`\nfail-closed guarantee: ${v.fail_closed_holds ? "HOLDS" : `BROKEN: ${v.stuck.length} stuck (${v.stuck.join(", ")})`}`);
  console.error(`retry spec:            ${v.retry_spec_holds ? "HOLDS" : `${v.retry_spec_violations.length} violation(s)`}`);
  console.error(`outcomes:              ${JSON.stringify(r.outcomes)}`);
  console.error(`landed but unseen:     ${r.landed_unseen.length}`);
  console.error("\n| call site | calls | 1st attempt ok | recovered on retry | exhausted |");
  console.error("|---|---|---|---|---|");
  for (const [site, s] of Object.entries(r.round6.call_sites).sort()) {
    console.error(`| ${site} | ${s.calls} | ${s.first_attempt_ok} | ${s.recovered_on_retry} | ${s.exhausted} |`);
  }
}

// ---------------------------------------------------------------------------
// proxy
// ---------------------------------------------------------------------------

async function proxyMode(o) {
  const log = o.log ? (e) => appendFileSync(o.log, JSON.stringify(e) + "\n") : () => {};
  const p = createChaosProxy({ upstream: o.upstream, kind: o.kind, profile: loadProfile(o.profile), seed: o.seed, log });
  const port = await p.listen(o.port || 8010);
  console.error(`chaos proxy (${o.kind}) on http://127.0.0.1:${port} -> ${o.upstream}`);
  console.error(`stats: curl http://127.0.0.1:${port}/__chaos   off/on: curl -X POST .../__chaos/off|on`);
  process.on("SIGINT", async () => {
    console.log(json(p.stats()));
    await p.close();
    process.exit(0);
  });
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

async function pendingIds(truth, readSource) {
  const ids = [];
  for (let start = 0; ; start += 100) {
    const r = await truth.read(readSource, "list_pending", [nativeToScVal(start, { type: "u32" }), nativeToScVal(100, { type: "u32" })]);
    if (r.error != null) throw new Error(`list_pending: ${r.error}`);
    ids.push(...r.value.map(BigInt));
    if (r.value.length < 100) return ids;
  }
}

async function verify(o) {
  const truth = new Truth(o);
  const ids = new Set(await pendingIds(truth, o.readSource));
  if (o.ids) for (const line of readFileSync(o.ids, "utf8").split(/\s+/).filter(Boolean)) ids.add(BigInt(line));

  const latest = await truth.latest();
  const rows = [];
  for (const id of ids) {
    const q = await truth.question(o.readSource, id);
    if (!q) {
      rows.push({ id, status: "not-found" });
      continue;
    }
    const deadline = deadlineOf(q);
    const status = statusOf(q);
    rows.push({ id, status, deadline, overdue: !TERMINAL.has(status) && latest > deadline + o.graceLedgers });
  }
  const overdue = rows.filter((r) => r.overdue);

  const swept = [];
  if (o.sweep && overdue.length) {
    const keeper = Keypair.fromSecret(o.keeperSecret);
    const client = new Client(o, o.rpc, (fn) => fn(0)); // direct, no chaos, no retry
    for (const r of overdue) swept.push({ id: r.id, ...(await client.invoke("refundTimeout", keeper, "refund_timeout", [u64(r.id)])) });
  }
  const stillStuck = [];
  for (const r of overdue) {
    const q = await truth.question(o.readSource, r.id);
    if (!TERMINAL.has(statusOf(q))) stillStuck.push(r.id);
  }

  const report = {
    mode: "verify",
    contract: o.contract,
    latest_ledger: latest,
    checked: rows.length,
    overdue_found: overdue.map((r) => r.id),
    swept,
    still_stuck: stillStuck,
    fail_closed_holds: stillStuck.length === 0,
    rows,
  };
  if (o.out) writeFileSync(o.out, json(report));
  else console.log(json(report));
  console.error(
    `checked ${rows.length}; overdue ${overdue.length}; ${o.sweep ? `swept ${swept.filter((s) => s.ok).length}; ` : ""}still stuck ${stillStuck.length}`,
  );
  process.exit(stillStuck.length === 0 ? 0 : 1);
}

// ---------------------------------------------------------------------------

const o = parseArgs(process.argv.slice(2));
if (o.cmd === "lifecycle") await lifecycle(o);
else if (o.cmd === "proxy") await proxyMode(o);
else await verify(o);
