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

# Cross-chain settlement bridge (issue #71)

## Verdict

Documentation only. No contract code changes land until the
bridge-to-contract interface (what actually calls `submit()`/`charge()`,
and from what address) is settled. This note names the rail, traces a
payer's funds from origin chain through to `open_question()`, and states
the refund path for a bridged payer.

## Chosen rail: Circle CCTP + a Stellar forwarder

CCTP is the fit here. The escrowed asset is USDC, and CCTP moves native
USDC 1:1 by burning on the origin chain and minting on Stellar — no
wrapped representation, no third-party validator set beyond Circle's
attestation service. Axelar GMP is the alternative (general
message-passing, broader chain coverage), but it introduces a validator
set and a different trust/latency model for what is, in this contract, a
single-asset transfer. CCTP's `receiveMessage` on Stellar already needs a
forwarder contract to deliver to a recipient that is not an EOA, which is
exactly the hook this pattern needs.

## Fund trace: origin chain -> `open_question()`

1. Payer holds USDC on an origin chain (e.g. Base) and calls CCTP's
   `depositForBurn` with `mintRecipient` set to the **forwarder contract's
   Stellar address** and a `hookData` payload naming the payer's Stellar
   address and the target question parameters.
2. Circle attests the burn; the attestation is submitted to Stellar's CCTP
   `MessageTransmitter`, which mints USDC to the forwarder and invokes the
   forwarder with the `hookData`.
3. The forwarder calls `token::Client::transfer` to move the minted USDC to
   the payer's Stellar address (or holds it and calls `submit()` directly
   — see the open interface question below).
4. `submit()` (or `charge()`) runs as today: `payer.require_auth()` is
   satisfied by the forwarder acting on the payer's behalf, and
   `token::Client::transfer` moves USDC into escrow. `open_question()` is
   then called with the resulting `Question`, exactly as in the native
   path.

## Refund path for a bridged payer

This is the part the happy path hides. `do_refund()` today only ever calls
`token_client.transfer` on Stellar — it has no notion of an origin chain.
So a bridged payer's refund is **Stellar-native**: `refund()` and
`refund_timeout()` pay the `Question.payer` of record on Stellar, and the
bridged payer must be able to receive USDC there. Bridging the refund back
to the origin chain is explicitly out of scope for this issue — it would
require `do_refund()` to emit a CCTP burn and a second attestation round,
which is a much larger change than the forwarder pattern above.

## Open questions

1. **Does the bridged payer need their own Stellar address as
   `Question.payer` of record?** Yes, under this design. `refund()` and
   `refund_timeout()` pay `Question.payer` on Stellar, so the payer of
   record must be a Stellar address the payer controls (or a forwarder
   acting for them). Making the origin-chain address the payer of record
   would force `do_refund()` to bridge back, which is out of scope.
2. **CCTP vs Axelar GMP?** CCTP for native USDC, as above. Axelar GMP is
   the better fit only if the escrowed asset or the refund path needs
   general message-passing across chains this contract does not currently
   touch.

## Interface still to settle (blocks code)

- Does the forwarder call `submit()`/`charge()` itself (acting as the
  authenticated payer), or does it transfer to the payer's Stellar address
  and let the payer call `submit()`? The former is closer to how `charge()`
  already lets the admin open a question funded by a prior `deposit()`
  without a per-question payer signature; the latter keeps the payer as the
  direct caller.
- What address is `Question.payer` in each case, and does that address
  match what `refund()`/`refund_timeout()` will pay?

No contract code changes land until these are settled.

# Stealth-address worker identity (issue #58)

## Verdict

Documentation only. No storage schema, `resolve()`, or `credit_owed()`
changes land until the consolidation tension below has an actual
resolution. This note states the tension, picks a resolution, and answers
the three open questions from the issue.

## The tension with the existing `Owed`/`Stake` accrual model

Every worker-facing balance in `src/lib.rs` is deliberately persistent and
linkable per `Address`:

- `get_owed()` accumulates across `resolve()` calls, so a worker can answer
  many questions and `withdraw()` once (see
  `withdraw_accumulates_across_multiple_resolved_questions_before_a_single_payout`).
- `get_stake()` is a single running total, not a per-question figure.

A stealth address is, by definition, a fresh `Address` per question. It
cannot be the same `Address` that accumulated `Owed`/`Stake` from a prior
question. So passing a per-question stealth address into `resolve()`'s
`workers` list forces one of two bad outcomes:

1. **Abandon the accrual model per stealth identity** — each stealth
   address holds its own `Owed`, and the worker is back to N discrete
   payouts, exactly what round 2's "streaming settlement" was built to
   avoid.
2. **Require the worker to later prove control of many stealth addresses
   to consolidate** — which is the consolidation problem this note must
   solve without deanonymizing the worker in the act of consolidating.

## Resolution: consolidate via a worker-controlled accumulator, not by
linking stealth addresses on-chain

The resolution is to keep `Owed`/`Stake` keyed by the **persistent worker
address** and never let a stealth address hold a balance at all. A stealth
address is used only as the *authorization* for a `resolve()` answer; the
credit is routed to the worker's persistent accumulator in the same call.

Concretely, the design (not implemented here) is:

- The worker publishes a **scan key** and a **spend/accumulator address**
  (the persistent `Address` that already holds `Owed`/`Stake`).
- For each question the worker derives a one-time stealth `Address` from
  the scan key. That stealth address is what appears in `resolve()`'s
  `workers` list and what satisfies `worker.require_auth()` — Soroban
  treats it like any other `Address`.
- `credit_owed()` credits the **accumulator address**, not the stealth
  address. The stealth address is a proof-of-participation token, not a
  balance holder. This preserves the existing accrual model exactly:
  `get_owed()` still accumulates across `resolve()` calls on one
  persistent `Address`, and `withdraw()` is still a single payout.
- The link between a stealth address and the accumulator is proven
  **off-chain** (the worker signs a statement binding the stealth address
  to the accumulator) and is never written to contract storage, so the
  on-chain record does not link the two.

This is the "actual resolution" the acceptance criteria require: the
accrual model is not abandoned, and consolidation does not require the
worker to reveal a set of stealth addresses in a withdrawal transaction.

## Open question 1: withdrawing scattered earnings without linking addresses

Under the resolution above, earnings are **not scattered** — they accrue
on the single accumulator address. The worker calls `withdraw()` once on
the accumulator, exactly as today. No withdrawal transaction ever names a
stealth address, so there is nothing to link in the withdrawal tx. The
unlinkability lives in the *answer* path (each `resolve()` sees a fresh
stealth address), not the *payout* path (one persistent accumulator).

If a future design instead lets stealth addresses hold balances, the
consolidation step would have to be a privacy-preserving proof (e.g. a
zero-knowledge set-membership proof that the accumulator controls N
stealth addresses) rather than a transaction that names them. That is a
much larger change and is explicitly not the chosen path here.

## Open question 2: does staking make sense per stealth address?

No. `Stake` is a standing, reusable bond — a single running total per
worker, and #54's category-specific stake minimums assume stake is an
identifiable per-worker figure. A per-question stealth address cannot hold
a standing bond without either fragmenting the bond across identities or
re-linking them. So staking stays on the **persistent accumulator
address**; stealth addresses apply only to unstaked/casual participation,
or to staked workers who still answer under a stealth address while their
bond remains on the accumulator. This is the point of real tension with
#54 and is called out here rather than papered over.

## Open question 3: privacy from whom?

This design targets **privacy from public chain observers** (and, as a
consequence, from other workers): an observer cannot link the fresh
address in each `resolve()` to the worker's persistent accumulator. It
does **not** provide privacy from the platform, because the platform sees
the off-chain binding proof between the stealth address and the
accumulator in order to route credit. If the goal were privacy from the
platform, the binding proof would have to be verified on-chain (a
zero-knowledge proof), which is a different and larger design. Stating
this explicitly is required by the acceptance criteria.

## What still blocks implementation

- The off-chain binding proof format (what the worker signs to bind a
  stealth address to the accumulator) must be specified before any
  `resolve()`/`credit_owed()` change.
- Whether the platform can be trusted to route credit to the accumulator
  without seeing the binding, or whether an on-chain proof is required,
  depends on the answer to open question 3 above.

No contract code changes land until these are settled.
