#!/usr/bin/env python3
"""Closed-form payoff model for docs/economics/slashing-threat-model.md.

Exact (hypergeometric) probabilities, no Monte Carlo, stdlib only. The
Monte Carlo counterpart that drives the real compiled contracts lives in
src/test_economics.rs; this script sweeps the parameter ranges that would be
too slow to run through a contract.

    python3 sim/slashing_model.py            # print every table as markdown
    python3 sim/slashing_model.py --check    # assert the doc's claims (CI)

Units are USDC. Contract constants mirror src/lib.rs.
"""
from __future__ import annotations

import argparse
import math
import sys

FEE = 0.20          # PLATFORM_FEE_BPS
SLASH = 0.05        # SLASH_BPS
CAP_OF_AMOUNT = 1.0  # SLASH_CAP_BPS_OF_AMOUNT (new contract only)


def slash_v020(stake: float, amount: float) -> float:
    return SLASH * stake


def slash_now(stake: float, amount: float) -> float:
    return min(SLASH * stake, CAP_OF_AMOUNT * amount)


def hyper_ge(N: int, C: int, n: int, t: int) -> float:
    """P(at least t of the n drawn are among the C marked), drawing n of N."""
    total = math.comb(N, n)
    return sum(math.comb(C, k) * math.comb(N - C, n - k) for k in range(t, min(C, n) + 1)) / total


def hyper_k(N: int, C: int, n: int, k: int) -> float:
    return math.comb(C, k) * math.comb(N - C, n - k) / math.comb(N, n)


def majority(n: int) -> int:
    return n // 2 + 1


def supermajority(n: int) -> int:
    """Smallest vote count strictly above 2/3 of n: 3->3, 5->4, 7->5, 9->7."""
    return 2 * n // 3 + 1


# ---------------------------------------------------------------------------
# A0: honest-error tax (no attacker)
# ---------------------------------------------------------------------------

def a0_honest_ev(stake: float, amount: float, n: int, err: float, slash_fn) -> float:
    """Expected P&L per question for an honest worker in an honest quorum.

    Paid 0.8A split over the correct voters when right (approximated by the
    expected number of correct peers), slashed when wrong and out-voted.
    """
    pool = (1 - FEE) * amount
    # Expected share when correct: pool / (1 + #correct among the other n-1).
    share = sum(
        math.comb(n - 1, j) * (1 - err) ** j * err ** (n - 1 - j) * pool / (1 + j)
        for j in range(n)
    )
    # Wrong and out-voted: at least majority(n) of the others are correct.
    outvoted = sum(
        math.comb(n - 1, j) * (1 - err) ** j * err ** (n - 1 - j)
        for j in range(majority(n), n)
    )
    return (1 - err) * share - err * outvoted * slash_fn(stake, amount)


def a0_breakeven_stake(amount: float, n: int, err: float) -> float:
    lo, hi = 0.0, 1e9
    for _ in range(200):
        mid = (lo + hi) / 2
        if a0_honest_ev(mid, amount, n, err, slash_v020) > 0:
            lo = mid
        else:
            hi = mid
    return lo


# ---------------------------------------------------------------------------
# A1: bribed wrong-answer collusion
# ---------------------------------------------------------------------------

def a1_gain_per_question(N, C, n, amount, bribe, err, accept_at, audit, ban_loss, slash_fn, stake):
    """Cartel's expected gain per question from lying-when-able vs honest play.

    The cartel coordinates on question ids, so it knows k (its seats in the
    quorum) and lies only when k >= accept_at. Honest errors are ignored in
    the capture condition (they only ever help the cartel slightly).
    """
    pool = (1 - FEE) * amount
    g = 0.0
    for k in range(accept_at, min(C, n) + 1):
        p = hyper_k(N, C, n, k)
        honest_pay = k * pool / n * (1 - err)       # what honest play would have earned
        lie_pay = pool                               # cartel captures the whole pool
        win = bribe + lie_pay - honest_pay
        # Audit: resolved against truth -> cartel unpaid, slashed, banned.
        caught = -(honest_pay + k * slash_fn(stake, amount) + k * ban_loss)
        g += p * ((1 - audit) * win + audit * caught)
    return g


def a1_min_audit_rate(N, C, n, amount, bribe, err, accept_at, ban_loss, slash_fn, stake):
    lo, hi = 0.0, 1.0
    if a1_gain_per_question(N, C, n, amount, bribe, err, accept_at, 1.0, ban_loss, slash_fn, stake) > 0:
        return float("inf")
    for _ in range(60):
        mid = (lo + hi) / 2
        if a1_gain_per_question(N, C, n, amount, bribe, err, accept_at, mid, ban_loss, slash_fn, stake) > 0:
            lo = mid
        else:
            hi = mid
    return hi


def ban_loss(N, amount, horizon=500):
    """What a banned identity forfeits: its expected earnings over the next
    `horizon` questions the platform dispatches. Each question pays out
    (1-FEE)*amount across the pool, so one identity's share is ~1/N of it."""
    return horizon * (1 - FEE) * amount / N


# ---------------------------------------------------------------------------
# A2: targeted slash-griefing
# ---------------------------------------------------------------------------

def a2_victim_loss_per_quorum(N, C, n, victim_stake, amount, accept_at, slash_fn):
    """Expected slash per question the victim is dispatched to, when the
    cartel lies every time it can out-vote the victim's quorum."""
    p = hyper_ge(N - 1, C, n - 1, accept_at)
    return p * slash_fn(victim_stake, amount)


# ---------------------------------------------------------------------------
# A3: stake timing
# ---------------------------------------------------------------------------

def a3_expected_slash_for_liar(stake, amount, pulls_stake: bool, contract: str):
    if contract == "v0.2.0":
        return 0.0 if pulls_stake else slash_v020(stake, amount)
    return slash_now(stake, amount)  # unbonding keeps it slashable


def a3_flash_stake_capital_cost(stake, annual_rate, warmup_days, unbond_days):
    """Opportunity cost of a stake held only to look credible for one burst."""
    return stake * annual_rate * (warmup_days + unbond_days) / 365


# ---------------------------------------------------------------------------

def fmt(x: float) -> str:
    if x == float("inf"):
        return "never"
    if abs(x) >= 100:
        return f"{x:,.0f}"
    return f"{x:.4f}".rstrip("0").rstrip(".") if abs(x) < 1 else f"{x:.2f}"


def table(headers, rows):
    out = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def report():
    err = 0.05
    print("## A0 — honest-error tax (no attacker), quorum 5, 5% honest error\n")
    rows = []
    for amount in (0.25, 1.0, 5.0):
        for stake in (1, 10, 100, 1000):
            rows.append([
                amount, stake,
                fmt(a0_honest_ev(stake, amount, 5, err, slash_v020)),
                fmt(a0_honest_ev(stake, amount, 5, err, slash_now)),
            ])
    print(table(["question", "stake", "EV/question v0.2.0", "EV/question now"], rows))
    print()
    print("Break-even stake on v0.2.0 (EV turns negative above it): " + ", ".join(
        f"{a} USDC question → {fmt(a0_breakeven_stake(a, 5, err))} USDC" for a in (0.25, 1.0, 5.0)))
    print()

    print("## A1 — P(cartel controls a quorum)\n")
    rows = []
    for N in (50, 100, 200):
        for frac in (0.1, 0.2, 0.3):
            C = int(N * frac)
            for n in (3, 5, 7):
                rows.append([N, f"{int(frac*100)}%", n,
                             fmt(hyper_ge(N, C, n, majority(n))),
                             fmt(hyper_ge(N, C, n, supermajority(n))) + f" (≥{supermajority(n)})"])
    print(table(["pool N", "cartel share", "quorum", "P(≥ majority)", "P(≥ supermajority)"], rows))
    print()

    print("## A1 — bribed collusion: cartel gain per question dispatched, quorum 5, 0.25 USDC, bribe 0.50, 10 USDC stakes\n")
    rows = []
    amount, bribe, stake = 0.25, 0.50, 10.0
    for N in (50, 100):
        for frac in (0.1, 0.2, 0.3):
            C = int(N * frac)
            ban = ban_loss(N, amount)
            base = a1_gain_per_question(N, C, 5, amount, bribe, err, 3, 0.0, 0.0, slash_now, stake)
            smaj = a1_gain_per_question(N, C, 5, amount, bribe, err, 4, 0.0, 0.0, slash_now, stake)
            hard = a1_gain_per_question(N, C, 5, amount, bribe, err, 4, 0.15, ban, slash_now, stake)
            g_star = a1_min_audit_rate(N, C, 5, amount, bribe, err, 4, ban, slash_now, stake)
            rows.append([N, f"{int(frac*100)}%", fmt(base), fmt(smaj), fmt(hard), f"{g_star*100:.1f}%"])
    print(table(["pool N", "cartel", "majority, no audit", "4-of-5, no audit", "4-of-5 + 15% audit + ban",
                 "min audit rate for EV<0"], rows))
    print("\n(ban loss = an identity's expected earnings over the platform's next 500 questions)\n")

    print("## A2 — targeted griefing: victim's expected slash per quorum it sits in\n")
    rows = []
    for N in (20, 50, 100):
        for frac in (0.1, 0.2, 0.3):
            C = int(N * frac)
            for vs in (10, 100):
                v = a2_victim_loss_per_quorum(N, C, 5, vs, 0.25, 3, slash_v020)
                c = a2_victim_loss_per_quorum(N, C, 5, vs, 0.25, 3, slash_now)
                s = a2_victim_loss_per_quorum(N, C, 5, vs, 0.25, 4, slash_now)
                rows.append([N, f"{int(frac*100)}%", vs, fmt(v), fmt(c), fmt(s),
                             fmt(v / s) + "×" if s else "∞"])
    print(table(["pool N", "cartel", "victim stake", "v0.2.0", "now (cap)", "now + 4-of-5", "reduction"], rows))
    print()

    print("## A3 — stake timing, 0.25 USDC question\n")
    rows = []
    for stake in (1, 10, 100):
        rows.append([stake,
                     fmt(a3_expected_slash_for_liar(stake, 0.25, True, "v0.2.0")),
                     fmt(a3_expected_slash_for_liar(stake, 0.25, True, "now")),
                     fmt(a3_flash_stake_capital_cost(stake, 0.08, 0, 0)),
                     fmt(a3_flash_stake_capital_cost(stake, 0.08, 1, 3))])
    print(table(["stake", "liar's slash v0.2.0 (pulls stake first)", "liar's slash now",
                 "flash-stake cost v0.2.0", "flash-stake cost now (8%/yr, 1d warmup + 3d unbond)"], rows))


def check():
    err = 0.05
    # A0: v0.2.0 makes a 100 USDC stake a losing position on the smallest tier; the cap fixes it.
    assert a0_honest_ev(100, 0.25, 5, err, slash_v020) < 0
    assert a0_honest_ev(100, 0.25, 5, err, slash_now) > 0
    for s in (1, 10, 100, 1000, 10**6):
        for a in (0.25, 1, 5):
            assert a0_honest_ev(s, a, 5, err, slash_now) > 0, (s, a)
    # A1: supermajority alone cuts capture probability by > 5x at 20% cartels.
    for N in (50, 100, 200):
        C = N // 5
        assert hyper_ge(N, C, 5, 3) > 5 * hyper_ge(N, C, 5, 4)
    # A1: baseline bribed collusion pays; hardened policy makes it negative EV
    # for cartels up to 30% of the pool.
    for N in (50, 100):
        for frac in (0.1, 0.2, 0.3):
            C = int(N * frac)
            ban = ban_loss(N, 0.25)
            assert a1_gain_per_question(N, C, 5, 0.25, 0.5, err, 3, 0, 0, slash_now, 10) > 0
            assert a1_gain_per_question(N, C, 5, 0.25, 0.5, err, 4, 0.15, ban, slash_now, 10) < 0
    # A2: cap alone gives >= stake/question-ish reduction for a 100 USDC victim.
    for N in (20, 50, 100):
        C = N // 5
        v = a2_victim_loss_per_quorum(N, C, 5, 100, 0.25, 3, slash_v020)
        c = a2_victim_loss_per_quorum(N, C, 5, 100, 0.25, 3, slash_now)
        assert abs(v / c - 20) < 1e-9
    # A3: pulling stake made lying free on v0.2.0, not now.
    assert a3_expected_slash_for_liar(10, 0.25, True, "v0.2.0") == 0
    assert a3_expected_slash_for_liar(10, 0.25, True, "now") == 0.25
    print("all model checks passed")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args()
    if args.check:
        check()
    else:
        report()
    sys.exit(0)
