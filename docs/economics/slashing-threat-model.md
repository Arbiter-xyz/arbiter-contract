# Slashing: threat model, collusion and griefing analysis

Issue #8. This analyses the economic security of `resolve()`'s slashing
against a coordinated adversary. For each attack it gives the payoff
before and after the mitigations shipped in v0.3, and says where each
mitigation is enforced. Every claim below links to the check that backs it:

- **Real-contract simulations:** [src/test_economics.rs](../../src/test_economics.rs)
  runs the deployed v0.2.0 Wasm and the current contract side by side,
  with the same seeds, quorums and decisions. Only the contract differs.
- **Closed-form sweep:** [sim/slashing_model.py](../../sim/slashing_model.py)
  computes exact hypergeometric probabilities over realistic parameter
  ranges. Its `--check` mode asserts the claims below and runs in CI. Full
  tables are in [model-tables.md](model-tables.md).

## 1. Model

**What the contract knows.** Nothing about answers. The backend (the
admin) decides who agreed with consensus (`workers`) and who didn't
(`losing_workers`). `resolve()` pays 80% of the question to `workers`,
20% to the platform, and slashes each loser. So slashing punishes
**disagreeing with consensus, not being wrong.** That fact drives
everything below.

**Actors.**

- *Honest workers* answer correctly with probability 1 − e. We use
  e = 5%.
- *A cartel* controls C of the N worker identities in the dispatch pool.
  It coordinates out of band, including by sharing which question ids
  each member received, so it always knows k, its seat count in a given
  quorum. It can create more identities (sybils), subject to whatever
  stake the backend requires.
- *A briber* pays the cartel B per corrupted answer that the backend
  accepts. This is the realistic motive for attacking an oracle.
- *A competitor-griefer* wants a specific staked worker V to lose stake
  or fall below the dispatch threshold.
- *The backend* is trusted to follow its policy. A compromised admin is
  out of scope (§7).

**Parameters.**

| symbol | meaning | values used |
|---|---|---|
| A | question amount | 0.25 USDC (smallest tier), 1, 5 |
| P = 0.8A | worker pool per question | |
| n | quorum size | 3, 5, 7 |
| N, C | dispatch pool, cartel identities | N ∈ {20…200}, C/N ∈ {10%, 20%, 30%} |
| t | votes needed for the backend to accept an answer | ⌊n/2⌋+1 (v0.2.0 backend), ⌊2n/3⌋+1 (hardened) |
| S | a worker's stake | 1 to 1,000 USDC |
| s(S, A) | slash per lost quorum | v0.2.0: 0.05·S; v0.3: min(0.05·S, A) |
| g | fraction of questions that are backend-planted audits with a known answer | 0, 15% |
| L | what a banned identity forfeits (its expected earnings over the platform's next 500 questions) | 500·P/N |

Quorums are drawn uniformly at random from the eligible pool, so the
cartel's seat count k is hypergeometric: h(k) = C(C,k)·C(N−C,n−k)/C(N,n).

## 2. Summary

| # | attack | v0.2.0 payoff | mitigation | where | payoff after | evidence |
|---|---|---|---|---|---|---|
| A0 | *(no attacker)* honest mistakes slash staked workers | a 100 USDC-staked honest worker **loses 0.21/question** on 0.25 USDC questions | per-question slash cap `min(5%·S, A)` | contract | +0.028/question at any stake | `a0_honest_error_tax_made_large_stakes_irrational_on_v020`; model A0 |
| A1 | bribed cartel answers wrong in unison whenever k ≥ t | **positive** at every cartel size tested, e.g. +0.031/question (N=100, 20% cartel) | accept only at ⌊2n/3⌋+1 (else refund) **and** 15% audits with ban | backend | **negative** for cartels up to 30% | `a1_supermajority_plus_audits_turn_bribed_collusion_negative_ev`; model A1 |
| A2 | cartel out-votes a targeted staked competitor to get them slashed | victim loses 5%·S_V per win, **independent of the question**: 5 USDC on a 0.25 USDC question at S_V = 100 | slash cap (contract) + supermajority acceptance (backend) | both | 20× less from the cap alone at S_V = 100; 200× to 1,800× with 4-of-5 | `a2_one_griefing_win_…`, `a2_monte_carlo_…`, `a2_micro_questions_…`; model A2 |
| A2′ | micro-question leverage: a 1-stroop question slashes as hard as a large one | 5 USDC slashed via a question worth 10⁻⁷ USDC | slash cap | contract | 1 stroop | `a2_micro_questions_no_longer_multiply_slash_leverage` |
| A3 | liar pulls stake between answering and `resolve()` | **slash = 0**: lying was free | two-step unbonding; unbonding stake stays slashable | contract | min(5%·S, A) | `a3_pulling_stake_before_resolve_…`, `a3_unbonding_stake_stays_slashable_…` |
| A3′ | flash-stake right before dispatch to look credible, withdraw right after | credibility is free; capital locked ~0 ledgers | `get_matured_stake()` (1-day warmup); backend gates on it | contract + backend | capital locked ≥ 1 day warmup + 3 days unbonding | `a3_flash_stake_counts_immediately_…`, `a3_begin_unstake_drops_matured_stake_immediately` |
| A4 | escape the losing list by not answering, or by copying visible answers | slash avoided / paid without working | commit–reveal; a missed reveal counts as losing | backend | n/a | §6 |

## 3. A0: the honest-error tax

Take the case with no attacker first, because it shows the core defect.
An honest worker is out-voted with probability ≈ e and then loses
0.05·S. They earn roughly P/n when right. Per question:

    EV(S) = (1−e)·E[P / #correct] − e·Pr[out-voted | wrong]·s(S, A)

Under v0.2.0, s = 0.05·S grows without bound in S while earnings don't
grow at all, so EV turns negative above a break-even stake
(model A0):

| question | break-even stake on v0.2.0 |
|---|---|
| 0.25 USDC | 16.2 USDC |
| 1 USDC | 64.9 USDC |
| 5 USDC | 325 USDC |

**Staking more to look credible was a losing trade.** An honest worker
with 100 USDC on 0.25 USDC questions loses 0.21 USDC per question in
expectation, with no attacker involved. The real-contract simulation
confirms it: over 300 questions, the 100 USDC-staked honest worker ends
at **−21.6 USDC on v0.2.0 vs +3.3 USDC now**. Honest workers as a group
lose 312 USDC to slashing vs 18 now. The same mechanism powers A2, where
the "noise" comes from an adversary instead.

**Principle behind the fix:** a lost quorum is an honest mistake, an
ambiguous question, or a griefing victim at least as often as it is a
liar. So its penalty must be bounded by what that question put at stake,
not by the worker's net worth. `SLASH_CAP_BPS_OF_AMOUNT = 100%` gives
min(0.05·S, A): a worker never loses more on a question than the
question was worth. With the cap, EV is positive at every stake tested
(model `--check` asserts stakes up to 10⁶ USDC).

## 4. A1: coordinated wrong-answer collusion

The cartel lies whenever k ≥ t, i.e. whenever lying changes the accepted
answer. Otherwise it plays honest and is indistinguishable from honest
workers. Per question dispatched, its expected gain over playing honest:

    G = Σ_{k≥t} h(k) · [ (1−g)·W_k − g·D_k ]
    W_k = B + P − k·P(1−e)/n          (bribe + whole pool, minus the share honest play earns)
    D_k = k·P(1−e)/n + k·s(S,A) + k·L (on an audit: unpaid, slashed, all k identities banned)

**Consensus slashing can't deter this and never could.** When the cartel
is the majority, *it* is `workers` and the honest minority is
`losing_workers`. Slashing then helps the attack by punishing the people
resisting it. No slash rate on consensus losses changes the cartel's
payoff. Only two things do:

1. **Pr[k ≥ t], via acceptance policy.** The backend accepts an answer
   only with ⌊2n/3⌋+1 agreeing votes (4 of 5) and refunds otherwise.
   That cuts the cartel's capture probability by 5× to 40× (model A1:
   N=100, 20% cartel, n=5: 5.3% → 0.54%). The cost is liveness: an
   honest market splits 4-of-5-or-worse on ~2.3% of questions at e = 5%.
   The simulation refunded 8 of 600 (`a1_hardened_policy_costs_honest_markets_little_liveness`).
2. **A truth-based penalty, via audits.** On a backend-planted question
   with a known answer, the backend resolves against the **truth**: the
   liars go in `losing_workers` even if they're the majority. They're
   slashed and **banned from dispatch**. Audits must be indistinguishable
   from real questions. Since the cartel lies whenever k ≥ t, it can't
   avoid them. Setting G < 0 gives the minimum audit rate,
   g* = W / (W + D). At the parameters in the summary, g* is 5.6% to 9.6%
   (model A1). We recommend 15% for margin.

Result, bribe = 2× the question (model A1, per question dispatched):

| pool N | cartel | majority, no audit | 4-of-5, no audit | 4-of-5 + 15% audit + ban |
|---|---|---|---|---|
| 100 | 10% | +0.0039 | +0.0001 | −0.0001 |
| 100 | 20% | +0.031 | +0.0029 | −0.0017 |
| 100 | 30% | +0.091 | +0.015 | −0.0088 |

On the real contract over 600 questions (N=20, 25% cartel, averaged over
4 seeds), the cartel's gain vs honest play goes from **+24.3 USDC (42
answers corrupted) to −9.6 USDC (2 corrupted, 3 of 5 identities
banned)**.

**Why the ban, not the stake, does the deterring.** The slash on an audit
is capped at A per identity, like any other. The penalty that makes D
large is L, the identity's future income, and the capital locked in its
matured stake while it's excluded. This is deliberate. A larger on-chain
audit slash would need the contract to trust the admin's claim "this was
an audit", and that label is exactly what a compromised admin would
abuse. The trade-off: if B ≫ L (a single answer worth far more than a
worker's future income), no audit rate is enough. Such questions need
larger quorums and a higher t, which the model's `min audit rate`
column shows.

## 5. A2: targeted slash-griefing

The cartel lies only when the victim V is in the quorum and the cartel
can out-vote it. V's expected slash per quorum it sits in:

    Pr[k ≥ t among the other n−1 seats] · s(S_V, A)

Under v0.2.0 this scales linearly with **the victim's** stake and not at
all with the question: 5 USDC per win against a 100 USDC stake, on a
0.25 USDC question. The cartel's own payoff doesn't change, since the
slash goes to the platform and the cartel still captures the pool.
Griefing a big competitor was therefore free to the attacker and
maximally damaging to the victim.

- **Cap (contract):** damage per win ≤ A. That's a 20× reduction at
  S_V = 100 on 0.25 USDC questions (exact, model `--check`).
- **4-of-5 acceptance (backend):** the cartel needs one more seat. With
  both, the reduction is 25× to 1,800× across the sweep (model A2).
- Real contract, 300 questions, 9 honest + 3-identity cartel
  (`a2_monte_carlo_targeted_griefing_damage_drops_by_the_stake_to_question_ratio`):

  |  | v0.2.0 | now (cap only) |
  |---|---|---|
  | victim slashed | 40.46 USDC | 2.63 USDC |
  | victim net P&L | **−35.77** | **+2.07** |
  | cartel P&L | 15.84 | 15.84 (identical: slashes never reach the cartel) |

- **Micro-question leverage (A2′):** v0.2.0's slash ignored the question
  amount, so the cheapest question in the system carried the same
  slashing power as the most expensive. Any path that gets a victim into
  `losing_workers` of a 1-stroop question (a lax minimum price, or a
  backend bug) cost them 5%. Now the damage is 1 stroop.

## 6. A3 and A4: stake timing and losing-list escape

**A3, pulling stake before `resolve()`.** v0.2.0's `unstake()` paid out
instantly. A worker bribed on one answer answers, unstakes everything,
and is listed as a loser with nothing to slash: **slash = 0**
(`a3_pulling_stake_before_resolve_escapes_the_slash_on_v020_but_not_now`).
The contract comment even noted "nothing stops a worker from unstaking
right before answering". The backend's periodically-refreshed stake
cache made this worse: the gate saw the stake, and the slash didn't.

Fix: unstaking is two-step. `begin_unstake(amount)` removes stake from
the active amount at once, so the backend stops dispatching against it.
The amount sits in an unbonding bucket that **remains slashable** until
`release_at = now + max(3 days, timeout_ledgers)`. After that,
`complete_unstake()` pays out whatever wasn't slashed. The delay is at
least the question timeout, so any question the worker answered is
resolved or refundable before the stake can leave.

**A3′, flash-staking for credibility.** Where stake gates dispatch or
weights trust, v0.2.0 let a fresh sybil stake the minimum, get
dispatched the same ledger, and withdraw immediately. Credibility cost
nothing but a network fee, so the cartel could inflate C/N freely. Now
`get_matured_stake()` counts only stake bonded ≥ 1 day. A top-up
re-warms only the new amount. `begin_unstake` drops matured stake
immediately. **The backend's gate must read `get_matured_stake()`.** A
sybil identity now locks S_min for at least 4 days (1 warmup + 3
unbonding) before it can do anything, plus L at risk while active. That
turns "free identities" into capital cost × identities × time, which is
what bounds C/N in §4.

**A4, escaping the losing list (backend).** `losing_workers` holds those
who *submitted* a non-consensus answer. A worker who sees consensus
forming against them (if answers are visible before the quorum closes)
can abstain and avoid the slash. Equally, they can copy the visible
majority and get paid without doing the work. Both are backend-level:
answers must be **committed (hash) before any are revealed**. A worker
who commits and doesn't reveal is treated as losing. The contract can't
enforce this. It has no view of answers.

## 7. What this does not cover

- **A compromised admin key** can already take pending escrow (resolve to
  its own addresses) and slash repeatedly. The cap limits each slash to
  the question amount, but a compromised admin can fund its own large
  questions and recycle the capital. Admin compromise is bounded by
  `refund_timeout()` for payers, not for stakers. Multi-sig/threshold
  admin is the fix, and it's out of scope here.
- **Selection is trusted.** Everything above assumes the backend draws
  quorums uniformly and doesn't leak membership. If an attacker can
  influence selection (for example by being "online" when a target's
  questions are dispatched), h(k) no longer applies. The backend should
  draw from the full eligible set with a seed committed before the
  question is posted, and cap per-identity dispatch share.
- **Audit leakage.** If audits are distinguishable (a different payer
  account, a timing pattern, a recycled answer set), the cartel skips
  them and A1 reverts to the "4-of-5, no audit" column. That column is
  still positive for large cartels.
- **B ≫ L.** See §4. High-value questions need bigger quorums.

## 8. Alternatives considered and rejected

- **Slash only when the winners are a supermajority (on-chain).** This
  would stop bare-majority griefing at the contract level. But it
  disables audit slashing exactly when it matters: on an audit where
  the cartel is the majority, the truthful winners are a minority, so
  the liars would be spared. Supermajority belongs in the backend's
  *acceptance* rule, not in the contract's slashing rule.
- **Larger consensus slash for deterrence.** This makes A0 and A2 worse
  (both scale with it) and does nothing for A1, where the cartel is
  never the one slashed.
- **Stake-weighted quorum selection.** This rewards concentrated capital
  with more seats, so a whale is a cartel of one. It also re-creates A3′
  unless weighted by matured stake. Not recommended.

## 9. Backend requirements (arbiter-backend)

1. Dispatch gate reads **`get_matured_stake()`**, not `get_stake()`.
2. Accept an answer only with ⌊2n/3⌋+1 agreeing votes. Otherwise
   `refund()`.
3. Plant indistinguishable audit questions at ≥ 10% (15% recommended).
   Resolve them against the known answer. Ban identities caught lying.
   Give honest workers two strikes.
4. Commit–reveal answers. A non-reveal counts as losing.
5. Uniform random quorums from the full eligible set, with the seed
   committed before posting.
6. Resolve within `timeout_ledgers` (the unbonding delay assumes it).

## Reproduce

```sh
cargo test test_economics -- --nocapture   # real-contract simulations, prints the numbers above
python3 sim/slashing_model.py              # full tables (model-tables.md)
python3 sim/slashing_model.py --check      # the assertions CI runs
```
