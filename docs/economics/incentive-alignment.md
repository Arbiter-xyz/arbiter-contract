# Incentive alignment: is 20% / 5% well tuned? (issue #117)

`PLATFORM_FEE_BPS = 2000` and `SLASH_BPS = 500` were picked, not derived.
This document models them. It gives #101 (fee-discount auction), #103
(cooperative pools), #126 and #127 one model to test their proposals
against, so they don't each argue about parameters on their own.

It builds on [slashing-threat-model.md](slashing-threat-model.md) (#8),
which already treats individual attacks in closed form (A0 to A4). This
document adds three things:

1. **Population dynamics over time.** Honest workers leave when the work
   stops paying, payers leave when answers go wrong, and a cartel learns
   whether attacking pays.
2. **A sweep over the fee, the slash rate and the quorum size together.**
3. **Contract-exact money.** Every fee, share, dust and slash in the
   simulation comes from integer code that a test proves is the same as
   `resolve()` / `slash()` in `src/lib.rs`.

## 0. Stealth-address worker identity (issue #58)

This section is a **design note only**. It answers #58's acceptance
criteria before any storage schema or `resolve()` / `credit_owed()`
change lands. Nothing below is implemented; the contract is unchanged.

### 0.1 The tension with `Owed` / `Stake`

Every worker-facing balance in `src/lib.rs` is deliberately persistent and
linkable per `Address`:

- `get_owed()` accumulates across `resolve()` calls, so a worker can
  answer many questions and `withdraw()` once (see
  `withdraw_accumulates_across_multiple_resolved_questions_before_a_single_payout`).
- `get_stake()` is a single running total, not a per-question figure.

A stealth address is, by definition, a fresh `Address` per question. It
cannot be the same `Address` that accumulated `Owed`/`Stake` from a prior
question. So a naive "one stealth address per question" scheme forces one
of two bad outcomes:

1. **Abandon accrual per stealth identity** — back to N discrete payouts,
   exactly what round 2's "streaming settlement" was built to avoid.
2. **Require workers to prove control of many stealth addresses** to
   consolidate — which, done naively, deanonymizes them in the act of
   consolidating.

This is the tension #58 asks to resolve *before* any code change. The
resolution below is a design, not a caveat.

### 0.2 Resolution: a scan-key-derived stealth address with a

consolidation proof, not a per-question payout

The scheme keeps the existing accrual model intact and adds an
unlinkability layer *on top of* it, rather than replacing it.

**Derivation.** A worker publishes a scan public key `S` (and a spend
public key `P`) once, out of band. For each question the payer derives an
ephemeral address `A_i = H(r_i · S) · G + P` (a standard stealth-address
construction), where `r_i` is a per-question nonce. The worker can detect
and spend `A_i` using its scan private key, but no observer can link `A_i`
to `S`, `P`, or to any other `A_j` without the scan private key.

**Accrual is unchanged.** `resolve()` still credits `Owed` to whatever
`Address` is in its `workers` list — now `A_i`. `credit_owed()` and the
`Stake` mechanism are not modified. The contract still sees a normal
`Address` verified by `require_auth()`. The only change is *which* address
appears in the list.

**Consolidation without linking.** The worker does not withdraw from each
`A_i` individually. Instead:

1. The worker derives a single **consolidation address** `C` from the same
   spend key `P` (e.g. `C = H("consolidate") · P`), used only for
   withdrawal.
2. For each `A_i` with non-zero `Owed`, the worker submits a withdrawal
   that pays out to `C`. The transaction authorizes `A_i` (via
   `require_auth()`), so the contract sees only "address `A_i` withdraws to
   `C`" — it never sees `S`, `P`, or the set of other `A_j`.
3. Because each withdrawal is a separate transaction, no single
   transaction links two stealth addresses. An observer watching the chain
   sees N independent withdrawals to N different (or the same) `C`
   addresses, with no on-chain proof they share an owner.

The unlinkability guarantee is therefore **transaction-level, not
address-level**: the contract never learns the mapping `{A_i} → owner`, and
no single transaction reveals it. This is the same guarantee a Monero-style
stealth address gives, applied to a Soroban `Address`.

**What this costs.** The worker pays N withdrawal transactions instead of
one. That is the price of unlinkability, and it is a *worker-side* cost,
not a contract change. The contract's `Owed` accrual, `withdraw()`
semantics, and `Stake` accounting are all untouched.

### 0.3 Open question 1: withdrawing scattered earnings without linking

Answered by §0.2: one withdrawal transaction per stealth address, each
paying to a consolidation address `C` derived from the spend key. No single
transaction contains two stealth addresses, so the withdrawal set is not
linkable on-chain. The worker can also batch withdrawals across *time*
(e.g. withdraw `A_1` in block 100, `A_2` in block 140) to defeat timing
correlation, at the cost of latency.

### 0.4 Open question 2: does staking make sense per stealth address?

**No — staking is incompatible with per-question stealth addresses as
defined here, and should be scoped to unstaked/casual participation.**

A stake is a standing, reusable bond. It is only meaningful if the same
`Address` can be slashed later for a question it answered earlier. A
one-time stealth address `A_i` is spent after one question; there is no
"later" in which it can be slashed, and no way to top it back up without
linking it to the worker's persistent identity.

So the design splits participation into two modes:

| mode | identity | stake | accrual |
|---|---|---|---|
| **Staked** | persistent `Address` | yes, standing bond | `Owed` accumulates, single `withdraw()` |
| **Stealth / casual** | per-question `A_i` | no | `Owed` accrues per `A_i`, consolidated per §0.2 |

This is in real tension with #54's category-specific stake minimums, which
assume stake is a standing, identifiable per-worker figure. #54's minimums
apply to the **staked** mode only. Stealth-mode workers are unstaked and
must be priced accordingly (e.g. lower per-question payout, or excluded
from categories that require a bond). That is a pricing decision for #54,
not a contract change here.

### 0.5 Open question 3: privacy from whom?

The design above targets **privacy from public chain observers** and from
the platform's on-chain view. It does *not* hide the worker from the
platform's off-chain backend, which must know `S`/`P` to derive `A_i` and
route questions. Concretely:

- **From public chain observers:** yes. `A_i` is unlinkable to `S`, `P`,
  or other `A_j` without the scan private key.
- **From the platform:** no, not in this design. The backend derives `A_i`
  from the worker's published scan key, so it knows the mapping. Hiding
  from the platform would require a different construction (e.g. the
  worker derives `A_i` itself and the payer only learns it at answer time),
  which is a larger change and out of scope for #58.
- **From other workers:** yes, to the same extent as from chain observers,
  since other workers only see the chain.

If the goal is privacy *from the platform*, this design is insufficient
and #58 should be reopened with that as the explicit target. The note
records this so the choice is deliberate, not accidental.

### 0.6 What is explicitly *not* landing

Per #58's acceptance criteria, no implementation lands until the tension
above has a resolution. This note provides that resolution (§0.2) and the
scoping decisions (§0.4, §0.5). The following remain **out of scope** and
unchanged:

- `resolve()`'s `workers` list handling
- `credit_owed()`
- the `Owed` / `Stake` storage schema
- `withdraw()` semantics

A future implementation PR would add the stealth-address derivation and
the consolidation-withdrawal path, but only after this note is reviewed.

## 1. The tool

| file | what it is |
|---|---|
| `sim/incentive/src/math.rs` | `mul_bps`, `slash`, `resolve`, copied from lib.rs. i128 only, `no_std`-clean |
| `sim/incentive/src/model.rs` | the agent-based model (§2) |
| `sim/incentive/src/main.rs` | the sweeps S1–S5, which write markdown |
| `src/test_incentive_math.rs` | includes `math.rs` with `#[path]` and checks it against the real contract (§1.1) |
| `docs/economics/incentive-tables.md` | generated output. Don't edit it by hand |

```sh
cd sim/incentive
cargo test                                                  # model self-checks
cargo run --release -- --out ../../docs/economics/incentive-tables.md
cargo run --release -- --seeds 4 --rounds 100               # quicker, noisier
# from the repo root: the contract-equivalence proof
cargo test test_incentive_math
```

The crate declares its own empty `[workspace]` and has no dependencies,
so building it never touches the contract build.

**Where it lives.** This addresses open question 2 of the issue. The tool
lives in this repo as a maintained tool, not as a one-off notebook,
because any future change to the fee or slash rate should re-run it. The
equivalence test is what makes keeping it here safe. If someone changes a
settlement formula in lib.rs and doesn't update `math.rs`,
`cargo test` fails. The simulation can't quietly end up modelling a
contract that no longer exists.

### 1.1 What the equivalence test proves

`src/test_incentive_math.rs`:

- `constants_match_lib_rs`: the fee, slash, cap and denominator constants
  match lib.rs.
- `mul_bps_matches`: a proptest over values up to 10³⁰ and every bps value
  from 0 to 10,000, compared with `OracleEscrow::mul_bps`.
- `resolve_matches_*`: each case runs a real `submit()` + `resolve()` in a
  Soroban `Env`, with losers' stakes built up bucket by bucket (settled,
  then unbonding through `begin_unstake`, then fresh warming). It then
  asserts three things against `sim_math::resolve()`:
  - every winner's `get_owed()` equals `share`
  - the platform's token balance changed by `fee + dust + Σ slash`
  - every loser's `(settled, warming, unbonding)` matches the simulation's
    buckets after the slash, which also covers the draining order

  The fixed cases cover:
  - no losers, over awkward amounts and quorum sizes (including amounts
    below 10,000 stroops and prime quorum sizes, so dust is non-zero)
  - slashes where the 5% rate is the binding limit
  - slashes where the per-question cap is the binding limit
  - unstaked losers
  - a mixed-bucket draining case

  A proptest (48 cases) also randomises the amount, the quorum size and up
  to four losers' buckets.

The simulation never uses floats for money. Floats appear only in
behavioural state: the adaptive cartel's payoff estimates, the payers'
memory of wrong answers, and summary statistics.

## 2. The model

One round is one batch of questions, 50 per round at full payer
confidence. For each question:

1. The backend draws `quorum` distinct active workers uniformly at random.
2. **Honest workers** answer correctly with probability 1 − e. Each pays
   its own per-answer cost *c* (API calls and compute), drawn once from
   U[0.5 c̄, 1.5 c̄].
3. **The cartel** coordinates on question ids, so it knows its seat count
   *k*. When k ≥ majority, its strategy decides whether every seat votes
   the same wrong answer. A lie costs no work. If the lie is accepted, an
   outside party pays the cartel a bribe *B* (the SchellingCoin "P + ε"
   attacker, see §6).
4. The majority wins. With probability *g* (the audit rate), a wrong
   majority is caught and resolved against the truth, so the liars become
   `losing_workers`. A tie, or an audit that finds no correct voter,
   becomes a refund.
5. `math::resolve()` computes the fee, shares, dust and slashes.
6. Caught cartel identities are banned and replaced at `sybil_cost`.
   Slashed stake is topped back up to its target. The slash counts as
   realised P&L, and the re-bond is only capital.

Between rounds:

- An honest worker whose P&L over its last 20 answers was negative leaves
  for good.
- While honest work is profitable on average, a new honest worker joins
  with probability 20% per round.
- Payer demand shrinks by `sensitivity × recent wrong-answer rate`.

**How the attacker is modelled.** This is open question 1 of the issue.
There are three cartel strategies, and none of them is the real
adversary:

| strategy | behaviour | what it's for |
|---|---|---|
| `Honest` | plays exactly like an honest worker | the counterfactual: "cartel excess" = P&L(strategy) − P&L(honest), same seeds |
| `Opportunistic` | lies every time it holds a majority | the upper bound on damage for a fixed strategy; the same attacker as model A1 in #8 |
| `Adaptive` | lies on a majority only while its learned payoff from lying (EMA) beats its learned payoff from honest play, plus 5% exploration | a cartel that stops attacking once audits make it unprofitable, then probes again later |

The simulation doesn't model several things:

- **Griefing** (A2 in #8), where the cartel attacks to get a competitor
  slashed rather than for a bribe.
- **Identity farming** timed to the warmup.
- **Cartels that learn the audit distribution.** The model assumes audits
  can't be told apart from real questions.
- **Commit–reveal evasion** (A4).

The conclusions therefore hold only for a bribe-driven attacker that
can't tell which questions are audits. A real adversary that learns more
than an EMA can only do better than `Adaptive`. That makes the
cartel-excess columns a **lower bound on attacker profit** for adaptive
play, and `Opportunistic` its fixed-strategy ceiling.

Defaults:

- 0.25 USDC questions (the smallest pricing tier)
- 10 USDC stakes
- 80 honest workers and 20 cartel workers
- quorum 5
- honest error rate e = 5%
- c̄ = 0.02 USDC per answer
- bribe B = 0.5 USDC per captured question
- 5% audits
- 1 USDC per replacement identity
- payer sensitivity 3× (a 10% wrong rate costs 30% of demand)
- 200 rounds, averaged over 8 seeds

All of these are stated assumptions, not measurements. The two that
matter most, c̄ and B, are named again next to every finding that depends
on them.

## 3. Findings

Two kinds of results appear below.

- **Analytical** findings follow exactly from the contract's formulas.
  The sweep numbers must agree with them, which makes them a useful check
  on the simulation itself.
- **Simulated** findings need the generated tables.

> **Status.** This PR adds the tool, the equivalence tests and the
> analysis below. `docs/economics/incentive-tables.md` has **not** been
> generated or committed in it yet. The sweep numbers S1–S5 come from
> running the command in §1. Wherever a claim below says "S*n* should
> show", it is a falsifiable prediction from the analytical result. It
> isn't a measured number until the tables exist. Whoever generates them
> should update this section if any prediction fails.

Notation: A is the question amount, f the fee (0.20), s the slash rate
(0.05), n the q

/* … truncated 10438 chars — edit only what you need near the top … */
