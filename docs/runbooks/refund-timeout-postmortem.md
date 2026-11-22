# Runbook: elevated `refund_timeout()` activity (issue #118)

**Use this when** questions start settling through `refund_timeout()` (or
`sweep_timeouts()`) at a noticeably higher rate than your baseline, or
when the backend's `lost_race_to_timeout_refund` log tag starts showing
up often.

**Read first:**
[SETTLEMENT_RACES.md](../SETTLEMENT_RACES.md) explains why every race
here is safe for funds. [ttl-archival.md](../ttl-archival.md) explains
why a timeout refund still works after archival.

**Revisit when #109 (circuit breaker) lands.** Section 4 describes the
levers that exist *today*, and today there's no contract-level pause.

---

## 0. What the contract guarantees, in one paragraph

Every question snapshots `timeout_ledgers` from the global
`TimeoutLedgers` value when it's opened: `Question.timeout_ledgers`, set
in `open_question()` in `src/lib.rs`. Once
`ledger >= created_at + timeout_ledgers`, **anyone** can call
`refund_timeout(id)`, with no `require_auth()` at all. The call moves the
full `amount` back to `Question.payer`. No other address can ever receive
it.

`set_timeout_ledgers()` changes the default for **future** questions only.
It can't push out the deadline of a question that's already pending.
That's deliberate: an admin can't disable the escape hatch after the
fact.

`resolve()` and `refund_timeout()` both write `Question(id)`, so they're
totally ordered. Whichever lands first wins. The other fails with
`QuestionNotPending` (#6). Before the deadline, `refund_timeout()` fails
with the not-yet-due error.

**Consequence for triage:** a timeout refund never loses anyone's money.
It does mean workers who already answered **aren't paid**, and the
platform earns no fee on that question. The payer gets back money for
work that may already have been delivered to them.

**Fail-closed depends on a keeper.** The contract cannot call
`refund_timeout()` on its own. The "funds are always recoverable"
guarantee is only *fail-closed* if something actually calls it once the
deadline passes. That something is arbiter-backend's keeper/sweeper (or
any third party, since the call is unauthenticated). If no keeper is
deployed and monitored wherever this contract is live, a question can sit
`Pending` past its deadline indefinitely and the payer's funds stay
locked. This is a **deployment gap, not a contract gap**: the contract
correctly permits the refund, but nothing triggers it. See §1.3 for the
liveness check that catches a missing or stalled keeper.

---

## 1. Detection

### 1.1 Signals

| signal | source | what "elevated" looks like |
|---|---|---|
| `question_refunded` events with `via_timeout = true` | contract events (topic: `question_id`; data: `payer`, `amount`, `via_timeout`) | timeout refunds / all settlements, per hour, above your normal level (normally ≈ 0) |
| `lost_race_to_timeout_refund` | arbiter-backend `oracle.js` structured logs | any sustained rate: each one is a `resolve()` that was built and sent but arrived after a refund |
| pending backlog near its deadline | `pending_count()`, `list_pending(start, limit)` plus `get_question(id)` per id | count of pending ids with `created_at + timeout_ledgers − current_ledger` < your resolve p99 latency |
| `question_settled` with status `Refunded` | contract events | the same as the first row, but also counts admin `refund()`. Use `via_timeout` to tell them apart |

`lost_race_to_timeout_refund` is emitted by the backend (a separate repo,
`arbiter-backend`). It isn't a contract event. The backend tags a
`resolve()` that failed with `QuestionNotPending` when the question's
on-chain status is `Refunded`. That's what separates a race it lost from
a double-resolve bug. Correlate it with the contract's
`question_refunded` event by `question_id`.

### 1.2 On-chain reads you'll need

```sh
# Per question: created_at, timeout_ledgers (the SNAPSHOT that governs it), status, payer, amount
stellar contract invoke --id $CONTRACT --source-account $ANY --network $NET -- \
  get_question --question_id $ID

# Global default for NEW questions only. It doesn't say anything about the ones already open.
stellar contract invoke --id $CONTRACT ... -- get_timeout_ledgers

# Current pending backlog
stellar contract invoke --id $CONTRACT ... -- pending_count
stellar contract invoke --id $CONTRACT ... -- list_pending --start 0 --limit 100

# Timeout-refund events in a window (their tx hash leads to the CALLER; see 2.1)
stellar events --network $NET --id $CONTRACT --start-ledger $FROM --type contract
```

For each refunded question, compute these three numbers. Everything in
section 2 is built from them:

- `deadline = created_at + timeout_ledgers`
- `refund_lag = refund_ledger − deadline` (≥ 0 always)
- `resolve_lag = backend_resolve_submit_ledger − deadline`, from the
  backend log. Negative means the backend *tried* before the deadline.

**Don't** use the current `get_timeout_ledgers()` value to compute a
deadline. Always use the question's own `timeout_ledgers` from
`get_question()`. The two differ for every question opened before the
last `set_timeout_ledgers()` call.

### 1.3 Keeper liveness check (fail-closed precondition)

Before trusting the fail-closed claim, confirm a keeper is actually
running and sweeping. The contract can't self-trigger `refund_timeout()`,
so a missing or stalled keeper silently breaks the guarantee.

```sh
# 1. Are there pending questions already past their deadline?
#    Any id here means the keeper is not keeping up (or isn't deployed).
stellar contract invoke --id $CONTRACT ... -- list_pending --start 0 --limit 100
#    For each id, get_question and check: current_ledger >= created_at + timeout_ledgers

# 2. Is the keeper's sweeper account still submitting?
#    Look for recent sweep_timeouts() / refund_timeout() txs from the keeper
#    source account in the last N ledgers.
stellar events --network $NET --id $CONTRACT --start-ledger $FROM --type contract
```

If step 1 finds overdue pending questions and step 2 shows no recent
keeper activity, treat it as a **deployment gap**: the fail-closed claim
does not hold in this environment. Escalate to whoever owns the
arbiter-backend deployment and confirm the keeper/sweeper is deployed,
funded, and monitored. Until it is, payers' funds on overdue questions
are locked, not refunded.

---

## 2. Triage: which of the three is it?

### 2.1 Find the caller

`refund_timeout()` has no auth, so the contract doesn't record who called
it. Take the `txHash` from the `question_refunded` event and fetch the
transaction with RPC `getTransaction`, or look it up in a block explorer.
The transaction's **source account** is the caller.

Sort every timeout refund in the incident window into one of these
buckets:

- **backend / keeper:** your own sweeper account, if you run one that
  calls `sweep_timeouts()`
- **payer:** the source account equals `Question.payer`, or is funded by it
- **third party:** anyone else

### 2.2 Decision table

| | A. backend was down | B. lost races (backend slow, not down) | C. deliberate payer front-running |
|---|---|---|---|
| `resolve()` txs from the admin during the window | **none or near zero, for every question** | present. Many succeed, some fail with `QuestionNotPending` | present and mostly succeed. Failures concentrate on a few payers |
| `lost_race_to_timeout_refund` logs | few or none (the backend never got as far as sending) | **many** | some, concentrated on the same payers |
| `resolve_lag` | no resolve attempt at all | clustered just **around 0**: attempts land near or after the deadline | spread out; the refund just happens to beat them |
| `refund_lag` | spread out: whoever noticed first | small | **consistently 0–2 ledgers**: the refund lands in the first ledger it can |
| caller (2.1) | mixed: many payers, keepers, your sweeper | mixed | **the payer**, repeatedly, or an account it funds |
| payers affected | many, in proportion to traffic | many, skewed toward questions that took longest to answer | **few**, over-represented far beyond their traffic share |
| answer delivered to the payer before the refund? | usually no | sometimes | **yes**: they got the result off-chain, then refunded |

**Why "attacker front-running resolve()" means the payer.** The refund
always goes to `Question.payer`. A third party who races
`refund_timeout()` gains nothing financially. At most they grief the
workers out of a payment the backend was about to make.

So a third-party caller is a nuisance, or a keeper doing its job. It
isn't a profit-motivated attack. The profit motive belongs to a
**payer**: someone who receives answers before settlement, then pays
nothing by refunding the moment the deadline passes.

Note that this only works when the backend hasn't resolved by the
deadline. **C always depends on B.** A fast backend leaves nothing to
race.

### 2.3 Quick checks, in order

1. **Did any `resolve()` from the admin land in the window?** No → it's
   **A**. Go to §4.1.
2. **Is `resolve_lag` clustered near 0 across many payers?** Yes → it's
   **B**. The backend's end-to-end latency (answer collection, then
   `resolve()` inclusion, plus RPC read-after-write lag and retries) is
   eating the whole window. Go to §4.2.
3. **Is it the same few payers, calling it themselves, with `refund_lag`
   of about 0?** Yes → it's **C**, running on top of B. Go to §4.2 and
   §4.3.
4. **Do `resolve()` calls fail with an auth error instead of
   `QuestionNotPending`?** Then this isn't a timeout problem. It's an
   admin-key problem. Go to §4.4.

---

## 3. Recommended starting `timeout_ledgers`

This addresses open question 2 of the issue. This repo has no
production downtime data to fit a value to. The honest answer is a
method, plus a default to start from.

    timeout_ledgers ≥ p99.9(question → resolve() included)  +  longest outage you're willing to absorb without refunds

**Starting point: 

/* … truncated 6240 chars — edit only what you need near the top … */
