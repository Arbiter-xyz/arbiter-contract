//! Issue #117: proves the incentive simulation's arithmetic is the
//! contract's arithmetic.
//!
//! sim/incentive/src/math.rs is compiled straight into this test module and
//! every function in it is checked against the real contract: constants
//! against lib.rs's, mul_bps() against OracleEscrow::mul_bps(), and
//! resolve()/slash() against an actual resolve() call in a Soroban Env —
//! per-winner Owed credit, the platform's token delta, and each loser's
//! warming/settled/unbonding buckets after the slash. If lib.rs's
//! settlement formulas change without the simulation following, this fails.

extern crate std;

use super::*;
use proptest::prelude::*;
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger};
use std::vec::Vec as StdVec;

#[path = "../sim/incentive/src/math.rs"]
mod sim_math;

use sim_math::{Params, Stake};

#[test]
fn constants_match_lib_rs() {
    assert_eq!(sim_math::PLATFORM_FEE_BPS, PLATFORM_FEE_BPS);
    assert_eq!(sim_math::SLASH_BPS, SLASH_BPS);
    assert_eq!(sim_math::SLASH_CAP_BPS_OF_AMOUNT, SLASH_CAP_BPS_OF_AMOUNT);
    assert_eq!(sim_math::BPS_DENOM, BPS_DENOM);
    assert_eq!(Params::CONTRACT.fee_bps, PLATFORM_FEE_BPS);
    assert_eq!(Params::CONTRACT.slash_bps, SLASH_BPS);
}

proptest! {
    #[test]
    fn mul_bps_matches(value in 0i128..1_000_000_000_000_000_000_000_000_000i128, bps in 0i128..=10_000) {
        prop_assert_eq!(sim_math::mul_bps(value, bps), OracleEscrow::mul_bps(value, bps));
    }
}

/// A loser to set up on-chain with exactly these buckets at resolve() time.
#[derive(Clone, Copy, Debug)]
struct LoserSpec {
    settled: i128,
    warming: i128,
    unbonding: i128,
}

struct Market {
    env: Env,
    client_id: Address,
    token: Address,
    platform: Address,
    payer: Address,
}

impl Market {
    fn new() -> Market {
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();
        env.ledger().set_sequence_number(1_000);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let client_id = env.register(OracleEscrow, ());
        let admin = Address::generate(&env);
        let platform = Address::generate(&env);
        OracleEscrowClient::new(&env, &client_id).initialize(&admin, &token, &platform, &17_280);
        let payer = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token).mint(&payer, &(i128::MAX / 4));
        Market {
            env,
            client_id,
            token,
            platform,
            payer,
        }
    }

    fn client(&self) -> OracleEscrowClient<'_> {
        OracleEscrowClient::new(&self.env, &self.client_id)
    }

    fn mint(&self, to: &Address, amount: i128) {
        token::StellarAssetClient::new(&self.env, &self.token).mint(to, &amount);
    }

    fn advance(&self, ledgers: u32) {
        let s = self.env.ledger().sequence();
        self.env.ledger().set_sequence_number(s + ledgers);
    }
}

/// Runs one real resolve() and the simulation's resolve() on the same
/// inputs and asserts every observable amount agrees.
fn check_resolve(amount: i128, winners: u32, losers: &[LoserSpec]) {
    let m = Market::new();
    let c = m.client();

    // Settled (+ what will become unbonding) first, matured past warmup...
    let loser_addrs: StdVec<Address> = losers
        .iter()
        .map(|l| {
            let a = Address::generate(&m.env);
            let bonded = l.settled + l.unbonding;
            if bonded > 0 {
                m.mint(&a, bonded);
                c.stake(&a, &bonded);
            }
            a
        })
        .collect();
    m.advance(STAKE_WARMUP_LEDGERS);
    // ...then unbond from settled, then add fresh warming on top.
    for (l, a) in losers.iter().zip(loser_addrs.iter()) {
        if l.unbonding > 0 {
            c.begin_unstake(a, &l.unbonding);
        }
        if l.warming > 0 {
            m.mint(a, l.warming);
            c.stake(a, &l.warming);
        }
        let info = c.get_stake_info(a);
        assert_eq!(
            (info.settled, info.warming, info.unbonding),
            (l.settled, l.warming, l.unbonding),
            "fixture setup"
        );
    }

    let winner_addrs: StdVec<Address> = (0..winners).map(|_| Address::generate(&m.env)).collect();
    c.submit(&m.payer, &1, &amount);

    let tok = token::Client::new(&m.env, &m.token);
    let platform_before = tok.balance(&m.platform);
    let mut w = Vec::new(&m.env);
    for a in &winner_addrs {
        w.push_back(a.clone());
    }
    let mut l = Vec::new(&m.env);
    for a in &loser_addrs {
        l.push_back(a.clone());
    }
    c.resolve(&1, &w, &l);

    let mut sim_losers: StdVec<Stake> = losers
        .iter()
        .map(|l| Stake {
            warming: l.warming,
            settled: l.settled,
            unbonding: l.unbonding,
        })
        .collect();
    let s = sim_math::resolve(amount, winners, &mut sim_losers, Params::CONTRACT);

    for a in &winner_addrs {
        assert_eq!(c.get_owed(a), s.share, "share, amount={amount} n={winners}");
    }
    assert_eq!(
        tok.balance(&m.platform) - platform_before,
        s.platform_take(),
        "platform take, amount={amount} n={winners} losers={losers:?}"
    );
    for (a, sim) in loser_addrs.iter().zip(sim_losers.iter()) {
        let info = c.get_stake_info(a);
        assert_eq!(
            (info.settled, info.warming, info.unbonding),
            (sim.settled, sim.warming, sim.unbonding),
            "loser buckets, amount={amount}"
        );
    }
}

#[test]
fn resolve_matches_without_losers() {
    for amount in [1, 7, 9_999, 10_000, 2_500_000, 10_000_000, 123_456_789] {
        for n in [1, 2, 3, 5, 7, 9, 13] {
            check_resolve(amount, n, &[]);
        }
    }
}

#[test]
fn resolve_matches_with_uncapped_slashes() {
    // 5% of 10 USDC = 0.5 USDC < a 1 USDC question: SLASH_BPS decides.
    let l = LoserSpec {
        settled: 100_000_000,
        warming: 0,
        unbonding: 0,
    };
    check_resolve(10_000_000, 3, &[l, l]);
}

#[test]
fn resolve_matches_when_the_per_question_cap_binds() {
    // 5% of 100 USDC = 5 USDC > a 0.25 USDC question: the cap decides.
    let l = LoserSpec {
        settled: 1_000_000_000,
        warming: 0,
        unbonding: 0,
    };
    check_resolve(2_500_000, 5, &[l]);
}

#[test]
fn resolve_matches_across_bucket_draining_order() {
    // Warming is drained first, then settled, then unbonding.
    let specs = [
        LoserSpec { settled: 50_000_000, warming: 1, unbonding: 0 },
        LoserSpec { settled: 0, warming: 3, unbonding: 40_000_000 },
        LoserSpec { settled: 7, warming: 300_000, unbonding: 90_000_000 },
        LoserSpec { settled: 19, warming: 0, unbonding: 0 },
    ];
    check_resolve(2_500_000, 3, &specs);
}

#[test]
fn resolve_matches_for_unstaked_losers() {
    let none = LoserSpec {
        settled: 0,
        warming: 0,
        unbonding: 0,
    };
    check_resolve(2_500_000, 4, &[none, none]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn resolve_matches_random(
        amount in 1i128..1_000_000_000,
        winners in 1u32..12,
        losers in proptest::collection::vec(
            (0i128..500_000_000, 0i128..50_000_000, 0i128..200_000_000),
            0..4,
        ),
    ) {
        let specs: StdVec<LoserSpec> = losers
            .into_iter()
            .map(|(settled, warming, unbonding)| LoserSpec { settled, warming, unbonding })
            .collect();
        check_resolve(amount, winners, &specs);
    }
}
