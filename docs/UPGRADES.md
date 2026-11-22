# Upgrades and migration

## Design

Soroban contracts don't have to be immutable. A contract can replace its
own code with `update_current_contract_wasm(hash)`, and when it does, its
**address, storage and token balance stay exactly where they are**. The
escrow uses that, behind a timelock:

| entrypoint | who | effect |
|---|---|---|
| `propose_upgrade(new_wasm_hash)` | admin | schedules the upgrade for `now + UPGRADE_DELAY_LEDGERS`; emits `upgrade_proposed`. Proposing again replaces it and restarts the delay |
| `cancel_upgrade()` | admin | drops the proposal; emits `upgrade_cancelled` |
| `execute_upgrade()` | admin | once the delay has passed, swaps the code; emits `upgrade_executed`. The new code runs from the next call |
| `get_pending_upgrade()` | anyone | the scheduled hash and `executable_at`, or none |
| `version()` | anyone | `CONTRACT_VERSION` of the running code |

### Why not a proxy or a migration contract

Both move funds or authority between two contracts, and that creates a
window where a question could be claimed from both, or from neither.
Guarding against that means a per-question handoff protocol that stays
correct when interrupted halfway. An in-place upgrade avoids the problem
entirely. There is only ever **one** contract, so there's no second place
a question could be settled, and nothing is copied that could be
duplicated or lost. Each question keeps its status, amount, deadline and
escrowed funds through the upgrade, and the existing `Pending` check
keeps settlement to exactly once.

### Why the timelock, and why that length

Upgrade authority is the most powerful key there is: new code can do
anything with every balance the contract holds. Without a delay, a stolen
admin key could drain it in one transaction. The timelock turns that into
a public, cancellable announcement with an exit window:

- `UPGRADE_DELAY_LEDGERS = MAX_TIMEOUT_LEDGERS + 17_280` (8 days at 5 s),
  and a compile-time assertion keeps it strictly longer than any question
  timeout.
- **Questions pending when the upgrade is proposed** reach their
  `refund_timeout()` deadline while the old code is still running, so any
  payer who doesn't trust the new code can take a full refund first.
- **Questions opened after the proposal** get their timeout shortened by
  `open_question()`, so their deadline lands no later than
  `executable_at - 1`. Once no window is left, `submit()`/`charge()` fail
  with `UpgradeInProgress` until the upgrade executes or is cancelled.
- **Stakes, owed earnings and prepaid balances** can be withdrawn at any
  time anyway.

So at the moment new code starts running, every question still pending
has already had its refund deadline pass under the old code. Nobody's
funds depend on trusting the upgrade. They only depend on noticing it,
which is what the event and `get_pending_upgrade()` are for. The trade-off
is that a fix takes 8 days to ship. In an emergency the admin can still
stop the bleeding right away: `refund()` every pending question and stop
routing new ones.

### Interrupted and failed upgrades

- **Uploaded but never proposed, or proposed and then abandoned:** the
  running code is untouched and fully functional. A proposal just sits
  there until it's cancelled or replaced.
- **`execute_upgrade()` fails** (hash never uploaded, or archived): the
  call removes the proposal, then the host rejects the code swap, and the
  whole transaction rolls back. The proposal is still there, the old code
  still runs and every question still settles.
  (`a_failed_upgrade_is_fully_rolled_back`,
  `upgrading_to_a_hash_that_was_never_uploaded_leaves_v2_running`.)
- **The code swap is a single transaction.** Nothing exists partway
  between "all old code" and "all new code".

### Storage compatibility: the one real risk

The new code reads the old code's storage. `#[contracttype]` structs
encode as maps keyed by field name, and enums as vectors tagged by variant
name. So renaming or retyping a field or variant makes every existing
entry undecodable, and every question with it. Nothing at runtime catches
that; `storage_layout_is_pinned` in
[`src/test_upgrade.rs`](../src/test_upgrade.rs) does. It pins the exact
XDR of every persisted type: each `DataKey` variant, `Question`, `Status`
and `PendingUpgrade`.

Rules for the next version:

1. Never rename, retype, reorder-by-renaming or remove anything the pin
   test covers. **Adding** `DataKey` variants, error codes or entrypoints
   is fine.
2. New per-question data goes in a new key (for example
  `AssetKey::QuestionToken(u64)`), not in new `Question` fields. Or add a new
   versioned type that the code reads with a fallback to the old one.
3. Migrations must be lazy: read the old format, write the new one when
   touched. An eager migration pass would need a window where un-migrated
   data blocks settlement, and `refund_timeout()` must never be blocked.
4. The new version must keep `propose_upgrade` / `cancel_upgrade` /
   `execute_upgrade` / `get_pending_upgrade` / `version`. Code without them
   can never be upgraded again. Bump `CONTRACT_VERSION`.
5. If a pin genuinely has to change, write the migration plan in this
   file first, then update the pinned value.

Question schema commitments follow rule 2: the persisted `Question` keeps its
original pinned shape and has no `schema_hash` field. The optional 32-byte hash
is stored separately under `QuestionSchemaHash(u64)` and read back with
`get_question_schema_hash()`; for questions opened before this field existed,
it returns `None`.
The contract treats the hash as opaque and does not fetch or validate a schema.

### Worked example

`worked_example_v2_to_v3_with_pending_questions` runs on real WASM. v2 is
the deployable build; v3 is the same source built with `--features
upgrade-test-v3`, so `version()` returns 3. The test:

1. deploys v2 and creates: a resolved question (owed carried across), a
   pending `submit()`, a pending `charge()` from a prepaid deposit, a
   staked worker, and a question that later escapes by timeout;
2. proposes v3 (uploaded), opens one question during the delay and one
   close to `executable_at` (clamped to a 9-ledger window). The escaping
   question's payer calls `refund_timeout()` under v2; an early
   `execute_upgrade()` is rejected;
3. executes. `version()` is 3 at the same address, and every question,
   owed balance, stake, prepaid balance and the token balance are
   identical;
4. under v3: resolves (with a slash), admin-refunds, resolves the
   mid-delay question and timeout-refunds the clamped one. A second
   settlement of any question fails;
5. drains: every worker withdraws, the staker unstakes, the payer
   withdraws the remaining balance. The contract ends at **0**, total
   supply is unchanged, and every party's balance matches the expected
   fee, dust, slash and share math to the stroop.

```sh
scripts/build-wasm.sh
cargo test worked_example -- --ignored --nocapture
```

## Runbook: testnet

Prerequisites: `stellar` CLI; an identity for the admin (`stellar keys
generate admin --network testnet --fund`); `CONTRACT` set to the escrow's
contract id.

```sh
# 1. Build and pin the artifact you reviewed.
git checkout <release-tag>
stellar contract build
shasum -a 256 target/wasm32v1-none/release/oracle_escrow.wasm   # == the wasm hash

# 2. Pre-flight: the full suite, WASM tests included, must be green.
#    Run each step separately and stop on the first failure. Do not chain
#    with `&&`: a broken `cargo test` compile (e.g. the known 194 compile
#    errors on some branches) must fail the gate loudly, not be masked by
#    an earlier step's exit code. The `--ignored` run is the WASM upgrade
#    suite and only makes sense once the plain suite compiles and passes.
scripts/build-wasm.sh
cargo test
cargo test -- --ignored

# 3. Upload the code. This prints the wasm hash; it must match step 1.
HASH=$(stellar contract upload --network testnet --source admin \
  --wasm target/wasm32v1-none/release/oracle_escrow.wasm)

# 4. Check the running version and that nothing is already scheduled.
stellar contract invoke --network testnet --source admin --id "$CONTRACT" -- version
stellar contract invoke --network testnet --source admin --id "$CONTRACT" -- get_pending_upgrade

# 5. Propose. Note executable_at and announce it (hash, source tag, date).
stellar contract invoke --network testnet --source admin --id "$CONTRACT" \
  -- propose_upgrade --new_wasm_hash "$HASH"

# 6. Wait out the timelock, then execute.
stellar contract invoke --network testnet --source admin --id "$CONTRACT" -- execute_upgrade
stellar contract invoke --network testnet --source admin --id "$CONTRACT" -- version   # == new CONTRACT_VERSION
```

If step 2 fails to compile, the gate is doing its job: the branch is not
release-ready. Fix the compile errors before proceeding; do not skip the
step or treat a red suite as advisory.
