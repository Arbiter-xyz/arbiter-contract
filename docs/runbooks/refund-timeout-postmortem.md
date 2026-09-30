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

**Starting point: 17,280 ledgers (1 day at 5 s ledgers).** That's the
value the economics simulations initialize with (`src/test_economics.rs`).

- It covers a same-day incident response with plenty of room.
- It keeps the payer's worst-case wait at a day.
- It's far below `MAX_TIMEOUT_LEDGERS` (120,960, 7 days).

Only shorten it once you have measured latency data showing the p99.9
sits well inside the window. Remember that a change only applies to
questions opened *after* it.

---

## 4. Mitigations: what exists today, and what doesn't

**The gap, stated plainly.** There's no contract-level pause, circuit
breaker or deadline extension. #109 would add one. Nothing an operator
does can stop `refund_timeout()` from succeeding on a question that's
already past its deadline. That's the whole point of the escape hatch,
and it's working as designed. Every lever below either **makes the
backend resolve sooner** or **changes what future questions get**.

### 4.1 A: backend down

1. Restore backend liveness. That's the only thing that stops new timeout
   refunds.
2. On recovery, **resolve by deadline, soonest first.** Walk
   `list_pending` → `get_question` and sort by `created_at +
   timeout_ledgers`. The default FIFO or id order can spend the remaining
   window on questions that aren't at risk yet.
3. Accept that questions already past their deadline may be refunded by
   anyone before you get to them. Resolving them is still correct and
   safe: if the refund wins, `resolve()` just fails with
   `QuestionNotPending`.

### 4.2 B: losing races

1. Find where the latency is going: answer collection versus `resolve()`
   submission and inclusion. Look at RPC read-after-write lag, fee-bump
   retries, and a single-threaded submit queue.
2. Prioritise resolves by deadline, as in §4.1 step 2.
3. `set_timeout_ledgers(larger)` helps **new questions only**. Use it if
   p99 latency really is close to the window. It doesn't rescue anything
   currently pending.

### 4.3 C: payer front-running

The contract has no lever here: the payer is entitled to the refund.
The mitigations are backend policy:

- **Don't release answers to the payer until `resolve()` is confirmed**,
  or at least don't release them within the last N ledgers before a
  question's deadline. This takes away the payoff.
- Rate-limit, require prepayment through `deposit()`/`charge()`, or
  refuse service to payers whose timeout-refund rate is far above
  baseline.
- Workers aren't paid for these questions on-chain. Deciding whether to
  compensate them off-chain is a business decision. Record it in the
  postmortem.

`reopen_question()` (#87) lets a payer voluntarily re-fund a refunded id
under a new window. That's only useful for honest payers who were hit by
case A or B.

### 4.4 Suspected admin-key compromise

- `set_admin()` / `propose_admin_rotation()` **don't take effect
  immediately**. Both schedule a rotation that can't run before
  `ADMIN_ROTATION_DELAY_LEDGERS` (~8 days), and it needs the new admin's
  authorisation too. See
  [MULTI_ASSET_AND_ADMIN_ROTATION.md](../MULTI_ASSET_AND_ADMIN_ROTATION.md)
  and the #107 runbook.
- **During those 8 days the compromised key can still call
  `resolve()`/`refund()`.** Rotation isn't an incident-response lever on
  the timescale of a timeout window. It's a recovery step.
- Watch `get_pending_admin_rotation()`. If an attacker proposes a
  rotation, `cancel_admin_rotation()` it with the incumbent key.
- In this scenario `refund_timeout()` is actually the **payer's
  protection**: a compromised admin can refuse to resolve, but it can't
  keep payers' funds.

---

## 5. Postmortem data to capture

Capture this while it's fresh. RPC event retention is limited, and
`getEvents` typically covers only about the last 7 days.

**Per affected question:**

| field | from |
|---|---|
| `question_id`, `payer`, `amount`, token (`get_question_token`) | `get_question()` |
| `created_at`, `timeout_ledgers` (snapshot), `deadline` | `get_question()`, computed |
| refund ledger, refund tx hash, **caller** (source account) | `question_refunded` event → `getTransaction` |
| `refund_lag` | computed |
| backend: answers-collected time, first `resolve()` attempt ledger, number of attempts, final error | arbiter-backend logs (`oracle.js`, `lost_race_to_timeout_refund`) |
| `resolve_lag` | computed |
| was the answer delivered to the payer before the refund? (y/n, when) | backend delivery logs |
| workers who answered, and the share they would have been paid | backend logs + `preview_resolve()` if it was run |

**Incident-wide:**

- The window: first and last timeout refund ledger.
- Timeout refunds / all settlements during the window, and at baseline.
- The `get_timeout_ledgers()` value, plus whether `set_timeout_ledgers()`
  changed during or just before the window (`timeout_ledgers_changed`
  events).
- The backend's uptime timeline, and its p50/p99 latency from question to
  `resolve()` included.
- The caller breakdown (backend / payer / third party) with counts.
- The top payers by timeout-refund count, and each one's share of total
  traffic.
- The classification (A / B / C / key) and the evidence for it from §2.2.
- The total value refunded, and the total worker pay and platform fee
  forgone.

**Postmortem template:**

```
## Timeout-refund incident <date>
Window: ledgers <from>–<to>  (<wall-clock>)
Classification: A / B / C / key-compromise   Evidence: <§2.2 rows that decided it>
Impact: <n> questions, <x> refunded, <y> worker pay forgone, <z> fee forgone
Callers: backend <n>, payer <n>, third-party <n>
Latency: p50/p99 question→resolve <..>, timeout_ledgers in force <..>
Root cause:
What stopped it:
Follow-ups: (backend latency / delivery-before-settlement policy / timeout_ledgers change / #109)
```

---

## 6. Cross-references

- `src/lib.rs`: `refund_timeout()`, `try_refund_timeout()`,
  `sweep_timeouts()`, `do_refund()`, `Question.timeout_ledgers`,
  `DataKey::TimeoutLedgers`, `set_timeout_ledgers()`.
- `src/test_races.rs`: `resolve_vs_refund_timeout_in_every_order_and_adjacent_ledger`.
- arbiter-backend `oracle.js`: the `lost_race_to_timeout_refund` log tag.
- #107: admin rotation runbook.
- #109: circuit breaker. **When it lands, §4 needs rewriting.**
