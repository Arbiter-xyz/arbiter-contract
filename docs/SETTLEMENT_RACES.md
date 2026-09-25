# Settlement races: threat model

What can happen when several parties try to settle the same question at
the same time, and why each case is safe. Tests:
[`src/test_races.rs`](../src/test_races.rs).

## Settlement paths

A question is opened by `submit()` or `charge()` and starts `Pending`.
Exactly one of these may move it to a terminal state:

| path | who | valid when | result |
|---|---|---|---|
| `resolve(id, workers, losing_workers)` | admin | Pending | Resolved: fee + dust + slashes to the platform, shares credited to workers |
| `refund(id)` | admin | Pending | Refunded: full amount to the payer |
| `refund_timeout(id)` | anyone, no auth | Pending and `ledger >= created_at + timeout_ledgers` | Refunded: full amount to the payer |

`Resolved` and `Refunded` are terminal. A settled question id can never be
reopened, because `open_question()` rejects any id that already has an
entry.

## What an attacker controls (and what they don't)

The contract's safety rests on these properties of Soroban execution.
They are guarantees of the network, not assumptions about honest callers:

1. **Atomic transactions.** Each Soroban transaction carries exactly one
   `InvokeHostFunction` operation, and it either applies all its effects or
   none. A failed transaction still pays its fee but changes no state: no
   partial transfer, no half-written question.
2. **Serial application within a ledger.** A ledger's transactions apply
   in a deterministic order the network picks. Fees decide whether a
   transaction gets into the ledger, not where in it. A submitter can't
   choose to run between two other transactions. With parallel Soroban execution
   (protocol 23+), transactions whose footprints write the same entry go in
   the same cluster and run one after the other. Every settlement path
   writes `Question(id)`, so two settlements of one question never
   interleave. They are totally ordered.
3. **Footprints are fixed at submission.** The read/write set comes from
   simulation. If execution needs an entry outside it, the transaction
   fails as a whole. A stale simulation can make a transaction fail, never
   partially apply.
4. **No re-entrancy.** The host refuses to re-enter a contract already on
   the call stack, so the token can't call back into the escrow in the
   middle of a settlement. Settlement also flips the status before any
   token call (checks-effects-interactions), so correctness doesn't depend
   on this rule either.
5. **The ledger that counts is the one the transaction lands in.**
   `refund_timeout()` compares against `env.ledger().sequence()` at
   execution, not at simulation.

So an attacker's whole power over a race is **which order the competing
transactions apply in, and which ledger each lands in**, within the limits
of fees and timing. The tests enumerate exactly that. Each scenario runs
on a fresh copy of the same starting state, in **every order** of the
competing transactions, with each one landing at **deadline−1, deadline
or deadline+1** (never earlier than the one before it). After each step
they check:

- at most one settlement ever succeeds, and it's the one the model
  predicts for that order;
- every other attempt fails with the expected error and changes nothing
  (compared field by field against a snapshot taken before it);
- the winner's accounting is exact to the stroop, and the contract still
  holds exactly what it owes (unsettled escrow + owed + stakes).

## Threat catalogue

| # | race | outcome | test |
|---|---|---|---|
| 1 | `resolve()` vs `refund_timeout()` around the deadline, same or adjacent ledgers | first valid one wins, other fails `QuestionNotPending`; before the deadline `refund_timeout()` fails `TooEarlyForTimeout` whatever the order | `resolve_vs_refund_timeout_in_every_order_and_adjacent_ledger` |
| 2 | all five paths at once (two different resolves, admin refund, two timeout refunds), all 120 orders | exactly one winner, always the first to land | `every_settlement_path_racing_at_once_has_exactly_one_winner` |
| 3 | resolve / refund / refund_timeout, every order × every landing ledger across the deadline | matches the model in every case | `every_settlement_path_racing_across_the_deadline_boundary` |
| 4 | two `resolve()`s with different quorums (backend retry, two instances) | first wins; the second quorum is never credited or slashed | `two_competing_resolves_never_both_pay_out` |
| 5 | two `refund_timeout()`s (third party vs payer) | one refund, never two | `two_refund_timeouts_never_double_refund` |
| 6 | `resolve()` simulated before the deadline, landing after a `refund_timeout()` | resolve fails cleanly: no fee, no credit, no slash | `resolve_simulated_before_the_deadline_but_landing_after_a_refund_fails_cleanly` |
| 7 | losing worker front-runs `resolve()` with `unstake()` | allowed by design (staking is voluntary). Only their own slash is skipped; fee, dust and credits are exact | `losing_worker_front_running_resolve_with_unstake_only_escapes_their_own_slash` |
| 8 | `set_admin()` lands before a `resolve()` signed by the old admin | resolve fails auth with no side effects; `refund_timeout()` unaffected | `admin_rotation_landing_first_invalidates_the_old_admins_resolve` |
| 9 | two payers `submit()` the same id in one ledger | one question; the loser's transfer rolls back with their failed transaction | `competing_submits_for_one_question_id_take_only_the_winners_funds` |
| 10 | re-submitting a settled id so a stale settlement lands on the new question | impossible: `QuestionAlreadyExists`, and every late settlement fails `QuestionNotPending` | `a_settled_question_id_can_never_be_reopened_and_settled_again` |
| 11 | `charge()` vs `withdraw_balance()` on the same deposit | whichever lands first gets the funds; the deposit is never both escrowed and returned | `charge_racing_withdraw_balance_never_spends_the_same_deposit_twice` |
| 12 | `set_timeout_ledgers()` lands just before a payer's `submit()` | **residual, bounded**: the question snapshots the new value, which is capped at `MAX_TIMEOUT_LEDGERS` (7 days) and can't be applied retroactively | `raising_the_timeout_in_the_same_ledger_as_a_submit_is_bounded` |
| 13 | `execute_upgrade()` vs `refund_timeout()` | every question reaches its deadline strictly before an upgrade can execute (see [UPGRADES.md](UPGRADES.md)) | `test_upgrade.rs` exit-window tests |

The property-based fuzzer ([`src/test_fuzz.rs`](../src/test_fuzz.rs))
covers the same ground from the other direction: random sequences of every
entrypoint, with the fund-accounting invariant checked after each call.

## Gaps found and fixed

**Unbounded `timeout_ledgers` could disable the escape hatch.** Before
this change `initialize()` and `set_timeout_ledgers()` only rejected 0.
`refund_timeout()` computed `created_at + timeout_ledgers` in `u32`, and
the release profile has `overflow-checks = true`. After
`set_timeout_ledgers(u32::MAX)`, that addition overflowed and panicked for
every question submitted afterwards. `refund_timeout()` could never
succeed for them, so only the admin could refund them, which is exactly
the single point of failure the permissionless timeout exists to remove.
Fix:

- Both entry points now reject anything above `MAX_TIMEOUT_LEDGERS =
  120_960` (7 days at 5 s) with `InvalidTimeout`. That also keeps every
  timeout below the 500,000-ledger TTL a question gets on write, and
  shorter than the upgrade delay.
- The deadline is computed with `saturating_add` as defense in depth.
- Test: `a_huge_timeout_can_no_longer_disable_the_escape_hatch`.

**Settlement relied on the token not calling back.** `resolve()` and
`do_refund()` transferred tokens before writing the terminal status.
Transaction atomicity and the host's re-entrancy ban made that safe, but
only because of them. Both now write the status first.

**The contract instance's TTL was never extended.** Instance storage
(admin, token, timeout) and the contract code share a TTL that nothing
ever bumped. On mainnet that TTL starts at the network minimum (2,073,600
ledgers, about 120 days), so a live, busy contract would archive on
schedule. Every call, `refund_timeout()` included, would then need a
restore first. Every state-changing call now extends it.

## Out of scope

- **Admin key compromise.** An attacker holding the admin key can
  `resolve()` pending questions to workers they control. `refund_timeout()`
  bounds how long a payer waits on an *absent* admin, not a malicious one.
  Code upgrades by a compromised key are timelocked (see
  [UPGRADES.md](UPGRADES.md)).
- **Network-level censorship**, e.g. validators refusing to include a
  payer's `refund_timeout()`. That's outside what a contract can address.
