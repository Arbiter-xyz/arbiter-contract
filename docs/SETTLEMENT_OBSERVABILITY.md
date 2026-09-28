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