import test from "node:test";
import assert from "node:assert/strict";
import { writeFileSync, unlinkSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  AttemptTimeout,
  withTimeout,
  referenceRetry,
  validateRetryContract,
  loadRetry,
  instrument,
  checkRetrySpec,
} from "./retry.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(__dirname, ".tmp-test-fixtures");

test("referenceRetry passes validateRetryContract", async () => {
  await validateRetryContract(referenceRetry, "referenceRetry");
  const loaded = await loadRetry();
  assert.equal(loaded.name, "reference");
  assert.equal(loaded.retry, referenceRetry);
});

test("withTimeout resolves fast promises and rejects slow ones with AttemptTimeout", async () => {
  const fast = await withTimeout(Promise.resolve("ok"), 100);
  assert.equal(fast, "ok");

  await assert.rejects(
    () => withTimeout(new Promise((r) => setTimeout(r, 100)), 15),
    (err) => err instanceof AttemptTimeout && /timed out after 15 ms/.test(err.message)
  );
});

test("loadRetry adapts withRetry, retry, and default exports and validates contract", async () => {
  mkdirSync(fixtureDir, { recursive: true });
  const validPath = join(fixtureDir, "valid-retry.mjs");
  const invalidExportPath = join(fixtureDir, "invalid-export.mjs");
  const brokenOptsPath = join(fixtureDir, "broken-opts.mjs");

  try {
    writeFileSync(
      validPath,
      `export async function withRetry(fn, { attempts = 2, baseDelayMs = 1, nonRetryable = () => false } = {}) {
        let last;
        for (let i = 0; i < attempts; i++) {
          if (i > 0) await new Promise((r) => setTimeout(r, baseDelayMs));
          try {
            return await fn(i);
          } catch (e) {
            last = e;
            if (nonRetryable(e)) throw e;
          }
        }
        throw last;
      }\n`
    );

    const loaded = await loadRetry(validPath);
    assert.equal(loaded.name, validPath);
    const val = await loaded.retry(() => 42, { attempts: 2, baseDelayMs: 1 });
    assert.equal(val, 42);

    writeFileSync(invalidExportPath, `export const notAFunction = 123;\n`);
    await assert.rejects(() => loadRetry(invalidExportPath), /no withRetry\/retry\/default export/);

    // A module that ignores opts.attempts should fail contract validation
    writeFileSync(
      brokenOptsPath,
      `export async function retry(fn) {
        return await fn(0);
      }\n`
    );
    await assert.rejects(() => loadRetry(brokenOptsPath), /executed 1 attempt\(s\) instead of 2/);
  } finally {
    try {
      unlinkSync(validPath);
    } catch {}
    try {
      unlinkSync(invalidExportPath);
    } catch {}
    try {
      unlinkSync(brokenOptsPath);
    } catch {}
    rmSync(fixtureDir, { recursive: true, force: true });
  }
});

test("instrument and checkRetrySpec detect clean runs and spec violations", async () => {
  const log = [];
  let count = 0;
  const wrapped = instrument(log, { callId: "c1", label: "submit" }, async () => {
    count++;
    if (count === 1) throw new Error("transient 503");
    return "done";
  });

  const out = await referenceRetry(wrapped, { attempts: 2, baseDelayMs: 20, timeoutMs: 200 });
  assert.equal(out, "done");
  assert.equal(log.length, 2);

  const report = checkRetrySpec(log, { attempts: 2, baseDelayMs: 20, timeoutMs: 200 });
  assert.equal(report.calls, 1);
  assert.deepEqual(report.violations, []);

  // Synthetic violation: too many attempts & retried after success
  const badLog = [
    { callId: "c2", label: "resolve", attempt: 0, start: 1000, end: 1010, ok: true, error: null },
    { callId: "c2", label: "resolve", attempt: 1, start: 1015, end: 1020, ok: false, error: "err" },
    { callId: "c2", label: "resolve", attempt: 2, start: 1025, end: 1030, ok: true, error: null },
  ];
  const badReport = checkRetrySpec(badLog, { attempts: 2, baseDelayMs: 100, timeoutMs: 200, jitterMs: 5 });
  const kinds = badReport.violations.map((v) => v.kind);
  assert.ok(kinds.includes("too-many-attempts"));
  assert.ok(kinds.includes("backoff-too-short"));
  assert.ok(kinds.includes("retried-after-success"));
});
