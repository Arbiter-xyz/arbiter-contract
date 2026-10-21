// Retry plumbing for the chaos driver (issue #121).
//
// The thing under test is arbiter-backend's retry.js: bounded exponential
// backoff (2 attempts) with a single-digit-second per-attempt timeout. It
// lives in the other repo, so there are two ways to get it here:
//
//   --retry-module ../arbiter-backend/src/retry.js   the real one (preferred)
//   (nothing)                                        referenceRetry below,
//                                                    written to the same spec
//
// Either way every attempt goes through `instrument()`, which records when
// it started and how it ended, independently of the retry implementation.
// That record is what the driver checks against the spec: never more than
// `attempts` attempts, and each backoff at least the spec's minimum.

import { pathToFileURL } from "node:url";
import { resolve as resolvePath } from "node:path";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export class AttemptTimeout extends Error {
  constructor(ms) {
    super(`attempt timed out after ${ms} ms`);
    this.name = "AttemptTimeout";
  }
}

export function withTimeout(promise, ms) {
  let t;
  return Promise.race([
    promise.finally(() => clearTimeout(t)),
    new Promise((_, reject) => {
      t = setTimeout(() => reject(new AttemptTimeout(ms)), ms);
    }),
  ]);
}

/// Same contract as retry.js: `attempts` tries in total, the k-th retry
/// waits baseDelayMs * 2^(k-1), each try bounded by timeoutMs. A
/// `nonRetryable(err)` error (a contract error: retrying can't change the
/// answer) is thrown immediately.
export async function referenceRetry(fn, { attempts = 2, timeoutMs = 8000, baseDelayMs = 500, nonRetryable = () => false } = {}) {
  let last;
  for (let i = 0; i < attempts; i++) {
    if (i > 0) await sleep(baseDelayMs * 2 ** (i - 1));
    try {
      return await withTimeout(Promise.resolve().then(() => fn(i)), timeoutMs);
    } catch (e) {
      last = e;
      if (nonRetryable(e)) throw e;
    }
  }
  throw last;
}

/// Loads retry.js from arbiter-backend and adapts it to
/// `(fn, opts) => Promise`. retry.js's exact export isn't pinned by this
/// repo, so this accepts the obvious shapes. If none fits, adapt it here:
/// this function is the only place that knows about retry.js's API.
///
/// Because there's no version pin or contract test against the real
/// retry.js, a signature change there would otherwise break the harness
/// silently. So after resolving the export we probe it once with a
/// throwaway call and fail loudly with a clear diagnostic if it doesn't
/// behave like `(fn, opts) => Promise`.
export async function loadRetry(modulePath) {
  if (!modulePath) return { name: "reference", retry: referenceRetry };
  const mod = await import(pathToFileURL(resolvePath(modulePath)).href);
  const f = mod.withRetry ?? mod.retry ?? mod.default?.withRetry ?? mod.default?.retry ?? mod.default;
  if (typeof f !== "function") {
    throw new Error(`${modulePath}: no withRetry/retry/default export; adapt loadRetry() in tools/chaos/retry.mjs`);
  }
  const retry = (fn, opts) => f(fn, opts);
  await assertRetryContract(modulePath, retry);
  return { name: modulePath, retry };
}

/// Probes a loaded retry implementation against the contract the chaos
/// harness relies on: `retry(fn, opts)` returns a Promise, calls `fn` at
/// least once, and resolves with `fn`'s value. Throws a diagnostic naming
/// the module and the observed shape if the contract is broken, so a
/// retry.js signature change fails loudly instead of silently.
async function assertRetryContract(modulePath, retry) {
  let calls = 0;
  const sentinel = Symbol("retry-contract-probe");
  let result;
  try {
    result = await retry(() => {
      calls++;
      return sentinel;
    }, { attempts: 1, timeoutMs: 1000, baseDelayMs: 0 });
  } catch (e) {
    throw new Error(
      `${modulePath}: retry export does not satisfy the (fn, opts) => Promise contract ` +
        `(probe threw: ${e?.message ?? e}); adapt loadRetry() in tools/chaos/retry.mjs`,
    );
  }
  if (calls < 1) {
    throw new Error(
      `${modulePath}: retry export never invoked the wrapped fn; ` +
        `expected (fn, opts) => Promise. Adapt loadRetry() in tools/chaos/retry.mjs`,
    );
  }
  if (result !== sentinel) {
    throw new Error(
      `${modulePath}: retry export did not resolve with fn's return value ` +
        `(got ${typeof result}); expected (fn, opts) => Promise. ` +
        `Adapt loadRetry() in tools/chaos/retry.mjs`,
    );
  }
}

/// Wraps `fn` so every attempt is logged into `log` as
/// { callId, label, attempt, start, end, ok, error, nonRetryable }. All attempts
/// of one retry() call share `callId`. `end` is when fn itself settled, which
/// for a timed-out attempt can be after the retry layer gave up on it.
///
/// When `nonRetryable` is provided, each caught error is tested against it and
/// the result is recorded on the attempt record.
export function instrument(log, { callId, label }, fn, { nonRetryable = () => false } = {}) {
  let n = 0;
  return async (...args) => {
    const rec = { callId, label, attempt: n++, start: Date.now(), end: null, ok: false, error: null, nonRetryable: false };
    log.push(rec);
    try {
      const v = await fn(...args);
      rec.ok = true;
      return v;
    } catch (e) {
      rec.error = String(e?.message ?? e).slice(0, 120);
      rec.nonRetryable = nonRetryable(e);
      throw e;
    } finally {
      rec.end = Date.now();
    }
  };
}

/// Checks the attempt log against the retry spec. Returns
/// { calls, violations }; no violations = the retry layer behaved as
/// specified under the injected faults.
///
/// Gaps are measured start to start, because a timed-out attempt's own end
/// isn't observable from inside it. So:
///   start[k] - start[k-1] >= backoff(k)                    (backoff honoured)
///   start[k] - start[k-1] <= timeoutMs + 2*backoff(k) + slackMs
///                                                          (per-attempt timeout
///                                                           enforced, allowing
///                                                           up to 2x jitter)
/// with backoff(k) = baseDelayMs * 2^(k-1).
export function checkRetrySpec(log, { attempts, baseDelayMs, timeoutMs, jitterMs = 50, slackMs = 1000 }) {
  const byCall = new Map();
  for (const r of log) {
    if (!byCall.has(r.callId)) byCall.set(r.callId, []);
    byCall.get(r.callId).push(r);
  }
  const violations = [];
  for (const [id, recs] of byCall) {
    recs.sort((a, b) => a.start - b.start);
    const label = recs[0].label;
    if (recs.length > attempts) violations.push({ call: id, label, kind: "too-many-attempts", attempts: recs.length });
    for (let k = 1; k < recs.length; k++) {
      const gap = recs[k].start - recs[k - 1].start;
      const backoff = baseDelayMs * 2 ** (k - 1);
      if (gap < backoff - jitterMs) {
        violations.push({ call: id, label, kind: "backoff-too-short", retry: k, gap_ms: gap, min_ms: backoff });
      }
      if (gap > timeoutMs + 2 * backoff + slackMs) {
        violations.push({ call: id, label, kind: "attempt-timeout-not-enforced", retry: k, gap_ms: gap, max_ms: timeoutMs + 2 * backoff + slackMs });
      }
    }
    if (recs.some((r, i) => r.ok && i < recs.length - 1)) {
      violations.push({ call: id, label, kind: "retried-after-success" });
    }
    // The first attempt's error was classified nonRetryable — the retry layer
    // must throw immediately without further attempts.
    if (recs.length > 1 && recs[0].nonRetryable) {
      violations.push({ call: id, label, kind: "retried-after-nonRetryable", attempts: recs.length });
    }
  }
  return { calls: byCall.size, violations };
}
