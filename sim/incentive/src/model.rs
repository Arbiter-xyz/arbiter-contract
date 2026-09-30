//! The agent-based model. Every token amount is an i128 in the token's
//! smallest unit (stroops for 7-decimal USDC), and every amount that the
//! contract would move is computed by math.rs — the same formulas as
//! lib.rs. Floats appear only in behavioural state (the adaptive cartel's
//! payoff estimates, the payers' memory of wrong answers), never in money.
//!
//! One round = one batch of questions. For each question:
//!
//! 1. The backend draws `quorum` distinct active workers uniformly.
//! 2. Honest workers answer correctly with probability 1 - error, and pay
//!    their private per-answer cost either way.
//! 3. The cartel sees how many seats it holds (it coordinates on question
//!    ids) and, if it holds a majority, its strategy decides whether to
//!    vote the same wrong answer on every seat. A lie costs no work.
//! 4. Majority wins. With probability `audit_bps` a wrong majority is
//!    caught and the backend resolves against the truth instead (losers
//!    are then the wrong voters). A tie, or an audit that finds no correct
//!    voter, becomes a refund.
//! 5. math::resolve() computes fee, shares, dust and slashes.
//! 6. Caught cartel identities are banned and replaced at `sybil_cost`.
//!    Workers top slashed stake back up to their target, so the loss is
//!    booked as P&L rather than silently shrinking future slashes.
//!
//! Between rounds, honest workers whose P&L over their last
//! `retention_window` answers was negative leave for good; a new honest
//! worker may join when honest work was profitable on average. Payer
//! demand shrinks with the recent wrong-answer rate.

use crate::math::{self, Params, Stake};
use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Cartel identities behave exactly like honest workers. Used as the
    /// counterfactual when measuring what attacking is worth.
    Honest,
    /// Lie on every question where the cartel holds a majority.
    Opportunistic,
    /// Lie on a majority only while the learned payoff of lying beats the
    /// learned payoff of answering honestly (plus a little exploration).
    Adaptive,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub params: Params,
    pub quorum: u32,
    pub honest_workers: u32,
    pub cartel_workers: u32,
    pub rounds: u32,
    /// Questions per round at full payer confidence.
    pub questions_per_round: u32,
    pub amount: i128,
    pub honest_stake: i128,
    pub cartel_stake: i128,
    /// Probability an honest answer is wrong.
    pub honest_error_bps: u32,
    /// Mean per-answer cost for an honest worker; each draws its own cost
    /// uniformly from [0.5x, 1.5x] of this.
    pub answer_cost: i128,
    /// Paid to the cartel by an outside party per successfully captured
    /// question (split over its seats).
    pub bribe: i128,
    pub audit_bps: u32,
    /// Cost of standing up a fresh identity after a ban (new account,
    /// warm-up time, any KYC). Stake itself is not a cost: it's returned.
    pub sybil_cost: i128,
    pub strategy: Strategy,
    /// Answers after which an honest worker reviews its P&L.
    pub retention_window: u32,
    /// Per-round chance a new honest worker joins while honest work pays.
    pub entry_bps: u32,
    /// Demand multiplier lost per unit of recent wrong-answer rate, in bps
    /// (30_000 means a 10% wrong rate costs 30% of demand).
    pub payer_sensitivity_bps: u32,
    pub seed: u64,
}

pub const USDC: i128 = 10_000_000;

impl Default for Config {
    fn default() -> Config {
        Config {
            params: Params::CONTRACT,
            quorum: 5,
            honest_workers: 80,
            cartel_workers: 20,
            rounds: 200,
            questions_per_round: 50,
            amount: USDC / 4,
            honest_stake: 10 * USDC,
            cartel_stake: 10 * USDC,
            honest_error_bps: 500,
            answer_cost: USDC / 50,
            bribe: USDC / 2,
            audit_bps: 500,
            sybil_cost: USDC,
            strategy: Strategy::Adaptive,
            retention_window: 20,
            entry_bps: 2_000,
            payer_sensitivity_bps: 30_000,
            seed: 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Honest,
    Cartel,
}

struct Worker {
    kind: Kind,
    stake: Stake,
    target_stake: i128,
    cost: i128,
    active: bool,
    /// Part of the population at round 0 (retention is measured on these).
    original: bool,
    window_pnl: i128,
    window_n: u32,
}

impl Worker {
    fn honest(cfg: &Config, rng: &mut Rng, original: bool) -> Worker {
        let lo = cfg.answer_cost / 2;
        let cost = lo + rng.below((cfg.answer_cost.max(1)) as u64) as i128;
        Worker {
            kind: Kind::Honest,
            stake: Stake::settled(cfg.honest_stake),
            target_stake: cfg.honest_stake,
            cost,
            active: true,
            original,
            window_pnl: 0,
            window_n: 0,
        }
    }

    fn cartel(cfg: &Config) -> Worker {
        Worker {
            kind: Kind::Cartel,
            stake: Stake::settled(cfg.cartel_stake),
            target_stake: cfg.cartel_stake,
            cost: cfg.answer_cost,
            active: true,
            original: true,
            window_pnl: 0,
            window_n: 0,
        }
    }
}

struct Learner {
    attack: f64,
    honest: f64,
}

impl Learner {
    const EXPLORE_BPS: u32 = 500;
    const ALPHA: f64 = 0.1;

    fn decide(&self, s: Strategy, rng: &mut Rng) -> bool {
        match s {
            Strategy::Honest => false,
            Strategy::Opportunistic => true,
            Strategy::Adaptive => self.attack > self.honest || rng.chance_bps(Self::EXPLORE_BPS),
        }
    }

    fn learn(&mut self, attacked: bool, per_seat: f64) {
        let e = if attacked { &mut self.attack } else { &mut self.honest };
        *e = (1.0 - Self::ALPHA) * *e + Self::ALPHA * per_seat;
    }
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub demanded: u64,
    pub resolved: u64,
    pub refunded: u64,
    /// Questions that could not be dispatched: fewer than `quorum` active
    /// workers were left.
    pub unserved: u64,
    pub wrong: u64,
    pub attacks: u64,
    pub caught: u64,
    pub honest_start: u32,
    pub honest_retained: u32,
    pub honest_final: u32,
    pub honest_earned: i128,
    pub honest_slashed: i128,
    pub honest_pnl: i128,
    pub cartel_pnl: i128,
    pub cartel_bans: u32,
    pub platform_by_round: Vec<i128>,
    pub final_demand: u32,
}

impl Outcome {
    pub fn platform_mean(&self) -> f64 {
        let n = self.platform_by_round.len().max(1) as f64;
        self.platform_by_round.iter().map(|&x| x as f64).sum::<f64>() / n
    }

    /// Coefficient of variation of per-round platform revenue.
    pub fn platform_cv(&self) -> f64 {
        let m = self.platform_mean();
        if m == 0.0 {
            return 0.0;
        }
        let n = self.platform_by_round.len().max(1) as f64;
        let var = self
            .platform_by_round
            .iter()
            .map(|&x| (x as f64 - m).powi(2))
            .sum::<f64>()
            / n;
        var.sqrt() / m
    }
}

fn demand(cfg: &Config, wrong_ema: f64) -> u32 {
    let loss = cfg.payer_sensitivity_bps as f64 / 10_000.0 * wrong_ema;
    (cfg.questions_per_round as f64 * (1.0 - loss).max(0.0)).round() as u32
}

pub fn run(cfg: &Config) -> Outcome {
    assert!(cfg.quorum > 0);
    let mut rng = Rng::new(cfg.seed);
    let mut workers: Vec<Worker> = Vec::new();
    for _ in 0..cfg.honest_workers {
        workers.push(Worker::honest(cfg, &mut rng, true));
    }
    for _ in 0..cfg.cartel_workers {
        workers.push(Worker::cartel(cfg));
    }

    let mut out = Outcome {
        honest_start: cfg.honest_workers,
        ..Outcome::default()
    };
    let majority = cfg.quorum / 2 + 1;
    let q = cfg.quorum as usize;
    let mut learner = Learner {
        attack: (cfg.bribe / majority as i128) as f64,
        honest: 0.0,
    };
    let mut wrong_ema = 0.0f64;
    let mut active: Vec<usize> = Vec::new();

    for _round in 0..cfg.rounds {
        let want = demand(cfg, wrong_ema);
        out.demanded += want as u64;
        out.final_demand = want;
        let mut platform = 0i128;
        let (mut round_resolved, mut round_wrong) = (0u32, 0u32);
        let mut honest_margin = 0i128;

        for _ in 0..want {
            active.clear();
            active.extend((0..workers.len()).filter(|&i| workers[i].active));
            if active.len() < q {
                out.unserved += 1;
                continue;
            }
            for i in 0..q {
                let j = i + rng.below((active.len() - i) as u64) as usize;
                active.swap(i, j);
            }
            let seats: Vec<usize> = active[..q].to_vec();
            let k = seats
                .iter()
                .filter(|&&i| workers[i].kind == Kind::Cartel)
                .count() as u32;
            let decided = k >= majority;
            let attack = decided && learner.decide(cfg.strategy, &mut rng);
            if attack {
                out.attacks += 1;
            }

            // true = voted the correct answer.
            let votes: Vec<bool> = seats
                .iter()
                .map(|&i| {
                    if attack && workers[i].kind == Kind::Cartel {
                        false
                    } else {
                        !rng.chance_bps(cfg.honest_error_bps)
                    }
                })
                .collect();
            let correct = votes.iter().filter(|v| **v).count() as u32;
            let wrong_votes = cfg.quorum - correct;

            let majority_correct = correct > wrong_votes;
            let audited =
                correct != wrong_votes && !majority_correct && rng.chance_bps(cfg.audit_bps);
            let winners_correct = majority_correct || audited;
            let winner_count = if winners_correct { correct } else { wrong_votes };
            let refund = correct == wrong_votes || winner_count == 0;
            if audited && attack {
                out.caught += 1;
            }

            // Settlement, through the contract's own arithmetic.
            let mut loser_stakes: Vec<Stake> = Vec::new();
            let mut settlement = math::Settlement::default();
            if refund {
                out.refunded += 1;
            } else {
                for (s, &i) in seats.iter().enumerate() {
                    if votes[s] != winners_correct {
                        loser_stakes.push(workers[i].stake);
                    }
                }
                settlement = math::resolve(cfg.amount, winner_count, &mut loser_stakes, cfg.params);
                platform += settlement.platform_take();
                out.resolved += 1;
                round_resolved += 1;
                if !winners_correct {
                    out.wrong += 1;
                    round_wrong += 1;
                }
            }

            let bribe_each = if attack && !refund && !winners_correct {
                cfg.bribe / k as i128
            } else {
                0
            };
            let mut cartel_q_pnl = 0i128;
            let mut bans = 0u32;
            let mut li = 0usize;
            for (s, &i) in seats.iter().enumerate() {
                let w = &mut workers[i];
                let lied = attack && w.kind == Kind::Cartel;
                let mut pnl = if lied { 0 } else { -w.cost };
                let mut earned = 0;
                let mut taken = 0;
                if !refund {
                    if votes[s] == winners_correct {
                        earned = settlement.share;
                    } else {
                        taken = w.stake.slashable() - loser_stakes[li].slashable();
                        w.stake = loser_stakes[li];
                        li += 1;
                        // Top back up to target: the slash is a realised
                        // loss, the re-bond is just capital.
                        let short = w.target_stake - w.stake.slashable();
                        if short > 0 {
                            w.stake.settled += short;
                        }
                    }
                }
                pnl += earned - taken;

                match w.kind {
                    Kind::Honest => {
                        out.honest_earned += earned;
                        out.honest_slashed += taken;
                        out.honest_pnl += pnl;
                        honest_margin += pnl;
                        w.window_pnl += pnl;
                        w.window_n += 1;
                        if w.window_n >= cfg.retention_window {
                            if w.window_pnl < 0 {
                                w.active = false;
                            }
                            w.window_pnl = 0;
                            w.window_n = 0;
                        }
                    }
                    Kind::Cartel => {
                        pnl += bribe_each;
                        if lied && audited {
                            w.active = false;
                            bans += 1;
                            pnl -= cfg.sybil_cost;
                        }
                        cartel_q_pnl += pnl;
                    }
                }
            }
            for _ in 0..bans {
                workers.push(Worker::cartel(cfg));
            }
            out.cartel_bans += bans;
            out.cartel_pnl += cartel_q_pnl;
            if decided {
                learner.learn(attack, cartel_q_pnl as f64 / k as f64);
            }
        }

        out.platform_by_round.push(platform);
        if round_resolved > 0 {
            let rate = round_wrong as f64 / round_resolved as f64;
            wrong_ema = 0.8 * wrong_ema + 0.2 * rate;
        }
        let honest_active = workers
            .iter()
            .filter(|w| w.kind == Kind::Honest && w.active)
            .count() as u32;
        if honest_margin > 0
            && honest_active < 2 * cfg.honest_workers
            && rng.chance_bps(cfg.entry_bps)
        {
            workers.push(Worker::honest(cfg, &mut rng, false));
        }
    }

    out.honest_retained = workers
        .iter()
        .filter(|w| w.kind == Kind::Honest && w.original && w.active)
        .count() as u32;
    out.honest_final = workers
        .iter()
        .filter(|w| w.kind == Kind::Honest && w.active)
        .count() as u32;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_a_seed() {
        let cfg = Config {
            rounds: 20,
            ..Config::default()
        };
        let a = run(&cfg);
        let b = run(&cfg);
        assert_eq!(a.platform_by_round, b.platform_by_round);
        assert_eq!(a.cartel_pnl, b.cartel_pnl);
    }

    #[test]
    fn no_cartel_no_errors_means_no_slashing_and_no_wrong_answers() {
        let cfg = Config {
            cartel_workers: 0,
            honest_error_bps: 0,
            rounds: 20,
            ..Config::default()
        };
        let o = run(&cfg);
        assert_eq!(o.honest_slashed, 0);
        assert_eq!(o.wrong, 0);
        // Every resolved question pays exactly the contract's fee + dust.
        let s = math::resolve(cfg.amount, cfg.quorum, &mut [], cfg.params);
        let total: i128 = o.platform_by_round.iter().sum();
        assert_eq!(total, s.platform_take() * o.resolved as i128);
    }

    #[test]
    fn honest_strategy_never_attacks() {
        let cfg = Config {
            strategy: Strategy::Honest,
            rounds: 20,
            ..Config::default()
        };
        let o = run(&cfg);
        assert_eq!(o.attacks, 0);
        assert_eq!(o.cartel_bans, 0);
    }

    #[test]
    fn money_is_conserved_per_resolution() {
        // fee + dust + n * share == amount for every quorum size, which is
        // what lets the model treat platform_take() as the only leak.
        for n in 1..=9u32 {
            let s = math::resolve(USDC / 4, n, &mut [], Params::CONTRACT);
            assert_eq!(s.fee + s.dust + s.share * n as i128, USDC / 4);
        }
    }
}
