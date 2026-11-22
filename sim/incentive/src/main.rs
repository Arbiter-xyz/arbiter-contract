//! Incentive-alignment simulation for oracle-escrow (issue #117).
//!
//!     cargo run --release                                   # all sweeps, markdown to stdout
//!     cargo run --release -- --out ../../docs/economics/incentive-tables.md
//!     cargo run --release -- --seeds 4 --rounds 100         # faster, noisier
//!     cargo test                                            # model self-checks
//!
//! The contract-equivalence tests for src/math.rs live in the contract
//! crate (src/test_incentive_math.rs) because they need a Soroban Env.
//! Read docs/economics/incentive-alignment.md for what each column means
//! and what the numbers do and don't support.

mod math;
mod model;
mod rng;

use math::Params;
use model::{run, Config, Outcome, Strategy, USDC};
use std::fmt::Write as _;

struct Opts {
    seeds: u64,
    rounds: u32,
    out: Option<String>,
}

fn parse_args() -> Opts {
    let mut o = Opts {
        seeds: 8,
        rounds: 200,
        out: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--seeds" => o.seeds = val().parse().expect("--seeds N"),
            "--rounds" => o.rounds = val().parse().expect("--rounds N"),
            "--out" => o.out = Some(val()),
            "-h" | "--help" => {
                println!("usage: incentive-sim [--seeds N] [--rounds N] [--out FILE]");
                std::process::exit(0);
            }
            other => panic!("unknown argument {other}"),
        }
    }
    o
}

/// One table cell: the mean over `seeds` runs of a configuration, plus the
/// same seeds re-run with the cartel playing honestly as the counterfactual.
struct Summary {
    retention: f64,
    final_honest: f64,
    honest_slash_tax: f64,
    honest_margin_per_answer: f64,
    cartel_excess_per_id_per_1k: f64,
    attacks_per_1k: f64,
    wrong_rate: f64,
    served_rate: f64,
    platform_mean: f64,
    platform_cv: f64,
}

fn usdc(x: f64) -> f64 {
    x / USDC as f64
}

fn summarize(base: &Config, seeds: u64) -> Summary {
    let mut acc = Summary {
        retention: 0.0,
        final_honest: 0.0,
        honest_slash_tax: 0.0,
        honest_margin_per_answer: 0.0,
        cartel_excess_per_id_per_1k: 0.0,
        attacks_per_1k: 0.0,
        wrong_rate: 0.0,
        served_rate: 0.0,
        platform_mean: 0.0,
        platform_cv: 0.0,
    };
    for s in 0..seeds {
        let cfg = Config {
            seed: base.seed.wrapping_add(s * 7919),
            ..base.clone()
        };
        let o: Outcome = run(&cfg);
        let honest_cf = run(&Config {
            strategy: Strategy::Honest,
            ..cfg.clone()
        });

        let resolved_k = (o.resolved.max(1)) as f64 / 1000.0;
        let answers = (o.resolved + o.refunded).max(1) as f64 * cfg.quorum as f64;
        acc.retention += o.honest_retained as f64 / o.honest_start.max(1) as f64;
        acc.final_honest += o.honest_final as f64;
        acc.honest_slash_tax += if o.honest_earned > 0 {
            o.honest_slashed as f64 / o.honest_earned as f64
        } else {
            0.0
        };
        acc.honest_margin_per_answer += usdc(o.honest_pnl as f64) / answers;
        if cfg.cartel_workers > 0 {
            acc.cartel_excess_per_id_per_1k += usdc((o.cartel_pnl - honest_cf.cartel_pnl) as f64)
                / cfg.cartel_workers as f64
                / resolved_k;
        }
        acc.attacks_per_1k += o.attacks as f64 / resolved_k;
        acc.wrong_rate += o.wrong as f64 / o.resolved.max(1) as f64;
        acc.served_rate += o.resolved as f64 / o.demanded.max(1) as f64;
        acc.platform_mean += usdc(o.platform_mean());
        acc.platform_cv += o.platform_cv();
    }
    let n = seeds as f64;
    for f in [
        &mut acc.retention,
        &mut acc.final_honest,
        &mut acc.honest_slash_tax,
        &mut acc.honest_margin_per_answer,
        &mut acc.cartel_excess_per_id_per_1k,
        &mut acc.attacks_per_1k,
        &mut acc.wrong_rate,
        &mut acc.served_rate,
        &mut acc.platform_mean,
        &mut acc.platform_cv,
    ] {
        *f /= n;
    }
    acc
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

fn header(md: &mut String, title: &str, note: &str, cols: &[&str]) {
    let _ = writeln!(md, "\n## {title}\n\n{note}\n");
    let _ = writeln!(md, "| {} |", cols.join(" | "));
    let _ = writeln!(md, "|{}|", cols.iter().map(|_| "---").collect::<Vec<_>>().join("|"));
}

fn base(o: &Opts) -> Config {
    Config {
        rounds: o.rounds,
        ..Config::default()
    }
}

/// S1: does slashing deter lying, or only tax honest mistakes?
fn sweep_slash(md: &mut String, o: &Opts) {
    header(
        md,
        "S1 — SLASH_BPS sweep (adaptive cartel, 20% of pool, quorum 5, 5% audit)",
        "Fee fixed at 2000 bps. `cartel excess` is cartel P&L minus the same seeds with the cartel playing honestly, per identity per 1000 resolved questions (USDC).",
        &[
            "slash bps",
            "honest retained",
            "honest slash tax",
            "honest margin/answer",
            "cartel excess",
            "attacks/1k",
            "wrong rate",
            "platform rev/round",
            "rev CV",
        ],
    );
    for slash_bps in [0, 100, 250, 500, 1000, 2000, 5000] {
        let cfg = Config {
            params: Params {
                slash_bps,
                ..Params::CONTRACT
            },
            ..base(o)
        };
        let s = summarize(&cfg, o.seeds);
        let _ = writeln!(
            md,
            "| {slash_bps}{} | {} | {} | {:.4} | {:.3} | {:.1} | {} | {:.3} | {:.3} |",
            if slash_bps == 500 { " (current)" } else { "" },
            pct(s.retention),
            pct(s.honest_slash_tax),
            s.honest_margin_per_answer,
            s.cartel_excess_per_id_per_1k,
            s.attacks_per_1k,
            pct(s.wrong_rate),
            s.platform_mean,
            s.platform_cv,
        );
    }
}

/// S2: the same sweep with a stake large enough that the per-question cap
/// (SLASH_CAP_BPS_OF_AMOUNT) binds for every slash rate >= 250 bps.
fn sweep_slash_cap(md: &mut String, o: &Opts) {
    header(
        md,
        "S2 — SLASH_BPS sweep at 100 USDC stakes (per-question cap binds)",
        "Same as S1 but honest and cartel stakes are 100 USDC, so `min(slash_bps x stake, amount)` hits the 0.25 USDC cap from 25 bps upward.",
        &["slash bps", "honest retained", "honest slash tax", "cartel excess", "wrong rate"],
    );
    for slash_bps in [0, 10, 25, 100, 500, 2000] {
        let cfg = Config {
            params: Params {
                slash_bps,
                ..Params::CONTRACT
            },
            honest_stake: 100 * USDC,
            cartel_stake: 100 * USDC,
            ..base(o)
        };
        let s = summarize(&cfg, o.seeds);
        let _ = writeln!(
            md,
            "| {slash_bps}{} | {} | {} | {:.3} | {} |",
            if slash_bps == 500 { " (current)" } else { "" },
            pct(s.retention),
            pct(s.honest_slash_tax),
            s.cartel_excess_per_id_per_1k,
            pct(s.wrong_rate),
        );
    }
}

/// S3: is a 20% platform cut sustainable against worker retention?
fn sweep_fee(md: &mut String, o: &Opts) {
    header(
        md,
        "S3 — PLATFORM_FEE_BPS sweep (no cartel, quorum 5)",
        "Honest workers only; per-answer cost ~U[0.01, 0.03] USDC. Shows where the cut starts pricing marginal workers out and what that does to served demand and revenue.",
        &[
            "fee bps",
            "honest retained",
            "final pool",
            "honest margin/answer",
            "served",
            "platform rev/round",
            "rev CV",
        ],
    );
    for fee_bps in [1000, 1500, 2000, 2500, 3000, 4000, 5000] {
        let cfg = Config {
            params: Params {
                fee_bps,
                ..Params::CONTRACT
            },
            cartel_workers: 0,
            ..base(o)
        };
        let s = summarize(&cfg, o.seeds);
        let _ = writeln!(
            md,
            "| {fee_bps}{} | {} | {:.1} | {:.4} | {} | {:.3} | {:.3} |",
            if fee_bps == 2000 { " (current)" } else { "" },
            pct(s.retention),
            s.final_honest,
            s.honest_margin_per_answer,
            pct(s.served_rate),
            s.platform_mean,
            s.platform_cv,
        );
    }
}

/// S4: quorum size vs cartel share.
fn sweep_quorum(md: &mut String, o: &Opts) {
    header(
        md,
        "S4 — quorum size x cartel share (adaptive cartel, current fee/slash, 5% audit)",
        "Larger quorums split the same 0.8 x amount over more seats, so they also lower honest margin.",
        &["quorum", "cartel share", "cartel excess", "wrong rate", "honest margin/answer", "honest retained"],
    );
    for quorum in [3, 5, 7, 9] {
        for cartel in [10u32, 20, 30] {
            let cfg = Config {
                quorum,
                honest_workers: 100 - cartel,
                cartel_workers: cartel,
                ..base(o)
            };
            let s = summarize(&cfg, o.seeds);
            let _ = writeln!(
                md,
                "| {quorum} | {cartel}% | {:.3} | {} | {:.4} | {} |",
                s.cartel_excess_per_id_per_1k,
                pct(s.wrong_rate),
                s.honest_margin_per_answer,
                pct(s.retention),
            );
        }
    }
}

/// S5: slashing only bites a cartel that gets caught — audit x slash grid.
fn sweep_audit(md: &mut String, o: &Opts) {
    header(
        md,
        "S5 — audit rate x SLASH_BPS: cartel excess P&L (opportunistic cartel)",
        "Cartel excess per identity per 1000 resolved questions, USDC. Negative = attacking loses money versus playing honestly.",
        &["audit", "slash 0", "slash 500 (current)", "slash 2000", "slash 5000"],
    );
    for audit_bps in [0u32, 100, 500, 1000, 2500] {
        let mut row = format!("| {} |", pct(audit_bps as f64 / 10_000.0));
        for slash_bps in [0, 500, 2000, 5000] {
            let cfg = Config {
                params: Params {
                    slash_bps,
                    ..Params::CONTRACT
                },
                audit_bps,
                strategy: Strategy::Opportunistic,
                ..base(o)
            };
            let s = summarize(&cfg, o.seeds);
            let _ = write!(row, " {:.3} |", s.cartel_excess_per_id_per_1k);
        }
        let _ = writeln!(md, "{row}");
    }
}

fn main() {
    let o = parse_args();
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# Incentive-alignment simulation tables\n\n\
         Generated by `cargo run --release -- --seeds {} --rounds {}` in `sim/incentive/` — do not edit by hand.\n\
         Read with [incentive-alignment.md](incentive-alignment.md). Amounts in USDC; every fee, share, dust and\n\
         slash is computed by `sim/incentive/src/math.rs`, which `src/test_incentive_math.rs` checks against\n\
         the real contract. Defaults: 0.25 USDC questions, 10 USDC stakes, 80 honest + 20 cartel workers,\n\
         5% honest error, 0.5 USDC bribe per captured question, 1 USDC per replacement identity, 50 questions/round.",
        o.seeds, o.rounds
    );
    sweep_slash(&mut md, &o);
    sweep_slash_cap(&mut md, &o);
    sweep_fee(&mut md, &o);
    sweep_quorum(&mut md, &o);
    sweep_audit(&mut md, &o);

    match &o.out {
        Some(path) => std::fs::write(path, &md).expect("write --out"),
        None => print!("{md}"),
    }
}
