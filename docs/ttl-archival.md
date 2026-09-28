# What happens when a Pending question's storage archives

Issue #7. Everything below was measured, not assumed. The tests named in
each section reproduce it: [src/test_ttl.rs](../src/test_ttl.rs) for the
host-level behaviour, and `testnet_fork_*` for a fork of real testnet state.
Numbers come from Stellar testnet's live settings on 2026-09-24, protocol 28:

| setting | value | ≈ at 5 s/ledger |
|---|---|---|
| `min_persistent_ttl` | 120,960 ledgers | 7 days |
| `max_entry_ttl` | 3,110,400 ledgers | 180 days |

## Short answer

**Payers' funds are never stranded by archival on protocol 23+.** On v0.2.0,
though, recovering them could cost more than they're worth, and v0.2.0 was
archiving much sooner than its code comments claimed. v0.3 fixes both.

1. **Archived persistent entries are not deleted.** Since protocol 23, the
   next transaction that touches an archived entry restores it
   automatically, with no separate restore operation. `refund_timeout()`
   stays permissionless end to end: anyone can call it on an archived
   question, the host restores the question (and the contract instance and
   code, if those archived too), and the payer is paid.
   Tests: `legacy_archived_pending_question_still_refunds_via_auto_restore`,
   `current_archived_pending_question_refunds_via_auto_restore`,
   `archived_question_can_still_be_resolved_by_the_backend`.
2. **The tests prove a restore actually happened.** They don't just check
   that the call succeeded. `disk_read_entries` on the invocation counts
   entries read back from the archive. It is ≥ 3 for the archived refund
   and exactly 0 for the same call on a live question
   (`a_live_question_refund_reads_nothing_from_disk`).
3. **A failed call rolls its restore back.** A question that archived
   *before* its deadline can't be restored early. A `refund_timeout()` that
   returns `TooEarlyForTimeout` reverts, restore included, and at the
   deadline the call restores and pays
   (`legacy_question_archived_before_its_deadline_is_still_refundable_at_the_deadline`).
4. **Restoring costs something, and on v0.2.0 the cost could exceed the
   question.** Modelled fees for `refund_timeout()`, from
   `cargo test restore_cost -- --nocapture` (SDK pubnet fee snapshot, in
   XLM):

   | state | fee | of which rent |
   |---|---|---|
   | nothing archived | 0.0055 | 0 |
   | only this question (and its payer's balance) archived, contract in use | 0.196 | 0.180 |
   | dormant v0.2.0 contract: question, instance **and** ~22 KB code archived | **25.5** | 25.49 |

   The last row dominates because protocol 23 charges rent on a code
   entry's *in-memory* module size, and a dormant contract has to restore
   its code before anything can run. For a 0.25 USDC question, whoever
   rescues the payer pays about 100× the refund. The funds are technically
   recoverable but economically stranded, until someone decides to pay
   the one-off restore for everyone.

## What v0.2.0 actually did

These were measured on the deployed v0.2.0 Wasm
([fixtures/oracle_escrow_v0.2.0.wasm](../fixtures/oracle_escrow_v0.2.0.wasm))
under testnet's settings:

- **Questions lived ~7 days, not ~29.** v0.2.0 calls
  `extend_ttl(100_000, 500_000)`, which only extends an entry whose
  remaining TTL is ≤ 100,000. A fresh entry starts at
  `min_persistent_ttl` = 120,960, so the call does nothing. A question's
  TTL was 120,959 ledgers, not 500,000
  (`legacy_extend_ttl_was_a_no_op_on_a_fresh_question_under_testnet_settings`).
  The same no-op applied to every first write of a stake, owed balance or
  prepaid balance.
- **Nothing ever extended the instance or code.** They counted down from
  deploy regardless of traffic
  (`legacy_instance_ttl_is_never_extended_however_busy_the_contract_is`).
  A v0.2.0 deployment that nobody extends by hand
  (`stellar contract extend`) archives its instance about 7 days after
  deploy. Every call after that pays the dormant-contract restore above
  once, then the instance lives another `min_persistent_ttl`.
- **A long timeout could archive before its own refund window.** With
  `timeout_ledgers` above ~120k, the question archived before
  `refund_timeout()` could even be called
  (`legacy_question_with_a_long_timeout_archives_before_it_can_be_refunded`).

## What v0.3 changes

| change | why | test |
|---|---|---|
| Threshold is now `EXTEND_TO − 1 day` instead of a fixed 100,000 | Every write actually reaches the 500k target whatever the network's `min_persistent_ttl`. | `fresh_question_ttl_now_reaches_the_persistent_target` |
| Instance and code are extended on every state-changing call | A contract in use can't go dormant. Only one that nobody at all has touched for ~29 days needs the expensive restore. | `instance_ttl_is_extended_by_ordinary_use` |
| A Pending question is kept live until `deadline + 7 days` (at least 500k ledgers, at most the network max) | `refund_timeout()` never has to restore anything within a week of the deadline, however long the timeout. | `question_ttl_covers_its_refund_deadline_plus_grace_even_for_long_timeouts`, `question_ttl_is_clamped_to_the_network_max_for_absurd_timeouts` |
| Pending-index entries get the same TTL | Enumeration (#9) can't be broken by a stale index entry. | `pending_index_entries_live_as_long_as_the_question_needs` |
| `touch_question(id)`, permissionless | Keep-alive for a question whose timeout exceeds `max_entry_ttl`, or for a settled record someone wants readable. Needs no signature. | `touch_question_is_permissionless_and_re_extends_a_decayed_entry` |
| `touch(address)` now also covers the prepaid `Balance` | A payer who deposits once and goes quiet was in the same position as a worker who earns once. | `touch_now_also_covers_a_payers_prepaid_balance` |

## Operational guidance

- **Backend sweep:** once a day, call `touch_question` on questions older than
  `PERSISTENT_TTL_EXTEND_TO − 2 days` that you still care about. Call `touch`
  on workers with a nonzero owed balance or stake and on payers with a
  nonzero prepaid balance. Each call is cheap when there's nothing to
  extend: the threshold makes it a no-op within a day of the last extension.
- **If the whole contract ever does go dormant:** run
  `stellar contract restore --id <contract>` (or just make any call). One
  restore brings back the instance and code for everyone, and later
  per-question refunds cost ~0.2 XLM, not ~25.
- **Before protocol 23** (not relevant on testnet or mainnet today): archived
  entries needed an explicit `RestoreFootprintOp`, which was also
  permissionless. No version of this contract relies on anything that can't
  be restored by anyone.

## How the tests exercise real archival

The SDK's test environment runs the same `soroban-env-host` the network
runs, with the ledger's TTLs tracked per entry. Advancing the ledger past
an entry's `live_until` really archives it. The host then treats the next
access exactly as protocol 23 does: persistent entries are auto-restored
and charged as disk reads, temporary entries are gone. The tests run the
real v0.2.0 Wasm, not a Rust re-implementation, so its code entry archives
and restores too.

The fork test goes further. It loads a snapshot of **real testnet ledger
state**: the deployed Wasm, instance, token and a real Pending question
created by the migration e2e run. It then advances past every TTL in that
snapshot and calls `refund_timeout()`. See the "Fork test" section of
[docs/migration.md](migration.md) for how the snapshot was taken.

What can't be done in a test run is wait out 120,960 real ledgers on
testnet. No network setting lets an entry archive sooner, so live-network
archival is established through the host code the network runs plus the
forked real state, not by waiting a week.
