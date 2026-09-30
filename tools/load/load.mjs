#!/usr/bin/env node
// Load harness for oracle-escrow against a real (local sandbox) network,
// plus the backend's HTTP surface. Issue #119; see docs/LOAD_TESTING.md.
//
//   npm --prefix tools/load install
//   tools/load/sandbox.sh                      # local quickstart + deployed, initialized contract; prints env
//   node tools/load/load.mjs contract --contract C... --admin-secret S... \
//        [--questions 500] [--payers 20] [--channels 0] [--workers 25] [--quorum 5] \
//        [--amount 2500000] [--collide-every 0] [--phases submit,resolve,withdraw] [--out result.json]
//   node tools/load/load.mjs backend --url http://localhost:3000/oracle --body-template body.json \
//        [--requests 500] [--concurrency 20] [--sse-url http://localhost:3000/events] [--sse-clients 5]
//
// Concurrency model: a Stellar source account can only have one
// transaction in flight (its sequence number), so parallelism here is
// "number of distinct source accounts", exactly as it would be for real
// callers. Each payer / channel / worker runs its own queue sequentially;
// the queues run concurrently.
//
//   --channels 0   resolve() is sent with the admin as the transaction source,
//                  so every resolve() queues behind the admin's sequence number.
//   --channels N   resolve() is sent from N funded channel accounts, with the
//                  admin authorizing through a signed SorobanAuthorizationEntry
//                  (address credentials). This measures the fix for the
//                  single-admin-source bottleneck, if there is one.

import {
  Account,
  Address,
  BASE_FEE,
  Contract,
  Keypair,
  Operation,
  TransactionBuilder,
  authorizeEntry,
  nativeToScVal,
  rpc,
  scValToNative,
  xdr,
} from "@stellar/stellar-sdk";
import { writeFileSync, readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

const LOCAL_PASSPHRASE = "Standalone Network ; February 2017";

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const [mode, ...rest] = argv;
  const o = {
    mode,
    rpc: "http://localhost:8000/rpc",
    friendbot: "http://localhost:8000/friendbot",
    passphrase: LOCAL_PASSPHRASE,
    questions: 500,
    payers: 20,
    channels: 0,
    workers: 25,
    quorum: 5,
    amount: 2_500_000,
    collideEvery: 0,
    phases: "submit,resolve,withdraw",
    requests: 500,
    concurrency: 20,
    sseClients: 0,
    pollMs: 500,
    txTimeoutS: 120,
  };
  for (let i = 0; i < rest.length; i++) {
    const k = rest[i].replace(/^--/, "").replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    const v = rest[++i];
    o[k] = typeof o[k] === "number" ? Number(v) : v;
  }
  if (!["contract", "backend"].includes(mode)) usage();
  if (mode === "contract" && (!o.contract || !o.adminSecret)) usage();
  if (mode === "backend" && (!o.url || !o.bodyTemplate)) usage();
  if (o.quorum > o.workers) throw new Error("--quorum must be <= --workers");
  return o;
}

function usage() {
  console.error(readFileSync(new URL(import.meta.url)).toString().split("\n").slice(3, 11).join("\n"));
  process.exit(2);
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

class PhaseStats {
  constructor(name) {
    this.name = name;
    this.ok = 0;
    this.errors = {};
    this.latencies = [];
    this.ledgers = new Map(); // ledger -> tx count included
    this.tryAgainLater = 0;
    this.start = 0;
    this.end = 0;
  }
  fail(kind) {
    this.errors[kind] = (this.errors[kind] ?? 0) + 1;
  }
  success(latencyMs, ledger) {
    this.ok++;
    this.latencies.push(latencyMs);
    if (ledger != null) this.ledgers.set(ledger, (this.ledgers.get(ledger) ?? 0) + 1);
  }
  summary() {
    const s = [...this.latencies].sort((a, b) => a - b);
    const q = (p) => (s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : null);
    const secs = (this.end - this.start) / 1000;
    const ls = [...this.ledgers.keys()];
    const span = ls.length ? Math.max(...ls) - Math.min(...ls) + 1 : 0;
    return {
      phase: this.name,
      ok: this.ok,
      errors: this.errors,
      try_again_later: this.tryAgainLater,
      wall_s: +secs.toFixed(2),
      ok_per_s: +(this.ok / Math.max(secs, 1e-9)).toFixed(3),
      latency_ms: { p50: q(0.5), p95: q(0.95), p99: q(0.99), max: s.at(-1) ?? null },
      ledgers_spanned: span,
      ok_per_ledger: span ? +(this.ok / span).toFixed(2) : null,
      max_in_one_ledger: ls.length ? Math.max(...this.ledgers.values()) : null,
    };
  }
}

function printTable(rows) {
  console.log("\n| phase | ok | errors | TRY_AGAIN_LATER | wall s | ok/s | ok/ledger | max/ledger | p50 ms | p99 ms |");
  console.log("|---|---|---|---|---|---|---|---|---|---|");
  for (const r of rows) {
    const errs = Object.entries(r.errors).map(([k, v]) => `${k}:${v}`).join(" ") || "0";
    console.log(
      `| ${r.phase} | ${r.ok} | ${errs} | ${r.try_again_later} | ${r.wall_s} | ${r.ok_per_s} | ${r.ok_per_ledger} | ${r.max_in_one_ledger} | ${r.latency_ms.p50?.toFixed(0)} | ${r.latency_ms.p99?.toFixed(0)} |`,
    );
  }
}

// ---------------------------------------------------------------------------
// Transaction plumbing
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function contractErrorOf(text) {
  const m = String(text).match(/Error\(Contract, #(\d+)\)/);
  return m ? `contract#${m[1]}` : null;
}

class Chain {
  constructor(o) {
    this.o = o;
    this.server = new rpc.Server(o.rpc, { allowHttp: o.rpc.startsWith("http://") });
    this.contract = new Contract(o.contract);
    this.seq = new Map(); // G... -> BigInt current sequence
    this.latest = { ledger: 0, at: 0 };
  }

  async fund(kp) {
    const r = await fetch(`${this.o.friendbot}?addr=${kp.publicKey()}`);
    if (!r.ok && r.status !== 400) throw new Error(`friendbot ${r.status}: ${await r.text()}`);
  }

  async latestLedger() {
    if (Date.now() - this.latest.at > 2000) {
      this.latest = { ledger: (await this.server.getLatestLedger()).sequence, at: Date.now() };
    }
    return this.latest.ledger;
  }

  async account(kp) {
    const pk = kp.publicKey();
    if (!this.seq.has(pk)) {
      const a = await this.server.getAccount(pk);
      this.seq.set(pk, BigInt(a.sequenceNumber()));
    }
    return new Account(pk, this.seq.get(pk).toString());
  }

  build(account, op) {
    return new TransactionBuilder(account, { fee: BASE_FEE, networkPassphrase: this.o.passphrase })
      .addOperation(op)
      .setTimeout(this.o.txTimeoutS)
      .build();
  }

  /// Read-only call via simulation.
  async read(kp, method, args) {
    const tx = this.build(await this.account(kp), this.contract.call(method, ...args));
    const sim = await this.server.simulateTransaction(tx);
    if (rpc.Api.isSimulationError(sim)) throw new Error(sim.error);
    return scValToNative(sim.result.retval);
  }

  /// Simulate, optionally sign auth entries for `authSigner`, assemble,
  /// send, and wait for inclusion. Records into `stats`. Never throws.
  async invoke(stats, sourceKp, method, args, { authSigner } = {}) {
    const t0 = performance.now();
    const pk = sourceKp.publicKey();
    for (let attempt = 0; attempt < 20; attempt++) {
      try {
        let op = this.contract.call(method, ...args);
        let tx = this.build(await this.account(sourceKp), op);
        let sim = await this.server.simulateTransaction(tx);
        if (rpc.Api.isSimulationError(sim)) {
          stats.fail(contractErrorOf(sim.error) ?? "simulation");
          return;
        }
        if (authSigner) {
          const until = (await this.latestLedger()) + 200;
          const signed = await Promise.all(
            (sim.result.auth ?? []).map((e) => authorizeEntry(e, authSigner, until, this.o.passphrase)),
          );
          const decoded = tx.operations[0];
          op = Operation.invokeHostFunction({ func: decoded.func, auth: signed });
          tx = this.build(await this.account(sourceKp), op);
          sim = await this.server.simulateTransaction(tx);
          if (rpc.Api.isSimulationError(sim)) {
            stats.fail(contractErrorOf(sim.error) ?? "simulation-signed");
            return;
          }
        }
        const prepared = rpc.assembleTransaction(tx, sim).build();
        prepared.sign(sourceKp);
        const sent = await this.server.sendTransaction(prepared);
        if (sent.status === "TRY_AGAIN_LATER") {
          // Queue said no: sequence number not consumed.
          stats.tryAgainLater++;
          await sleep(250 * (attempt + 1));
          continue;
        }
        if (sent.status !== "PENDING" && sent.status !== "DUPLICATE") {
          this.seq.delete(pk); // resync on the next attempt
          stats.fail(`send:${sent.status}`);
          return;
        }
        // From here the sequence number is consumed whether it succeeds or fails.
        this.seq.set(pk, this.seq.get(pk) + 1n);
        const res = await this.wait(sent.hash);
        if (res.status === "SUCCESS") stats.success(performance.now() - t0, res.ledger);
        else stats.fail(res.status === "FAILED" ? contractErrorOf(res.resultXdr?.toXDR?.("base64")) ?? "tx-failed" : res.status);
        return;
      } catch (e) {
        const msg = String(e?.message ?? e);
        if (/tx_bad_seq|txBadSeq/i.test(msg)) {
          this.seq.delete(pk);
          continue;
        }
        stats.fail(contractErrorOf(msg) ?? `exception:${msg.slice(0, 40)}`);
        return;
      }
    }
    stats.fail("gave-up");
  }

  async wait(hash) {
    const deadline = Date.now() + this.o.txTimeoutS * 1000;
    while (Date.now() < deadline) {
      const r = await this.server.getTransaction(hash);
      if (r.status !== "NOT_FOUND") return r;
      await sleep(this.o.pollMs);
    }
    return { status: "TIMEOUT" };
  }
}

/// Runs `queues` (arrays of thunks) concurrently, each queue sequentially.
async function runQueues(stats, queues) {
  stats.start = performance.now();
  await Promise.all(
    queues.map(async (q) => {
      for (const job of q) await job();
    }),
  );
  stats.end = performance.now();
}

const u64 = (n) => nativeToScVal(BigInt(n), { type: "u64" });
const i128 = (n) => nativeToScVal(BigInt(n), { type: "i128" });
const addr = (kp) => new Address(kp.publicKey()).toScVal();
const addrVec = (kps) => xdr.ScVal.scvVec(kps.map(addr));

// ---------------------------------------------------------------------------
// contract mode
// ---------------------------------------------------------------------------

async function contractMode(o) {
  const chain = new Chain(o);
  const admin = Keypair.fromSecret(o.adminSecret);
  const payers = Array.from({ length: o.payers }, () => Keypair.random());
  const workers = Array.from({ length: o.workers }, () => Keypair.random());
  const channels = Array.from({ length: o.channels }, () => Keypair.random());
  const phases = new Set(o.phases.split(","));

  console.error(`funding ${payers.length + workers.length + channels.length} accounts via friendbot...`);
  const all = [...payers, ...workers, ...channels];
  for (let i = 0; i < all.length; i += 10) await Promise.all(all.slice(i, i + 10).map((k) => chain.fund(k)));

  // Random 48-bit base so re-runs against the same contract never collide
  // unless --collide-every asks for it.
  const idBase = BigInt(Math.floor(Math.random() * 2 ** 48)) << 8n;
  const ids = [];
  for (let i = 0; i < o.questions; i++) {
    const collide = o.collideEvery > 0 && i > 0 && i % o.collideEvery === 0;
    ids.push(idBase + BigInt(collide ? i - 1 : i));
  }

  const results = [];
  const submitted = [];

  if (phases.has("submit")) {
    const s = new PhaseStats(`submit x${o.payers} payers`);
    const queues = payers.map(() => []);
    ids.forEach((id, i) => {
      const payer = payers[i % payers.length];
      queues[i % payers.length].push(async () => {
        const before = s.ok;
        await chain.invoke(s, payer, "submit", [addr(payer), u64(id), i128(o.amount)]);
        if (s.ok > before) submitted.push({ id, i });
      });
    });
    await runQueues(s, queues);
    results.push(s.summary());
  }

  if (phases.has("resolve")) {
    const label = o.channels > 0 ? `resolve x${o.channels} channels` : "resolve (admin source)";
    const s = new PhaseStats(label);
    const sources = o.channels > 0 ? channels : [admin];
    const queues = sources.map(() => []);
    submitted.forEach(({ id, i }, k) => {
      const quorum = Array.from({ length: o.quorum }, (_, j) => workers[(i * o.quorum + j) % workers.length]);
      const src = sources[k % sources.length];
      queues[k % sources.length].push(() =>
        chain.invoke(s, src, "resolve", [u64(id), addrVec(quorum), addrVec([])], {
          authSigner: o.channels > 0 ? admin : undefined,
        }),
      );
    });
    await runQueues(s, queues);
    results.push(s.summary());
  }

  if (phases.has("withdraw")) {
    const s = new PhaseStats(`withdraw x${o.workers} workers`);
    const queues = workers.map((w) => [
      async () => {
        let owed = 0n;
        try {
          owed = BigInt(await chain.read(w, "get_owed", [addr(w)]));
        } catch {
          s.fail("read");
          return;
        }
        if (owed > 0n) await chain.invoke(s, w, "withdraw", [addr(w), i128(owed)]);
      },
    ]);
    await runQueues(s, queues);
    results.push(s.summary());
  }

  printTable(results);
  const out = { mode: "contract", rpc: o.rpc, contract: o.contract, params: { ...o, adminSecret: undefined }, results };
  if (o.out) writeFileSync(o.out, JSON.stringify(out, null, 2));
  else console.log(JSON.stringify(out, null, 2));
}

// ---------------------------------------------------------------------------
// backend mode
// ---------------------------------------------------------------------------

async function sseClient(url, counter, signal) {
  try {
    const r = await fetch(url, { headers: { accept: "text/event-stream" }, signal });
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return;
      buf += dec.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf("\n\n")) >= 0) {
        buf = buf.slice(idx + 2);
        counter.events++;
      }
    }
  } catch (e) {
    if (e.name !== "AbortError") counter.errors++;
  }
}

async function backendMode(o) {
  // The request body is a template taken from arbiter-backend's POST /oracle
  // contract; "{{n}}" is replaced with the request number so each request is
  // a distinct question. This harness doesn't hard-code the schema: the
  // backend owns it.
  const template = readFileSync(o.bodyTemplate, "utf8");
  const s = new PhaseStats(`POST ${new URL(o.url).pathname} x${o.concurrency}`);
  const sse = { events: 0, errors: 0 };
  const abort = new AbortController();
  const sseRuns = o.sseUrl ? Array.from({ length: o.sseClients || 1 }, () => sseClient(o.sseUrl, sse, abort.signal)) : [];

  const queues = Array.from({ length: o.concurrency }, () => []);
  for (let n = 0; n < o.requests; n++) {
    queues[n % o.concurrency].push(async () => {
      const t0 = performance.now();
      try {
        const r = await fetch(o.url, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: template.replaceAll("{{n}}", String(n)),
        });
        await r.arrayBuffer();
        if (r.ok) s.success(performance.now() - t0, null);
        else s.fail(`http${r.status}`);
      } catch (e) {
        s.fail(`net:${e.cause?.code ?? e.name}`);
      }
    });
  }
  await runQueues(s, queues);
  abort.abort();
  await Promise.allSettled(sseRuns);

  const summary = s.summary();
  summary.sse = o.sseUrl ? { clients: sseRuns.length, events: sse.events, errors: sse.errors } : null;
  printTable([summary]);
  const out = { mode: "backend", url: o.url, params: o, results: [summary] };
  if (o.out) writeFileSync(o.out, JSON.stringify(out, null, 2));
  else console.log(JSON.stringify(out, null, 2));
}

const o = parseArgs(process.argv.slice(2));
await (o.mode === "contract" ? contractMode(o) : backendMode(o));
