#!/usr/bin/env node
// Captures the real ledger state a refund_timeout() of one Pending question
// touches, from a live network, as a soroban-sdk LedgerSnapshot:
//   escrow instance + Wasm code, the Question entry (+ its pending-index
//   entries on v0.3), the token's instance and the escrow's token balance,
//   the payer's account + trustline and the asset issuer's account.
// Each entry keeps its REAL live_until ledger, so a test that loads the
// snapshot and advances the ledger archives exactly what the network would.
//
//   node tools/fork/make-fork-fixture.mjs --contract C... --question 1002 \
//     --asset AUSD:G... --source <key alias> --name v030 [--network testnet]
//
// Writes fixtures/testnet_fork_<name>.json (the snapshot) and
// fixtures/testnet_fork_<name>.rs (ids the test needs, include!()d by it).
// stellar-cli's own `snapshot create` would be the obvious tool, but the
// pinned CLI can't parse protocol-28 history buckets; RPC getLedgerEntries
// returns the same entries with their TTLs.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";

const a = {};
for (let i = 2; i < process.argv.length; i += 2) a[process.argv[i].replace(/^--/, "")] = process.argv[i + 1];
a.network ??= "testnet";
for (const k of ["contract", "question", "asset", "source", "name"]) if (!a[k]) throw new Error(`--${k} required`);
const PASSPHRASES = { testnet: "Test SDF Network ; September 2015", mainnet: "Public Global Stellar Network ; September 2015" };

function stellar(...args) {
  const r = spawnSync("stellar", args, { encoding: "utf8" });
  if (r.status !== 0) throw new Error(`stellar ${args.join(" ")}\n${r.stderr}`);
  return r.stdout.trim();
}
const scval = (json) => {
  const r = spawnSync("stellar", ["xdr", "encode", "--type", "ScVal"], { input: JSON.stringify(json), encoding: "utf8" });
  if (r.status !== 0) throw new Error(r.stderr);
  return r.stdout.trim();
};
const view = (fn, args = {}) =>
  JSON.parse(stellar("contract", "invoke", "--id", a.contract, "--source-account", a.source, "--network", a.network, "--send=no", "--", fn,
    ...Object.entries(args).flatMap(([k, v]) => [`--${k}`, String(v)])));

const entries = [];
let latest = 0;
function fetch(kind, ...args) {
  const r = spawnSync("stellar", ["ledger", "entry", "fetch", kind, ...args, "--network", a.network, "--output", "json"], { encoding: "utf8" });
  if (r.status !== 0) return false;
  const out = JSON.parse(r.stdout);
  latest = Math.max(latest, out.latestLedger);
  for (const e of out.entries ?? []) {
    entries.push([e.key, [{ last_modified_ledger_seq: e.lastModifiedLedgerSeq, data: e.val, ext: "v0" }, e.liveUntilLedgerSeq ?? null]]);
  }
  return (out.entries ?? []).length > 0;
}
const data = (contract, keyJson) => fetch("contract-data", "--contract", contract, "--durability", "persistent", "--key-xdr", scval(keyJson));

const [code, issuer] = a.asset.split(":");
const question = view("get_question", { question_id: a.question });
if (question.status !== "Pending") throw new Error(`question ${a.question} is ${question.status}, not Pending`);
// v0.2.0 has no get_token(); fall back to the instance storage entry.
fetch("contract-data", "--contract", a.contract, "--instance");
const instance = entries.at(-1)[1][0].data.contract_data.val.contract_instance;
const token = instance.storage.find((s) => s.key.vec?.[0]?.symbol === "Token").val.address;
fetch("contract-code", "--wasm-hash", instance.executable.wasm);

data(a.contract, { vec: [{ symbol: "Question" }, { u64: String(a.question) }] });
let indexed = false;
if (data(a.contract, { vec: [{ symbol: "PendingPos" }, { u64: String(a.question) }] })) {
  const pos = entries.at(-1)[1][0].data.contract_data.val.u32;
  data(a.contract, { vec: [{ symbol: "PendingAt" }, { u32: pos }] });
  data(a.contract, { vec: [{ symbol: "PendingCount" }] });
  // Settling swap-removes: the index TAIL moves into this question's slot,
  // so its entries are part of what a refund touches too.
  const count = entries.at(-1)[1][0].data.contract_data.val.u32;
  if (count - 1 !== pos) {
    data(a.contract, { vec: [{ symbol: "PendingAt" }, { u32: count - 1 }] });
    const tail = entries.at(-1)[1][0].data.contract_data.val.u64;
    data(a.contract, { vec: [{ symbol: "PendingPos" }, { u64: String(tail) }] });
  }
  indexed = true;
}
fetch("contract-data", "--contract", token, "--instance");
data(token, { vec: [{ symbol: "Balance" }, { address: a.contract }] });
fetch("account", "--account", question.payer);
fetch("trustline", "--account", question.payer, "--asset", `${code}:${issuer}`);
fetch("account", "--account", issuer);

const settings = JSON.parse(stellar("network", "settings", "--network", a.network, "--output", "json"));
const arch = settings.updated_entry.find((e) => e.state_archival).state_archival;
const snapshot = {
  // The embedded soroban-env-host speaks protocol 23 — the protocol that
  // introduced auto-restore; the network is on a later one with the same
  // archival semantics.
  protocol_version: 23,
  sequence_number: latest,
  timestamp: Math.floor(Date.now() / 1000),
  network_id: createHash("sha256").update(PASSPHRASES[a.network]).digest("hex"),
  base_reserve: 5000000,
  min_persistent_entry_ttl: arch.min_persistent_ttl,
  min_temp_entry_ttl: arch.min_temporary_ttl,
  max_entry_ttl: arch.max_entry_ttl,
  ledger_entries: entries,
};
const out = `fixtures/testnet_fork_${a.name}.json`;
writeFileSync(out, JSON.stringify(snapshot, null, 1) + "\n");
writeFileSync(`fixtures/testnet_fork_${a.name}.rs`, `// Generated by tools/fork/make-fork-fixture.mjs from ${a.network} at ledger ${latest}.
pub const CONTRACT: &str = "${a.contract}";
pub const TOKEN: &str = "${token}";
pub const QUESTION_ID: u64 = ${a.question};
pub const PAYER: &str = "${question.payer}";
pub const AMOUNT: i128 = ${question.amount};
pub const DEADLINE: u32 = ${Number(question.created_at) + Number(question.timeout_ledgers)};
pub const INDEXED: bool = ${indexed};
/// Highest live_until of any entry in the snapshot: one ledger past it,
/// every contract entry in the fork is archived.
pub const MAX_LIVE_UNTIL: u32 = ${Math.max(...entries.map(([, [, lu]]) => lu ?? 0))};
/// Entries with a TTL (contract data/code) that a refund must restore.
pub const ARCHIVABLE_ENTRIES: u32 = ${entries.filter(([, [, lu]]) => lu !== null).length};
`);
console.log(`wrote ${out}: ${entries.length} entries at ledger ${latest}`);
for (const [k, [e, lu]] of entries) {
  const what = k.contract_data ? `${k.contract_data.contract.slice(0, 6)}… ${JSON.stringify(k.contract_data.key)}` : Object.keys(k)[0];
  console.log(`  ${what.slice(0, 90).padEnd(92)} last_modified ${e.last_modified_ledger_seq}  live_until ${lu ?? "-"}${lu ? `  (ttl ${lu - e.last_modified_ledger_seq})` : ""}`);
}
