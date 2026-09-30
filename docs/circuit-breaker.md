# Circuit breaker (#109) and health view (#110)

## Decision: the pause is triggered off-chain

We looked at two ways to trigger the pause:

| | (a) Off-chain trigger | (b) On-chain self-trigger |
|---|---|---|
| Who decides to pause | A backend job watching `get_health()` / events calls `set_paused(true)` | The contract computes a rolling refund rate and pauses itself |
| Hot-path cost | One instance-storage read of `Paused` (instance storage is already loaded, so this adds no ledger-entry read) | A rolling-window structure read **and written** on every `submit`/`deposit`/`charge` and every refund |
| Tunability | The threshold can change without a redeploy | The threshold and window are baked into the WASM; changing them needs an upgrade |
| Fit with the codebase | Matches the existing split: the backend sets policy and the contract handles settlement (see `DataKey::Stake`'s doc comment) | Goes against that split |

**We chose (a).** The contract provides the primitive (`DataKey::Paused`,
`set_paused`, `is_paused`) and the raw signal (`get_health()`'s
`total_refunded` / `total_resolved` / `open_question_count`). The detection
algorithm is backend work and out of scope here.

(b) would be more trustless against a compromised *backend*, but it would not
protect against a compromised *admin key*. The anomaly this issue describes is
the admin issuing too many `refund()` calls. A self-pause would only stop new
deposits; it could not stop the attacker's refunds, because those pay the
original payers and cannot be redirected. What does protect payers is that
`refund()` can only ever send funds back to `question.payer`.

## What the pause gates

| Entry point | Paused? |
|---|---|
| `submit`, `submit_with_schema_hash`, `submit_with_category`, `submit_asset` | **rejected** with `ContractPaused` (400) |
| `deposit`, `deposit_asset` | **rejected** |
| `charge`, `charge_with_schema_hash`, `charge_with_category`, `charge_asset` | **rejected** (checked in `charge_for_token`) |
| `reopen_question` | **rejected** (it pulls a fresh payer transfer) |
| `resolve`, `refund`, `refund_timeout`, `sweep_timeouts` | allowed |
| `withdraw*`, `withdraw_balance*`, `begin_unstake`/`complete_unstake*` | allowed |
| `migrate_pending` / `import_question` | allowed (moves existing funds; it does not take new payer funds) |

The rule is that a pause only stops money from coming **in**. Money that is
already escrowed can always leave, so a pause can never strand funds. The test
`paused_contract_still_allows_resolve_and_refund_of_existing_questions`
covers this.

## Who can unpause, and the DoS surface

Only the admin can pause or unpause (`require_admin`). No third party can
trigger it, so an attacker cannot cause a false-positive halt through the
contract. The remaining risk is the backend's own detector raising a false
positive. That is an operational problem: the admin unpauses. It is not a new
on-chain attack surface. A compromised admin key could pause the contract to
deny service, but the same key could already stop resolving questions, and
`refund_timeout()` still bounds how long payers wait in both cases.

## Health counters (#110)

All counters live in instance storage and are updated in exactly two places:

- `record_question()` (used by `submit*`, `charge*` and `import_question`)
  and `reopen_question()`: `OpenQuestionCount += 1`, `TotalOpenedCount += 1`
- `settle_question()`, the one place a question leaves Pending (`resolve`,
  `refund`, `refund_timeout`, `migrate_pending`): `OpenQuestionCount -= 1`,
  plus `TotalResolvedCount` or `TotalRefundedCount` depending on the new
  status

The counters saturate instead of trapping, so an observability counter can
never fail a settlement. `get_health()` also returns the contract's own
default-token balance (TVL in the default asset), the admin/token/platform
addresses, and `paused`. Those addresses were already public; they can be
read directly from instance storage via RPC.

**Deployment note:** the counters start at zero when this code first runs.
On an in-place upgrade of an instance that already has Pending questions,
`OpenQuestionCount` will under-report until those questions settle. The
counter saturates at 0, so it will not underflow. `pending_count()` remains
the authoritative index-backed number.

### Suggested off-chain detector

Poll `get_health()` every N ledgers and compute the refund share
`Δtotal_refunded / (Δtotal_refunded + Δtotal_resolved)` over a sliding window.
If it goes past a threshold, call `set_paused(true)` and alert. The threshold
belongs in backend config, not here.
