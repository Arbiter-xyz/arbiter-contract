# Settlement observability before confirmation

## Findings

There is no confidentiality boundary for settlement inputs or relevant
contract state. A payer, worker, RPC operator receiving a simulation or
submission, and validators that receive a transaction may inspect its
arguments. Stellar does not provide a universal private-mempool guarantee;
exact pending-transaction visibility varies by RPC provider and network
path. Assume the submitter and any RPC or validator handling the transaction
can see it before ledger inclusion.

Soroban simulation is an execution preview, not a private oracle. The
caller-provided `resolve` question id, matching workers, losing workers,
and authorization data are available to the simulation service. Simulation
responses expose transaction/resource/auth information, and RPC versions
may include diagnostic events or additional execution details. A caller
should not rely on a simulated result being hidden from the RPC operator.

## What can be inferred

The payout distribution is fully derivable from already-public inputs and
state. `get_question()` exposes the question amount, and public getters
expose worker stake and accrued balances. `resolve()` supplies both worker
lists as transaction arguments. The contract uses a 20% fee, divides the
remaining pool evenly among matching workers, sends integer-division dust
to the platform, and slashes each losing worker by 5% of that worker's
asset-specific slashable stake up to the question amount. These values are
therefore not secret before settlement; a third party can calculate them
without simulation.

The successful `resolve()` return value is empty. The escrow's settlement
event reports question id and terminal status, not worker shares or exact
stake. Token transfer events and the resulting `get_owed*` / stake state
make actual settlement observable after inclusion. In execution order,
`resolve()` updates credit/slash storage, calls the token to transfer the
platform amount, then records the terminal question status and emits
`question_settled`; a token transfer event therefore precedes the escrow's
settlement event. These changes and events commit atomically. A simulation
service may expose the corresponding diagnostic events before inclusion,
but they contain no private information absent from the public inputs and
state. Resource use can reveal list sizes, which are also in the call.

## Leak assessment and response

No information leak beyond intentionally public transaction arguments and
contract state was identified. In particular, an observer does not need a
timing or state-observation side channel to infer exact stake or payout:
the stake getters, question amount, worker lists, and deterministic integer
arithmetic already disclose them. This is an accepted protocol property,
not a confidentiality guarantee.

No mitigation is added because hiding these values would require changing
the public settlement inputs/state model, not suppressing an event. If
stake or payout privacy becomes a product requirement, the design needs a
confidential computation/commitment scheme and a separate threat model.
Until then, backend operators should treat simulations and unconfirmed
transactions as public to the RPC provider and network participants that
receive them.

During this audit the settlement path also had two arithmetic/accounting
issues independent of observability: the computed platform share was not
transferred, and direct `i128 * bps` could overflow on very large amounts.
Resolution now transfers the per-question platform amount in its bound
asset and uses overflow-safe integer basis-point calculation.

## Cross-chain settlement bridge (design note, #71)

This note is documentation only. No contract code changes land until the
bridge-to-contract interface (what actually calls `submit()`/`charge()`, and
from what address) is settled, per the acceptance criteria for #71.

### Chosen rail: Circle CCTP with a Stellar forwarder

We name Circle CCTP (native USDC) plus a forwarder contract as the rail for
"pay from another chain". CCTP is preferred over Axelar GMP here because the
escrowed asset is USDC and CCTP moves native USDC 1:1 with no wrapped-asset
or liquidity-pool risk; its trust model is the issuer's attestation service
rather than a general validator set, and its latency (source finality plus
attestation) is predictable. Axelar GMP is a better fit for arbitrary
cross-chain *messages* and heterogeneous assets, but it introduces a
validator-set trust assumption and message-execution semantics we do not
need for a single-asset escrow. CCTP already requires a forwarder contract
for Stellar recipients, which matches `lib.rs`'s existing shape: the
forwarder, not the origin-chain payer, is the Stellar caller.

### Fund trace: origin chain to `open_question()`

1. Payer holds USDC on an origin chain and initiates a CCTP `depositForBurn`
   targeting Stellar, with the mint recipient set to the forwarder contract
   address and a payload identifying the intended escrow action.
2. Circle's attestation service observes the burn and produces an
   attestation; the message is relayed to Stellar.
3. CCTP mints native USDC to the forwarder contract on Stellar.
4. The forwarder calls the escrow. For a direct question it calls `submit()`
   with the payer's designated Stellar address as `payer`; for the
   admin-funded path it calls `charge()` against a prior `deposit()`. In both
   cases the forwarder is the transaction source, but the `payer` of record
   is the Stellar address the payer designated.
5. `submit()`/`charge()` run unchanged: `payer.require_auth()` is satisfied
   by the designated Stellar address's authorization, and the escrow's
   `token::Client::transfer` moves the now-Stellar-native USDC into escrow.
6. `open_question()` records the question with that Stellar address as
   `Question.payer`.

### Refund path for a bridged payer

A bridged payer must designate their own Stellar address as the
`Question.payer` of record. `refund()` and `refund_timeout()` call
`do_refund()`, which today only ever calls `token_client.transfer` on
Stellar; there is no bridge-back in the settlement path. Consequently a
refund pays the designated Stellar address in native USDC, and bridging that
USDC back to the origin chain is the payer's own responsibility, outside the
contract. Bridging refunds back to the origin chain is explicitly out of
scope for #71: it would require `do_refund()` to emit a CCTP burn intent and
coordinate with the forwarder, a much larger change than the forwarder
pattern above. If a payer does not designate a Stellar address they control,
the refund is unrecoverable by them, so the forwarder must require and
validate that address before calling `submit()`/`charge()`.

### Open questions resolved

1. The bridged payer does need their own Stellar address as
   `Question.payer` of record, so `refund()`/`refund_timeout()` can pay them
   back on Stellar. Refunds are not bridged back to the origin chain in this
   design; that is deferred as larger scope.
2. CCTP with a forwarder fits "pay from another chain" best for this
   single-asset USDC escrow, given its 1:1 native-USDC movement and
   issuer-attestation trust model. Axelar GMP remains the option if the
   escrow later needs arbitrary cross-chain messages or non-USDC assets.

### Interface still to settle before code

The forwarder-to-escrow interface is not yet frozen: whether the forwarder
calls `submit()` directly or routes through `charge()` after a `deposit()`,
and the exact address that appears as `payer`, must be agreed before any
contract change. Until then this remains a design note only.
