# Account abstraction compatibility (issue #73)

## Verdict

No contract-side change is needed. Every `require_auth()` call in
`src/lib.rs` (`admin.require_auth()` via `require_admin()`,
`payer.require_auth()` in `submit()`/`deposit()`/`withdraw_balance()`,
`worker.require_auth()` in `stake()`/`begin_unstake()`/
`complete_unstake()`/`withdraw()`/`withdraw_to()`) operates on a generic
`Address`. The contract never inspects an `Address` to determine whether
it is backed by a classic keypair or by a contract implementing Soroban's
`CustomAccountInterface` (`__check_auth`) — from this contract's point of
view the two are indistinguishable, and `require_auth()` is satisfied the
same way regardless.

`src/test_account_abstraction.rs` adds a regression test,
`stake_unstake_withdraw_work_unmodified_for_a_contract_address`, that runs
the full stake -> unstake -> withdraw flow with a worker `Address` backed
by a deployed contract instead of a keypair, confirming nothing on that
path special-cases the address kind.

## Simplification / out of scope

This does not deploy a full `CustomAccountInterface`-implementing
smart-wallet contract and drive a real signature ceremony (e.g. a
multisig threshold check, or the secp256r1-based passkey stand-in added
for #74) through `__check_auth`. That's genuinely Soroban-protocol
machinery living outside this contract, orthogonal to whether this
contract's own entry points special-case address kinds (they don't). The
regression test isolates exactly that claim using `mock_all_auths()`, the
harness every other test in this crate already uses.

## Open items for a future, separately-scoped issue

- Whether `sponsor.js`'s fee-bump sponsorship logic needs changes to
  submit fee-bump transactions on behalf of a smart-wallet address whose
  `__check_auth` requires multiple signers in one envelope — this is a
  backend/tooling question, not a contract one, and out of scope here.
- Any real gap found once a concrete smart-wallet contract is integrated
  end-to-end should be filed as its own issue per #73's acceptance
  criteria, rather than expanding this one.
