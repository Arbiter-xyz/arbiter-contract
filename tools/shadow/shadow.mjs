#!/usr/bin/env node
// Shadow-contract testing (issue #120; see docs/SHADOW_TESTING.md).
//
// Mirrors every successful state-changing call made against the PRODUCTION
// oracle-escrow contract onto a SHADOW deployment of a candidate lib.rs
// change, then diffs the two contracts' state. Production is only ever
// read: this tool holds no production key and sends nothing to it.
//
//   node tools/shadow/shadow.mjs identities --seed <hex> --addresses G...,G...
//   node tools/shadow/shadow.mjs mirror --prod C... --shadow C... --shadow-admin-secret S... \
//        --prod-admin G... --seed <hex> --from-ledger N [--follow] [--dry-run] \
//        [--token-map Cprod=Cshadow,...] [--state shadow-state.json]
//        [--shadow-token C... --faucet-secret S...]   (only for a non-native shadow token)
//   node tools/shadow/shadow.mjs diff --prod C... --shadow C... --shadow-admin-secret S... \
//        --prod-admin G... --seed <hex> [--expect expected.json] [--out report.json]
//
// Network flags (default testnet for both): --prod-rpc, --prod-passphrase,
// --shadow-rpc, --shadow-passphrase, --friendbot, --read-source G... (an
// existing account on the production network, used only as the simulation
// source for production reads).
//
// Identity mapping: every production G-address X is replayed as the shadow
// keypair derived from sha256(seed || X), so the tool can sign for every
// payer/worker without any of their keys. --prod-admin maps to the shadow
// admin. The production contract id maps to the shadow id, and tokens map
// through --token-map. Question ids are NOT remapped: the shadow is its own
// contract, so it has its own id space (QuestionAlreadyExists is per
// contract).

import {
  Account,
  Address,
  BASE_FEE,
  Keypair,
  Networks,
  Operation,
  TransactionBuilder,
  authorizeEntry,
  nativeToScVal,
  rpc,
  scValToNative,
  xdr,
} from "@stellar/stellar-sdk";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const [cmd, ...rest] = argv;
  const o = {
    cmd,
    prodRpc: "https://soroban-testnet.stellar.org",
    prodPassphrase: Networks.TESTNET,
    shadowRpc: "https://soroban-testnet.stellar.org",
    shadowPassphrase: Networks.TESTNET,
    friendbot: "https://friendbot.stellar.org",
    state: "shadow-state.json",
    pollMs: 5000,
    follow: false,
    dryRun: false,
  };
  for (let i = 0; i < rest.length; i++) {
    const k = rest[i].replace(/^--/, "").replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    if (k === "follow" || k === "dryRun") o[k] = true;
    else o[k] = rest[++i];
  }
  const need = {
    identities: ["seed", "addresses"],
    mirror: ["prod", "shadow", "shadowAdminSecret", "prodAdmin", "seed", "fromLedger"],
    diff: ["prod", "shadow", "shadowAdminSecret", "prodAdmin", "seed"],
  }[cmd];
  if (!need || need.some((k) => !o[k])) {
    console.error(readFileSync(new URL(import.meta.url)).toString().split("\n").slice(7, 22).join("\n"));
    process.exit(2);
  }
  o.fromLedger = Number(o.fromLedger ?? 0);
  o.tokenMap = Object.fromEntries((o.tokenMap ?? "").split(",").filter(Boolean).map((p) => p.split("=")));
  return o;
}

// ---------------------------------------------------------------------------
// Identity and value mapping
// ---------------------------------------------------------------------------

class Mapper {
  constructor(o) {
    this.o = o;
    this.admin = o.shadowAdminSecret ? Keypair.fromSecret(o.shadowAdminSecret) : null;
    this.byShadow = new Map(); // shadow G -> Keypair
    this.toProd = new Map(); // shadow address -> prod address
    if (this.admin) this.byShadow.set(this.admin.publicKey(), this.admin);
  }

  identity(prodG) {
    if (this.admin && prodG === this.o.prodAdmin) return this.admin;
    const seed = createHash("sha256").update(Buffer.from(this.o.seed, "hex")).update(prodG).digest();
    const kp = Keypair.fromRawEd25519Seed(seed);
    this.byShadow.set(kp.publicKey(), kp);
    this.toProd.set(kp.publicKey(), prodG);
    return kp;
  }

  address(prod) {
    if (prod === this.o.prod) return this.o.shadow;
    if (this.o.tokenMap[prod]) return this.o.tokenMap[prod];
    if (prod.startsWith("G")) return this.identity(prod).publicKey();
    return prod; // other contracts are passed through unchanged
  }

  unmap(shadow) {
    if (shadow === this.o.shadow) return this.o.prod;
    if (this.admin && shadow === this.admin.publicKey()) return this.o.prodAdmin;
    for (const [p, s] of Object.entries(this.o.tokenMap)) if (s === shadow) return p;
    return this.toProd.get(shadow) ?? shadow;
  }

  scVal(v) {
    switch (v.switch().name) {
      case "scvAddress": {
        const a = Address.fromScAddress(v.address()).toString();
        return new Address(this.address(a)).toScVal();
      }
      case "scvVec":
        return xdr.ScVal.scvVec((v.vec() ?? []).map((x) => this.scVal(x)));
      case "scvMap":
        return xdr.ScVal.scvMap(
          (v.map() ?? []).map((e) => new xdr.ScMapEntry({ key: this.scVal(e.key()), val: this.scVal(e.val()) })),
        );
      default:
        return v;
    }
  }

  /// Native JS value from a shadow read, with shadow addresses rewritten
  /// back to their production counterparts, so it can be compared directly.
  unmapNative(x) {
    if (typeof x === "string" && /^[GC][A-Z2-7]{55}$/.test(x)) return this.unmap(x);
    if (Array.isArray(x)) return x.map((y) => this.unmapNative(y));
    if (x && typeof x === "object" && !(x instanceof Uint8Array) && typeof x !== "bigint") {
      return Object.fromEntries(Object.entries(x).map(([k, v]) => [k, this.unmapNative(v)]));
    }
    return x;
  }
}

// ---------------------------------------------------------------------------
// RPC plumbing
// ---------------------------------------------------------------------------

function server(url) {
  return new rpc.Server(url, { allowHttp: url.startsWith("http://") });
}

function contractError(text) {
  const m = String(text).match(/Error\(Contract, #(\d+)\)/);
  return m ? Number(m[1]) : null;
}

async function simulateRead(srv, passphrase, sourceG, contract, fn, args) {
  const acct = await srv.getAccount(sourceG);
  const tx = new TransactionBuilder(acct, { fee: BASE_FEE, networkPassphrase: passphrase })
    .addOperation(Operation.invokeContractFunction({ contract, function: fn, args }))
    .setTimeout(60)
    .build();
  const sim = await srv.simulateTransaction(tx);
  if (rpc.Api.isSimulationError(sim)) return { error: contractError(sim.error) ?? sim.error };
  return { value: scValToNative(sim.result.retval), raw: sim.result.retval };
}

/// Invokes `fn` on the shadow with the shadow admin as source, signing every
/// address-credential auth entry with the mapped keypair. Returns
/// { ok, hash, error, returnValue, simulatedOnly }.
async function shadowInvoke(ctx, contract, fn, args, { dryRun = false } = {}) {
  const { srv, o, map } = ctx;
  const admin = map.admin;
  for (let attempt = 0; attempt < 5; attempt++) {
    const src = await srv.getAccount(admin.publicKey());
    const build = (op) =>
      new TransactionBuilder(new Account(src.accountId(), src.sequenceNumber()), {
        fee: BASE_FEE,
        networkPassphrase: o.shadowPassphrase,
      })
        .addOperation(op)
        .setTimeout(120)
        .build();

    let tx = build(Operation.invokeContractFunction({ contract, function: fn, args }));
    let sim = await srv.simulateTransaction(tx);
    if (rpc.Api.isSimulationError(sim)) return { ok: false, error: contractError(sim.error) ?? sim.error };
    if (dryRun) return { ok: true, simulatedOnly: true, returnValue: sim.result?.retval };

    const auth = sim.result?.auth ?? [];
    if (auth.some((e) => e.credentials().switch().name === "sorobanCredentialsAddress")) {
      const until = (await srv.getLatestLedger()).sequence + 200;
      const signed = await Promise.all(
        auth.map(async (e) => {
          if (e.credentials().switch().name !== "sorobanCredentialsAddress") return e;
          const who = Address.fromScAddress(e.credentials().address().address()).toString();
          const kp = map.byShadow.get(who);
          if (!kp) throw new Error(`no shadow key for auth address ${who}`);
          return authorizeEntry(e, kp, until, o.shadowPassphrase);
        }),
      );
      tx = build(Operation.invokeContractFunction({ contract, function: fn, args, auth: signed }));
      sim = await srv.simulateTransaction(tx);
      if (rpc.Api.isSimulationError(sim)) return { ok: false, error: contractError(sim.error) ?? sim.error };
    }

    const prepared = rpc.assembleTransaction(tx, sim).build();
    prepared.sign(admin);
    const sent = await srv.sendTransaction(prepared);
    if (sent.status === "TRY_AGAIN_LATER") {
      await sleep(2000 * (attempt + 1));
      continue;
    }
    if (sent.status !== "PENDING" && sent.status !== "DUPLICATE") {
      return { ok: false, error: `send:${sent.status}` };
    }
    for (;;) {
      const r = await srv.getTransaction(sent.hash);
      if (r.status === "SUCCESS") return { ok: true, hash: sent.hash, returnValue: r.returnValue };
      if (r.status === "FAILED") return { ok: false, hash: sent.hash, error: "tx-failed" };
      await sleep(1000);
    }
  }
  return { ok: false, error: "try-again-later" };
}

// ---------------------------------------------------------------------------
// Production traffic source: contract events -> transactions -> invocations
// ---------------------------------------------------------------------------

function invocationsOf(envelope, prodContract) {
  const txBody =
    envelope.switch().name === "envelopeTypeTxFeeBump"
      ? envelope.feeBump().tx().innerTx().v1().tx()
      : envelope.v1().tx();
  const out = [];
  for (const op of txBody.operations()) {
    if (op.body().switch().name !== "invokeHostFunction") continue;
    const hf = op.body().invokeHostFunctionOp().hostFunction();
    if (hf.switch().name !== "hostFunctionTypeInvokeContract") continue;
    const ic = hf.invokeContract();
    if (Address.fromScAddress(ic.contractAddress()).toString() !== prodContract) continue;
    out.push({ fn: ic.functionName().toString(), args: ic.args() });
  }
  return out;
}

async function* productionTxs(o, st) {
  const srv = server(o.prodRpc);
  const filters = [{ type: "contract", contractIds: [o.prod] }];
  for (;;) {
    const page = st.cursor
      ? await srv.getEvents({ filters, cursor: st.cursor, limit: 200 })
      : await srv.getEvents({ filters, startLedger: o.fromLedger, limit: 200 });
    const hashes = [];
    for (const e of page.events) if (!hashes.includes(e.txHash)) hashes.push(e.txHash);
    for (const h of hashes) {
      if (st.replayed[h]) continue;
      const tx = await srv.getTransaction(h);
      if (tx.status !== "SUCCESS") continue;
      yield { hash: h, ledger: tx.ledger, envelope: tx.envelopeXdr, returnValue: tx.returnValue };
    }
    st.cursor = page.cursor ?? st.cursor;
    if (page.events.length === 0) {
      if (!o.follow) return;
      await sleep(o.pollMs);
    }
  }
}

// ---------------------------------------------------------------------------
// State journal
// ---------------------------------------------------------------------------

function loadState(o) {
  if (existsSync(o.state)) return JSON.parse(readFileSync(o.state, "utf8"));
  return { cursor: null, replayed: {}, deferred: [], ids: [], addresses: [], divergences: [] };
}

function saveState(o, st) {
  writeFileSync(o.state, JSON.stringify(st, (_, v) => (typeof v === "bigint" ? v.toString() : v), 2));
}

function remember(st, fn, args) {
  const add = (arr, v) => {
    if (!arr.includes(v)) arr.push(v);
  };
  const walk = (v) => {
    const n = v.switch().name;
    if (n === "scvAddress") {
      const a = Address.fromScAddress(v.address()).toString();
      if (a.startsWith("G")) add(st.addresses, a);
    } else if (n === "scvVec") (v.vec() ?? []).forEach(walk);
  };
  args.forEach(walk);
  // Every entry point that names a question takes its id as the first u64.
  const firstU64 = args.find((a) => a.switch().name === "scvU64");
  if (firstU64 && /submit|charge|resolve|refund|reopen|touch_question|import_question/.test(fn)) {
    add(st.ids, firstU64.u64().toString());
    // Only questions whose opening call was mirrored have a comparable
    // history; ones opened before --from-ledger exist only in production.
    st.opened ??= [];
    if (/^(submit|charge)/.test(fn)) add(st.opened, firstU64.u64().toString());
  }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async function ensureFunded(ctx, st, prodArgs) {
  // Friendbot-create every mapped G-address the first time it appears.
  // With the native-XLM SAC as the shadow token (the recommended setup),
  // that is also the only funding they ever need on testnet.
  st.funded ??= [];
  for (const g of st.addresses) {
    const kp = ctx.map.identity(g);
    if (kp === ctx.map.admin || st.funded.includes(kp.publicKey())) continue;
    const r = await fetch(`${ctx.o.friendbot}?addr=${kp.publicKey()}`);
    if (r.ok || r.status === 400) st.funded.push(kp.publicKey());
  }
  // Optional: top up a non-native shadow token from a faucet account.
  if (ctx.o.faucetSecret && ctx.o.shadowToken) {
    const faucet = Keypair.fromSecret(ctx.o.faucetSecret);
    ctx.map.byShadow.set(faucet.publicKey(), faucet);
    const amount = prodArgs.find((a) => a.switch().name === "scvI128");
    const payer = prodArgs.find((a) => a.switch().name === "scvAddress");
    if (amount && payer) {
      await shadowInvoke(ctx, ctx.o.shadowToken, "transfer", [
        new Address(faucet.publicKey()).toScVal(),
        ctx.map.scVal(payer),
        amount,
      ]);
    }
  }
}

function sameReturn(ctx, prodRv, shadowRv) {
  if (!prodRv || !shadowRv) return true;
  const a = JSON.stringify(scValToNative(prodRv), (_, v) => (typeof v === "bigint" ? v.toString() : v));
  const b = JSON.stringify(ctx.map.unmapNative(scValToNative(shadowRv)), (_, v) =>
    typeof v === "bigint" ? v.toString() : v,
  );
  return a === b;
}

const TOO_EARLY_FOR_TIMEOUT = 8;

async function replayOne(ctx, st, item) {
  const { fn, args, hash, ledger, prodReturn } = item;
  const mapped = args.map((a) => ctx.map.scVal(a));
  const r = await shadowInvoke(ctx, ctx.o.shadow, fn, mapped, { dryRun: ctx.o.dryRun });
  if (!r.ok && fn === "refund_timeout" && r.error === TOO_EARLY_FOR_TIMEOUT) {
    // The shadow question was opened later than production's (replay lag),
    // so its deadline is later too. Not a divergence: retry later.
    return "deferred";
  }
  if (!r.ok) {
    st.divergences.push({ kind: "call", prod_tx: hash, ledger, fn, shadow_error: r.error });
  } else if (!r.simulatedOnly && !sameReturn(ctx, prodReturn, r.returnValue)) {
    st.divergences.push({
      kind: "return",
      prod_tx: hash,
      ledger,
      fn,
      prod: scValToNative(prodReturn),
      shadow: ctx.map.unmapNative(scValToNative(r.returnValue)),
    });
  }
  return r.ok ? "ok" : "diverged";
}

async function mirror(o) {
  const map = new Mapper(o);
  const ctx = { o, map, srv: server(o.shadowRpc) };
  const st = loadState(o);
  const counts = { ok: 0, diverged: 0, deferred: 0 };

  const retryDeferred = async () => {
    const still = [];
    for (const d of st.deferred) {
      const item = { ...d, args: d.args.map((b) => xdr.ScVal.fromXDR(b, "base64")) };
      const res = await replayOne(ctx, st, item);
      if (res === "deferred") still.push(d);
      else counts[res]++;
    }
    st.deferred = still;
  };

  for await (const tx of productionTxs(o, st)) {
    for (const inv of invocationsOf(tx.envelope, o.prod)) {
      remember(st, inv.fn, inv.args);
      await ensureFunded(ctx, st, inv.args);
      const item = { fn: inv.fn, args: inv.args, hash: tx.hash, ledger: tx.ledger, prodReturn: tx.returnValue };
      const res = await replayOne(ctx, st, item);
      if (res === "deferred") {
        st.deferred.push({ fn: inv.fn, hash: tx.hash, ledger: tx.ledger, args: inv.args.map((a) => a.toXDR("base64")) });
      }
      counts[res]++;
      console.error(`${tx.ledger} ${tx.hash.slice(0, 8)} ${inv.fn} -> ${res}`);
    }
    st.replayed[tx.hash] = true;
    await retryDeferred();
    saveState(o, st);
  }
  await retryDeferred();
  saveState(o, st);
  console.log(JSON.stringify({ ...counts, pending_deferred: st.deferred.length, divergences: st.divergences.length }));
}

/// Field-by-field state comparison. `timing` fields legitimately differ
/// because the shadow replays later than production (ledger numbers, and
/// the warming->settled split, which depends on elapsed ledgers).
const QUESTION_TIMING_FIELDS = new Set(["created_at"]);
const STAKE_TIMING_FIELDS = new Set(["warming_since", "unbonding_release_at", "settled", "warming"]);

function compareObjects(kind, key, prod, shadow, timing, out) {
  const norm = (v) => JSON.stringify(v, (_, x) => (typeof x === "bigint" ? x.toString() : x));
  if (prod?.error || shadow?.error) {
    if (norm(prod?.error) !== norm(shadow?.error)) out.push({ kind, key, field: "error", prod: prod?.error, shadow: shadow?.error, class: "unexpected" });
    return;
  }
  const p = prod.value;
  const s = shadow.value;
  if (typeof p !== "object" || p === null) {
    if (norm(p) !== norm(s)) out.push({ kind, key, field: "value", prod: p, shadow: s, class: "unexpected" });
    return;
  }
  for (const f of new Set([...Object.keys(p), ...Object.keys(s ?? {})])) {
    if (norm(p[f]) !== norm(s?.[f])) {
      out.push({ kind, key, field: f, prod: p[f], shadow: s?.[f], class: timing.has(f) ? "timing" : "unexpected" });
    }
  }
}

async function diff(o) {
  const map = new Mapper(o);
  const st = loadState(o);
  const prodSrv = server(o.prodRpc);
  const shadowSrv = server(o.shadowRpc);
  const prodSrc = o.readSource ?? o.prodAdmin;
  const shadowSrc = map.admin.publicKey();
  const out = [];

  const both = async (fn, prodArgs, shadowArgs) => [
    await simulateRead(prodSrv, o.prodPassphrase, prodSrc, o.prod, fn, prodArgs),
    await (async () => {
      const r = await simulateRead(shadowSrv, o.shadowPassphrase, shadowSrc, o.shadow, fn, shadowArgs);
      return r.value !== undefined ? { ...r, value: map.unmapNative(r.value) } : r;
    })(),
  ];

  const opened = new Set(st.opened ?? []);
  const skipped = st.ids.filter((id) => !opened.has(id));
  for (const id of st.ids.filter((id) => opened.has(id))) {
    const a = [nativeToScVal(BigInt(id), { type: "u64" })];
    const [p, s] = await both("get_question", a, a);
    compareObjects("question", id, p, s, QUESTION_TIMING_FIELDS, out);
  }
  for (const g of st.addresses) {
    if (g === o.prodAdmin) continue;
    const pa = [new Address(g).toScVal()];
    const sa = [new Address(map.address(g)).toScVal()];
    for (const fn of ["get_owed", "get_balance", "get_stake_info"]) {
      const [p, s] = await both(fn, pa, sa);
      compareObjects(fn, g, p, s, fn === "get_stake_info" ? STAKE_TIMING_FIELDS : new Set(), out);
    }
    // Total slashable stake must match exactly even when the bucket split
    // is timing-dependent.
    const [p, s] = await both("get_stake_info", pa, sa);
    if (p.value && s.value) {
      const total = (x) => BigInt(x.settled) + BigInt(x.warming) + BigInt(x.unbonding);
      if (total(p.value) !== total(s.value)) {
        out.push({ kind: "stake_total", key: g, prod: total(p.value).toString(), shadow: total(s.value).toString(), class: "unexpected" });
      }
    }
  }
  const [pc, sc] = await both("pending_count", [], []);
  compareObjects("pending_count", "-", pc, sc, new Set(), out);

  // Expected divergences: the intended behaviour change of the candidate.
  const expect = o.expect ? JSON.parse(readFileSync(o.expect, "utf8")) : [];
  const all = [...out, ...st.divergences.map((x) => ({ ...x, class: "unexpected" }))];
  for (const d of all) {
    const rule = expect.find(
      (r) => (!r.kind || r.kind === d.kind) && (!r.field || r.field === d.field) && (!r.fn || r.fn === d.fn),
    );
    if (rule && d.class === "unexpected") {
      d.class = "expected";
      d.reason = rule.reason;
    }
  }
  const by = (c) => all.filter((d) => d.class === c);
  const report = {
    compared: { questions: st.ids.length - skipped.length, skipped_opened_before_mirror: skipped.length, addresses: st.addresses.length, replayed_txs: Object.keys(st.replayed).length },
    unexpected: by("unexpected"),
    expected: by("expected"),
    timing: by("timing").length,
  };
  const json = JSON.stringify(report, (_, v) => (typeof v === "bigint" ? v.toString() : v), 2);
  if (o.out) writeFileSync(o.out, json);
  console.log(json);
  console.error(
    `\n${report.compared.questions} questions (${skipped.length} skipped: opened before the mirror), ${report.compared.addresses} addresses: ` +
      `${report.unexpected.length} unexpected, ${report.expected.length} expected, ${report.timing} timing-only divergences`,
  );
  process.exit(report.unexpected.length ? 1 : 0);
}

function identities(o) {
  const map = new Mapper(o);
  for (const g of o.addresses.split(",")) console.log(`${g} -> ${map.identity(g).publicKey()}`);
}

const o = parseArgs(process.argv.slice(2));
if (o.cmd === "identities") identities(o);
else if (o.cmd === "mirror") await mirror(o);
else await diff(o);
