#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

/// Storage keys for the fee-vesting treasury contract.
///
/// The schedule is fixed at `initialize()` time and cannot be mutated
/// afterwards: there is no governance entry point that rewrites it.
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Address allowed to (re)initialize the schedule. Set once.
    Admin,
    /// Address that receives unlocked fee revenue (DAO treasury / multisig).
    Beneficiary,
    /// Token contract used to pay out vested revenue.
    Token,
    /// Ledger timestamp at which vesting begins.
    Start,
    /// Ledger timestamp before which nothing is claimable (cliff).
    Cliff,
    /// Ledger timestamp at which the full amount is unlocked.
    End,
    /// Total amount of fee revenue subject to the schedule.
    Total,
    /// Amount already claimed by the beneficiary.
    Claimed,
}

#[contract]
pub struct FeeVesting;

#[contractimpl]
impl FeeVesting {
    /// Configure the vesting schedule. Callable once by the deployer.
    ///
    /// `start` is the vesting start timestamp, `cliff` the timestamp at
    /// which the first tokens unlock, and `end` the timestamp at which
    /// the entire `total` is unlocked. Requires `start <= cliff <= end`.
    pub fn initialize(
        env: Env,
        admin: Address,
        beneficiary: Address,
        token: Address,
        start: u64,
        cliff: u64,
        end: u64,
        total: i128,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        if start > cliff || cliff > end {
            panic!("invalid schedule");
        }
        if total < 0 {
            panic!("invalid total");
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Beneficiary, &beneficiary);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Start, &start);
        env.storage().instance().set(&DataKey::Cliff, &cliff);
        env.storage().instance().set(&DataKey::End, &end);
        env.storage().instance().set(&DataKey::Total, &total);
        env.storage().instance().set(&DataKey::Claimed, &0i128);
    }

    /// Amount currently unlocked by the schedule but not yet claimed.
    pub fn claimable(env: Env) -> i128 {
        let vested = Self::vested_amount(&env);
        let claimed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Claimed)
            .unwrap_or(0);
        vested - claimed
    }

    /// Total amount unlocked by the schedule at the current ledger time.
    pub fn vested_amount(env: &Env) -> i128 {
        let start: u64 = env.storage().instance().get(&DataKey::Start).unwrap();
        let cliff: u64 = env.storage().instance().get(&DataKey::Cliff).unwrap();
        let end: u64 = env.storage().instance().get(&DataKey::End).unwrap();
        let total: i128 = env.storage().instance().get(&DataKey::Total).unwrap();

        let now = env.ledger().timestamp();
        if now < cliff {
            return 0;
        }
        if now >= end || end == start {
            return total;
        }
        // Cliff-then-linear: nothing before the cliff, then linear from
        // `start` to `end` (the cliff only gates the first unlock).
        let elapsed = (now - start) as i128;
        let span = (end - start) as i128;
        total * elapsed / span
    }

    /// Transfer the currently-unlocked amount to the beneficiary.
    /// Reverts when nothing is claimable.
    pub fn claim_vested(env: Env) -> i128 {
        let amount = Self::claimable(env.clone());
        if amount <= 0 {
            panic!("nothing to claim");
        }

        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap();
        let token: Address = env.storage().instance().get(&DataKey::Token).unwrap();

        let claimed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Claimed)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::Claimed, &(claimed + amount));

        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &amount,
        );

        amount
    }
}

mod test;
