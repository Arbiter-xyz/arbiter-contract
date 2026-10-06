// Fault-injecting HTTP proxy for Soroban RPC (JSON-RPC) and Horizon.
// Issue #121; see docs/RPC_CHAOS_TESTING.md.
//
// Sits between a client (arbiter-backend's stellarClient.js, or
// tools/chaos/chaos.mjs) and a real upstream. For every request it rolls a
// seeded die and does one of:
//
//   pass          forward unchanged
//   delay         forward after a random delay
//   hang          never answer (the client's per-attempt timeout fires);
//                 the request is NOT forwarded
//   httpError     answer 503 without forwarding
//   rpcError      answer a JSON-RPC internal error without forwarding
//                 (Horizon: a 504 problem document)
//   dropAfter     forward, discard the upstream answer, then hang or 503.
//                 For sendTransaction / POST /transactions this is the
//                 "landed but the client never saw it" case
//
// plus two read-after-write lag models, closer to what round 6 observed on
// testnet than random errors are:
//
//   lag.getTransactionMs     getTransaction answers NOT_FOUND for this long
//                            after upstream first reports the tx included
//   lag.staleReadsMs         getLedgerEntries answers with the previous
//                            upstream answer for the same keys, if it's
//                            younger than this
//   lag.latestLedgerBehind   getLatestLedger reports this many ledgers
//                            fewer than upstream
//
// A profile is JSON:
//   { "default": { "p": { "delay": 0.1, "hang": 0.02, ... }, "delayMs": [200, 3000], "hangMs": 60000 },
//     "methods": { "sendTransaction": { "p": { "dropAfter": 0.1 } } },
//     "lag": { "getTransactionMs": 8000, "staleReadsMs": 6000, "latestLedgerBehind": 2 } }
// Method keys are JSON-RPC method names, or "VERB /first-segment" for
// Horizon (e.g. "POST /transactions"). Per-method settings override
// `default` field by field.
//
// Control endpoints on the proxy itself:
//   GET  /__chaos            stats and the active profile
//   POST /__chaos            replace the profile (body: profile JSON)
//   POST /__chaos/off        pass everything through (stats still counted)
//   POST /__chaos/on

import http from "node:http";

export const FAULTS = ["delay", "hang", "httpError", "rpcError", "dropAfter"];

/// mulberry32: small, seedable, good enough to make a run repeatable.
export function rng(seed) {
  let a = Number(seed) >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function settingsFor(profile, method) {
  const d = profile.default ?? {};
  const m = profile.methods?.[method] ?? {};
  return {
    p: { ...(d.p ?? {}), ...(m.p ?? {}) },
    delayMs: m.delayMs ?? d.delayMs ?? [100, 2000],
    hangMs: m.hangMs ?? d.hangMs ?? 60_000,
  };
}

/// Picks one fault (or "pass") for a request. Probabilities are checked in
/// FAULTS order against one roll, so they must sum to <= 1.
export function pickFault(profile, method, roll) {
  const { p } = settingsFor(profile, method);
  let acc = 0;
  for (const f of FAULTS) {
    acc += p[f] ?? 0;
    if (roll < acc) return f;
  }
  return "pass";
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

export function createChaosProxy({ upstream, kind = "rpc", profile = {}, seed = 1, log = () => {} }) {
  const rand = rng(seed);
  const state = {
    enabled: true,
    profile,
    stats: {}, // method -> { requests, pass, delay, ... , lagged }
    firstSeenTx: new Map(), // hash -> ms
    lastRead: new Map(), // getLedgerEntries params -> { at, body }
  };
  const up = new URL(upstream);

  const bump = (method, what) => {
    const s = (state.stats[method] ??= { requests: 0, pass: 0, lagged: 0, ...Object.fromEntries(FAULTS.map((f) => [f, 0])) });
    s[what] = (s[what] ?? 0) + 1;
  };

  async function forward(req, body) {
    const target = new URL(req.url, up);
    // Keep the upstream's path prefix (e.g. /rpc on quickstart).
    if (kind === "rpc") target.pathname = up.pathname;
    const headers = { ...req.headers, host: up.host };
    delete headers["content-length"];
    const r = await fetch(target, {
      method: req.method,
      headers,
      body: ["GET", "HEAD"].includes(req.method) ? undefined : body,
    });
    return { status: r.status, headers: Object.fromEntries(r.headers), body: Buffer.from(await r.arrayBuffer()) };
  }

  function send(res, { status, headers = {}, body }) {
    const h = { ...headers };
    delete h["content-length"];
    delete h["content-encoding"];
    delete h["transfer-encoding"];
    res.writeHead(status, h);
    res.end(body);
  }

  function rpcError(id, message) {
    return {
      status: 200,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32603, message: `chaos: ${message}` } }),
    };
  }

  function faultResponse(fault, rpcId) {
    if (fault === "httpError") return { status: 503, headers: { "content-type": "text/plain" }, body: "chaos: 503" };
    if (kind === "rpc") return rpcError(rpcId, fault);
    return {
      status: 504,
      headers: { "content-type": "application/problem+json" },
      body: JSON.stringify({ type: "chaos", title: "Timeout", status: 504, detail: `chaos: ${fault}` }),
    };
  }

  /// Read-after-write lag. Returns a response to serve instead of the
  /// upstream one, or null. Only JSON-RPC.
  function lagged(method, params, upstreamRes) {
    const lag = state.profile.lag ?? {};
    const now = Date.now();
    let parsed;
    try {
      parsed = JSON.parse(upstreamRes.body.toString());
    } catch {
      return null;
    }
    if (!parsed.result) return null;

    if (method === "getTransaction" && lag.getTransactionMs) {
      // Lag is measured from when upstream first reported the tx included,
      // not from the client's first poll.
      const hash = params?.hash;
      const included = parsed.result.status !== "NOT_FOUND";
      if (included && !state.firstSeenTx.has(hash)) state.firstSeenTx.set(hash, now);
      if (included && now - state.firstSeenTx.get(hash) < lag.getTransactionMs) {
        const r = { ...parsed.result, status: "NOT_FOUND" };
        for (const k of ["envelopeXdr", "resultXdr", "resultMetaXdr", "returnValue", "ledger", "createdAt"]) delete r[k];
        return { ...upstreamRes, body: JSON.stringify({ ...parsed, result: r }) };
      }
    }

    if (method === "getLedgerEntries" && lag.staleReadsMs) {
      const key = JSON.stringify(params);
      const prev = state.lastRead.get(key);
      if (prev && now - prev.at < lag.staleReadsMs && prev.body !== upstreamRes.body.toString()) {
        // Serve the older view; don't refresh the cache so the lag is
        // measured from when that older view was taken.
        return { ...upstreamRes, body: prev.body };
      }
      state.lastRead.set(key, { at: now, body: upstreamRes.body.toString() });
    }

    if (method === "getLatestLedger" && lag.latestLedgerBehind) {
      const r = { ...parsed.result, sequence: parsed.result.sequence - lag.latestLedgerBehind };
      return { ...upstreamRes, body: JSON.stringify({ ...parsed, result: r }) };
    }
    return null;
  }

  const server = http.createServer(async (req, res) => {
    try {
      if (req.url.startsWith("/__chaos")) return control(req, res);
      const body = await readBody(req);

      let method;
      let rpcId = null;
      let params;
      if (kind === "rpc") {
        try {
          const j = JSON.parse(body.toString());
          method = j.method;
          rpcId = j.id ?? null;
          params = j.params;
        } catch {
          method = "unparseable";
        }
      } else {
        method = `${req.method} /${new URL(req.url, "http://x").pathname.split("/")[1] ?? ""}`;
      }
      bump(method, "requests");

      const fault = state.enabled ? pickFault(state.profile, method, rand()) : "pass";
      const cfg = settingsFor(state.profile, method);
      bump(method, fault);
      log({ at: Date.now(), method, fault });

      switch (fault) {
        case "hang":
          await sleep(cfg.hangMs);
          return res.destroy();
        case "httpError":
        case "rpcError":
          return send(res, faultResponse(fault, rpcId));
        case "dropAfter": {
          await forward(req, body).catch(() => {});
          if (rand() < 0.5) {
            await sleep(cfg.hangMs);
            return res.destroy();
          }
          return send(res, faultResponse("httpError", rpcId));
        }
        case "delay": {
          const [lo, hi] = cfg.delayMs;
          await sleep(lo + rand() * (hi - lo));
          break;
        }
      }

      const upstreamRes = await forward(req, body);
      const lag = kind === "rpc" && state.enabled ? lagged(method, params, upstreamRes) : null;
      if (lag) bump(method, "lagged");
      send(res, lag ?? upstreamRes);
    } catch (e) {
      if (!res.headersSent) send(res, { status: 502, headers: {}, body: `chaos proxy: ${e.message}` });
    }
  });

  async function control(req, res) {
    const json = (o) => send(res, { status: 200, headers: { "content-type": "application/json" }, body: JSON.stringify(o, null, 2) });
    if (req.method === "GET") return json({ enabled: state.enabled, profile: state.profile, stats: state.stats });
    if (req.url === "/__chaos/off") state.enabled = false;
    else if (req.url === "/__chaos/on") state.enabled = true;
    else state.profile = JSON.parse((await readBody(req)).toString());
    return json({ enabled: state.enabled });
  }

  return {
    server,
    state,
    setProfile(p) {
      state.profile = p;
    },
    setEnabled(on) {
      state.enabled = on;
    },
    stats: () => structuredClone(state.stats),
    listen: (port) => new Promise((r) => server.listen(port, "127.0.0.1", () => r(server.address().port))),
    close: () =>
      new Promise((r) => {
        server.close(() => r());
        server.closeAllConnections(); // hung requests would otherwise hold close() open
      }),
  };
}
