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
(0.05), n the quorum size, e the honest error rate, and S a worker's
stake.

### F1. SLASH_BPS is mostly inert: for realistic stakes, the per-question cap decides the penalty (analytical)

`slash = min(s·S, A, S)`, so the cap binds once S ≥ A / s = **20·A**:

| question | stake at which SLASH_BPS stops mattering |
|---|---|
| 0.25 USDC | 5 USDC |
| 1 USDC | 20 USDC |
| 5 USDC | 100 USDC |

At the default 10 USDC stake on 0.25 USDC questions, every slash is
already exactly 0.25 USDC. Raising SLASH_BPS from 5% to anything higher
changes nothing for that worker. Lowering it changes nothing either,
until it drops below 2.5%.

S2 runs at 100 USDC stakes, where the cap binds from 25 bps upward.
**S2 should show flat rows from 25 bps to 2000 bps.** Only workers staked
below 20·A feel SLASH_BPS at all, and the backend's matured-stake gate
nudges workers to stake above that level.

**So the parameter that actually sets the penalty is
`SLASH_CAP_BPS_OF_AMOUNT` (100%), not `SLASH_BPS`.** Any proposal to "tune
slashing" (#126, #127) should be expressed as a change to the cap. A
change to SLASH_BPS only affects low-stake workers.

### F2. Slashing charges honest workers a large, quorum-dependent error tax (analytical)

With the cap binding, an honest worker who answers wrong is out-voted
almost every time. At e = 5% and n = 5, P(the other 4 have a correct
majority) = 0.986. Each such loss costs A. When the worker is right, it
earns about (1−f)·A/n. So:

    slash tax  ≈ e·n / ((1−e)(1−f))        (fraction of gross earnings lost to slashing)
    margin     ≈ A·[(1−e)(1−f)/n − e] − c   (per answer, first order: ignores splits with wrong peers)

| quorum n | slash tax (e=5%, f=20%) | margin before cost, A = 0.25 |
|---|---|---|
| 3 | 19.7% | 0.0508 USDC |
| 5 | **32.9%** | **0.0255 USDC** |
| 7 | 46.1% | 0.0146 USDC |
| 9 | 59.2% | 0.0086 USDC |

At today's settings, **about a third of an honest 5%-error worker's gross
earnings go back to the platform as slashes.** This is larger than the
platform fee's effect on the same worker. The tax grows linearly with the
quorum size, because the upside is split n ways and the downside isn't.

**S1 and S4 should show these tax levels** in their `honest slash tax`
column. The simulation computes splits exactly, so small deviations from
the first-order figures are expected.

This isn't necessarily a bug. Setting the margin (before cost) to zero
gives the error rate above which a worker loses money even at zero cost:

    e* = (1−f) / (n + 1 − f)   →   13.8% at n = 5,  21% at n = 3,  10.3% at n = 7

So the capped slash works as a **quality screen**: it prices out workers
who are wrong more than ~14% of the time at n = 5. That is the part of
"deterring bad answers" that slashing actually does. Section F3 covers
the part it doesn't do.

### F3. Slashing on consensus can't deter a majority cartel; audits and bans do (analytical, S1/S5 to quantify)

This was already argued in #8 (§4). The model makes it concrete. When the
cartel holds a majority, its members are the `workers` and the honest
minority is `losing_workers`. Every consensus slash therefore lands on
the people resisting the attack.

With no audits (S5, first row), raising SLASH_BPS **cannot** reduce
cartel excess and **must** raise the honest slash tax. S5's row at 0%
audit should be flat across slash rates, apart from the honest-minority
losses.

With audits, a caught identity loses its share, one capped slash (≤ A)
and its identity (`sybil_cost`). Worked example at the defaults (k = 3 of
n = 5, B = 0.5, A = 0.25):

- **Payoff when not caught:** W ≈ B + 0.8A − k·0.8A(1−e)/n ≈ 0.586 USDC
- **Loss when caught:** D ≈ 0.114 (forgone honest pay) + **0.75** (slash, 3 × A) + **3.0** (3 replacement identities)
- **Break-even audit rate:** g* = W / (W + D) ≈ **13%**. Without the slash term it would be ≈ 16%.

The slash moves the break-even audit rate by about 2 percentage points.
The ban/identity cost does most of the work, and the slash is secondary.
Because of F1, raising SLASH_BPS wouldn't increase that 0.75 anyway. Only
raising the *cap* would.

**S5 should show** the following:

- At 5% audits (below g*), the opportunistic cartel is still profitable.
- At 25% audits, cartel excess turns negative at every slash rate.
- Between 10% and 25% audits, the column-to-column spread caused by the
  slash rate is small compared with the effect of the audit rate itself.

### F4. The 20% fee is not the binding constraint on retention at the 0.25 USDC tier; quorum size is (analytical, S3/S4 to quantify)

Margin before cost at A = 0.25, n = 5, e = 5%. The last column is the
share of workers who can't break even with c ~ U[0.01, 0.03]:

| fee | margin before cost | workers who exit |
|---|---|---|
| 10% | 0.0303 | ~0% |
| 15% | 0.0279 | ~11% |
| **20% (current)** | **0.0255** | **~23%** |
| 25% | 0.0231 | ~34% |
| 30% | 0.0208 | ~46% |
| 40% | 0.0160 | ~70% |
| 50% | 0.0113 | ~94% |

Each 5-point change in the fee moves the margin by ≈ A(1−e)·0.05/n =
**0.0024 USDC per answer**. Changing the quorum from 5 to 7 moves it by
0.011, which is **about 4.5× more** than any single 5-point fee step.

The "workers who exit" column depends entirely on the assumed cost
distribution. With c̄ = 0.01 instead of 0.02, nobody exits below a 40%
fee. S3 turns this into retention, served demand and revenue. **S3 should
show** platform revenue per round rising with the fee up to the point
where exits push the active pool toward the quorum size. After that, the
`served` column drops and revenue falls. Where that turning point sits is
the real answer to "is 20% sustainable", and it moves with c̄.

### F5. Revenue stability (simulated only)

Per-round platform revenue is fee + dust + slashes. Slash revenue is
noisy: it spikes with cartel attacks that get audited, and with honest
error clusters. Fee revenue is proportional to served demand. The
`rev CV` column in S1 and S3 measures this. No closed form is claimed
here. Read it from the tables.

## 4. Recommendations

**PLATFORM_FEE_BPS = 2000: keep it.** Nothing in the analysis shows the
fee is what limits honest retention at the 0.25 USDC tier. Quorum size
and the capped slash tax (F2) matter more. Revisit the fee only if S3,
run with a cost distribution measured from real worker data, shows exits
starting below 25%.

**SLASH_BPS = 500: fine, because it barely matters (F1).** Don't raise it:
that adds honest error tax on low-stake workers and does nothing against
a majority cartel (F3). The lever with real effect is
`SLASH_CAP_BPS_OF_AMOUNT`, which trades the honest error tax (F2) against
the screening threshold e* and the caught-cartel penalty (F3). Any change
there should come with S1/S2/S5 re-run.

**Quorum size is the most sensitive parameter in the system.** The margin
goes as 1/n and the error tax goes as n. Any backend move to larger
default quorums should re-run S4 first.

**#101 (fee-discount auction).** The issue says it "needs real economic
modelling first". F4 is that modelling at first order. A discount of Δf
is worth A(1−e)·Δf/n per answer: about 0.0024 USDC for 5 points at the
0.25 tier and n = 5. That only helps workers whose costs sit within that
band of break-even. Recommendation: only pursue #101 for higher pricing
tiers, where A is large enough for the discount to matter, or once S3
with measured costs shows retention is fee-limited. Evaluate it by
running S3 at the auction's expected effective fee distribution.

**#103 (cooperative pools).** Pooling spreads the F2 tax across members
without reducing it. A pool of m workers with independent errors has the
same expected slash per answer, with lower variance. Pools help retention
only if workers who are close to break-even leave because of *variance*,
which the current exit rule (a negative 20-answer window) does model. So
S1/S4 with a longer `retention_window` gives a rough upper bound on what
pooling is worth.

**#126 / #127.** The issue text only says these propose changes to the
staking, slashing or payout system. Their specific mechanisms aren't
covered here. The model can still evaluate them:

- A change to the slash formula goes in `math::slash` / `Params`, then
  S1, S2 and S5 are re-run. Its lib.rs counterpart must land in the same
  PR, or `test_incentive_math` fails.
- A change to the payout split goes in `math::resolve`, then S3 and S4
  are re-run.

The pass/fail bar for either: honest retention and cartel excess must be
no worse than the current constants' rows in the same table.

## 5. How to extend

- **New behaviour.** Add a `Strategy` variant in `model.rs`.
- **New parameter.** Add a field to `Config` plus a sweep in `main.rs`.
- **New contract arithmetic.** Put it in `math.rs` *and* add a
  `check_resolve`-style case to `src/test_incentive_math.rs` that runs it
  through the real contract. Code in `math.rs` without a matching contract
  test defeats the purpose of the file.

## 6. Grounding in the literature

This is open question 3 of the issue. The model is ad hoc in its
parameters, but its structure follows well-studied designs:

- **Schelling-point oracles and the P + ε attack.** Buterin's
  *SchellingCoin* (2014) and *The P + epsilon attack* (2015) cover
  majority-vote oracles. An outside briber can make lying the equilibrium
  for less than the pool pays, as long as the payment is conditional on
  the lie winning. That is the `bribe` model here. It's also why F3 finds
  that consensus-based slashing can't be the defence.
- **Peer prediction.** Miller, Resnick & Zeckhauser, *Eliciting
  Informative Feedback: The Peer-Prediction Method* (2005), and Prelec,
  *A Bayesian Truth Serum for Subjective Data* (2004). Both reward
  agreement with peers without ground truth, and both admit uninformative
  equilibria like the coordinated lie. The standard fix is exactly the
  spot-checking modelled here as audits: see Gao, Wright & Leyton-Brown,
  *Incentivizing Evaluation via Limited Access to Ground Truth* (2016).
- **Escalating bonds and dispute rounds.** Augur and UMA's optimistic
  oracle defend with dispute bonds rather than fixed-rate slashing. That
  is roughly a larger, dispute-triggered cap. It's a candidate direction
  for #126/#127, and a natural next `Strategy` or `Params` extension to
  model.

A proper mechanism-design treatment, such as an equilibrium analysis of
the audit game, is out of scope for #117. The model is meant to be
falsifiable and re-runnable, not optimal.
