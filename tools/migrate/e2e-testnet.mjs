#!/usr/bin/env node
// End-to-end migration test on Stellar testnet with real transactions:
// deploys two instances of the current contract on a freshly issued asset,
// opens Pending questions on the old one, then exercises the orchestrator —
// dry run, a run killed with SIGKILL right after a batch lands, verify, a
// resumed run, verify again, and a replayed batch — checking balances and
// question state on-chain after every step.
//
//   stellar contract build
//   node tools/migrate/e2e-testnet.mjs            # ~5 minutes, needs network
//
// Every key is freshly generated and friendbot-funded under a run-specific
// prefix, so runs never interfere with each other or with real deployments.

import { spawnSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const NET = "testnet";
const WASM = "target/wasm32v1-none/release/oracle_escrow.wasm";
const P = `arbe2e${Date.now().toString(36)}`;
const journal = join(mkdtempSync(join(tmpdir(), "arbiter-migrate-")), "journal.jsonl");

function sh(args, { allowFail = false, env = {} } = {}) {
  const r = spawnSync(args[0], args.slice(1), { encoding: "utf8", env: { ...process.env, ...env } });
  if (r.status !== 0 && !allowFail) throw new Error(`${args.join(" ")}\n${r.stderr}\n${r.stdout}`);
  return { ok: r.status === 0, out: r.stdout.trim(), err: r.stderr, status: r.status, signal: r.signal };
}
const stellar = (...a) => sh(["stellar", ...a]).out;
const addr = (k) => stellar("keys", "address", k);
const invoke = (id, src, fn, args = {}, send = "yes") =>
  stellar("contract", "invoke", "--id", id, "--source-account", src, "--network", NET, `--send=${send}`, "--", fn,
    ...Object.entries(args).flatMap(([k, v]) => [`--${k}`, typeof v === "string" ? v : JSON.stringify(v)]));
const view = (id, fn, args) => JSON.parse(invoke(id, `${P}admin`, fn, args, "no"));
const step = (s) => console.log(`\n=== ${s}`);
function assert(cond, msg) {
  if (!cond) throw new Error(`ASSERTION FAILED: ${msg}`);
  console.log(`  ✓ ${msg}`);
}

step(`keys (prefix ${P})`);
const who = ["admin", "issuer", "platform", "payer1", "payer2", "payer3"];
for (const k of who) {
  stellar("keys", "generate", `${P}${k}`, "--network", NET, "--fund");
  // Friendbot occasionally reports success without funding: check, retry.
  for (let i = 0; !sh(["stellar", "ledger", "entry", "fetch", "account", "--account", `${P}${k}`, "--network", NET], { allowFail: true }).out.includes('"account"'); i++) {
    if (i >= 5) throw new Error(`could not fund ${P}${k}`);
    sh(["stellar", "keys", "fund", `${P}${k}`, "--network", NET], { allowFail: true });
  }
}
const A = Object.fromEntries(who.map((k) => [k, addr(`${P}${k}`)]));
console.log(A);

step("asset: AUSD issued by a fresh issuer, wrapped in its Stellar Asset Contract");
const asset = `AUSD:${A.issuer}`;
const token = stellar("contract", "asset", "deploy", "--asset", asset, "--source-account", `${P}issuer`, "--network", NET);
for (const k of ["platform", "payer1", "payer2", "payer3"])
  stellar("tx", "new", "change-trust", "--source-account", `${P}${k}`, "--line", asset, "--network", NET);
for (const k of ["payer1", "payer2", "payer3"]) invoke(token, `${P}issuer`, "mint", { to: A[k], amount: "1000000000" });
console.log({ token });

step("deploy old + new escrow instances (same Wasm)");
const hash = stellar("contract", "upload", "--wasm", WASM, "--source-account", `${P}admin`, "--network", NET);
const OLD = stellar("contract", "deploy", "--wasm-hash", hash, "--source-account", `${P}admin`, "--network", NET);
const NEW = stellar("contract", "deploy", "--wasm-hash", hash, "--source-account", `${P}admin`, "--network", NET);
for (const c of [OLD, NEW])
  invoke(c, `${P}admin`, "initialize", { admin: A.admin, token, platform: A.platform, timeout_ledgers: 17280 });
console.log({ wasm_hash: hash, OLD, NEW });

step("open questions on OLD: 10 submitted, then 1 resolved and 1 refunded -> 8 pending");
const amounts = [2500000, 5000000, 7500000, 10000000, 2500000, 12500000, 2500000, 5000000, 20000000, 2500000];
amounts.forEach((amount, i) => {
  const payer = `payer${(i % 3) + 1}`;
  invoke(OLD, `${P}${payer}`, "submit", { payer: A[payer], question_id: String(1001 + i), amount: String(amount) });
});
const worker = A.platform; // any address; resolve() only credits it
invoke(OLD, `${P}admin`, "resolve", { question_id: "1003", workers: [worker], losing_workers: [] });
invoke(OLD, `${P}admin`, "refund", { question_id: "1007" });
const bal = (c) => BigInt(view(token, "balance", { id: c }));
const pending = (c) => Number(view(c, "pending_count"));
const pendingSum = amounts.reduce((s, a, i) => (i === 2 || i === 6 ? s : s + BigInt(a)), 0n);
assert(pending(OLD) === 8, "OLD has 8 pending");
const owedOnOld = BigInt(view(OLD, "get_owed", { worker }));
assert(bal(OLD) === pendingSum + owedOnOld, `OLD holds pending escrow + owed (${pendingSum} + ${owedOnOld})`);
const oldStart = bal(OLD);
const newStart = bal(NEW);

const mig = (cmd, env = {}, extra = []) =>
  sh(["node", "tools/migrate/migrate.mjs", cmd, "--source", OLD, "--target", NEW, "--admin", `${P}admin`, "--network", NET, "--batch", "3", "--journal", journal, ...extra], { allowFail: true, env });

step("plan before the target has named its migration source: must report the problem and exit non-zero");
let r = mig("plan");
console.log(r.out);
assert(r.status === 1 && /migration source is unset/.test(r.out), "dry run flags the missing handshake");

step("target admin: set_migration_source(OLD)");
invoke(NEW, `${P}admin`, "set_migration_source", { source: OLD });

step("dry run");
r = mig("plan");
console.log(r.out);
assert(r.status === 0, "dry run is clean");
assert(/8 pending question\(s\), 6\.0000000 total, in 3 batch\(es\)/.test(r.out), "dry run reports exactly 8 questions / 6.0 AUSD / 3 batches");
assert(bal(OLD) === oldStart && bal(NEW) === newStart && pending(OLD) === 8, "dry run moved nothing");

step("run, killed with SIGKILL right after batch 2 lands on-chain (before it is journaled)");
r = mig("run", { MIGRATE_CRASH: "after-send:2" });
console.log(r.out);
assert(r.signal === "SIGKILL", "orchestrator died mid-migration");
assert(pending(OLD) === 2 && pending(NEW) === 6, "exactly two whole batches moved: 2 left on OLD, 6 on NEW");
assert(bal(OLD) + bal(NEW) === oldStart + newStart, "no funds created or destroyed across both contracts");

step("verify the half-finished state");
r = mig("verify");
console.log(r.out);
assert(r.status === 0, "every question is in exactly one place, balances match");

step("replay: send batch 1 again by hand, as a retried in-flight tx would");
const firstBatch = JSON.parse(`[${sh(["sh", "-c", `grep '"batch-done","n":1' ${journal} || grep '"batch-intent","n":1' ${journal}`]).out.split("\n")[0]}]`)[0].ids;
const replay = sh(["stellar", "contract", "invoke", "--id", OLD, "--source-account", `${P}admin`, "--network", NET, "--send=yes", "--",
  "migrate_pending", "--question_ids", JSON.stringify(firstBatch.map(Number)), "--target", NEW], { allowFail: true });
assert(!replay.ok && /Error\(Contract, #6\)/.test(replay.err + replay.out), `replayed batch [${firstBatch}] reverts with QuestionNotPending`);
assert(pending(OLD) === 2 && pending(NEW) === 6 && bal(OLD) + bal(NEW) === oldStart + newStart, "replay changed nothing");

step("resume");
r = mig("run");
console.log(r.out);
assert(r.status === 0 && pending(OLD) === 0 && pending(NEW) === 8, "resumed run finished the job");

step("verify the finished migration");
r = mig("verify");
console.log(r.out);
assert(r.status === 0, "all checks pass");
assert(bal(NEW) - newStart === pendingSum, `NEW gained exactly the pending escrow (${pendingSum})`);
assert(bal(OLD) === owedOnOld, "OLD keeps only what it still owes workers");

step("a migrated question settles normally on NEW");
const p1Before = BigInt(view(token, "balance", { id: A.payer1 }));
invoke(NEW, `${P}admin`, "refund", { question_id: "1001" });
assert(BigInt(view(token, "balance", { id: A.payer1 })) - p1Before === 2500000n, "refund on NEW pays the original payer");
invoke(NEW, `${P}admin`, "clear_migration_source");

console.log(`\nE2E PASSED\n${JSON.stringify({ network: NET, token, OLD, NEW, wasm_hash: hash, journal }, null, 2)}`);
