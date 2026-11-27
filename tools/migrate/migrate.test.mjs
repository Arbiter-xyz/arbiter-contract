import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { plan, run } from "./migrate.mjs";

test("plan flags and run skips a disputed question retained in the source index", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "arbiter-migrate-test-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const o = {
    source: "source",
    target: "target",
    network: "testnet",
    admin: "admin",
    batch: 10,
    journal: join(directory, "journal.jsonl"),
  };
  const disputed = {
    status: "Disputed",
    payer: "payer",
    amount: 100n,
    created_at: 10,
    timeout_ledgers: 20,
  };
  const client = {
    latestLedger: () => 11,
    pendingCount: () => 1,
    pendingPage: () => [42n],
    question: (contract) => contract === "source" ? disputed : null,
    questionToken: () => "token",
    migrationSource: () => "source",
    tokenOf: () => "token",
    balance: () => 100n,
    call: () => assert.fail("a disputed question must not be sent to migrate_pending"),
  };

  const report = plan(o, client);
  assert.equal(report.pending, 0);
  assert.deepEqual(report.batches, []);
  assert.ok(report.problems.some((problem) => problem.includes("question 42 is Disputed")));
  assert.deepEqual(report.blockers, []);

  const output = [];
  const originalLog = console.log;
  console.log = (...args) => output.push(args.join(" "));
  try {
    run(o, client);
  } finally {
    console.log = originalLog;
  }
  assert.ok(output.some((line) => line.includes("not migrated: #42 (Disputed)")));
});
