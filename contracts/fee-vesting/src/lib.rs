#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, Address, Env};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Address allowed to initialize the schedule, rotate the beneficiary, and rescue excess tokens.
    Admin,
    /// Address that receives unlocked fee revenue (DAO treasury / multisig).
    Beneficiary,
    /// SEP-41 token being vested.
    Token,
    /// Ledger timestamp at which vesting begins.
    Start,
    /// Ledger timestamp before which nothing unlocks.
    Cliff,
    /// Ledger timestamp at which 100% of `Total` is unlocked.
    End,
    /// Total token amount subject to the schedule.
    Total,
    /// Cumulative amount already transferred out via `claim_vested`.
    Claimed,
}

/// Public snapshot of the configured vesting schedule and current claim progress.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VestingSchedule {
    pub admin: Address,
    pub beneficiary: Address,
    pub token: Address,
    pub start: u64,
    pub cliff: u64,
    pub end: u64,
    pub total: i128,
    pub claimed: i128,
}

#[contract]
pub struct FeeVesting;

#[contractimpl]
impl FeeVesting {
    /// Configure the vesting schedule. Callable once by `admin`.
    ///
    /// `start` is the vesting start timestamp, `cliff` the timestamp at
    /// which the first tokens unlock, and `end` the timestamp at which
    /// the entire `total` is unlocked. Requires `start <= cliff <= end` and `total > 0`.
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
        admin.require_auth();

        if start > cliff || cliff > end {
            panic!("invalid schedule");
        }
        if total <= 0 {
            panic!("invalid total");
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Beneficiary, &beneficiary);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Start, &start);
        env.storage().instance().set(&DataKey::Cliff, &cliff);
        env.storage().instance().set(&DataKey::End, &end);
        env.storage().instance().set(&DataKey::Total, &total);
        env.storage().instance().set(&DataKey::Claimed, &0i128);
    }

    /// Amount currently unlocked by the schedule but not yet claimed.
    pub fn claimable_amount(env: Env) -> i128 {
        let vested = Self::vested_amount(&env);
        let claimed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Claimed)
            .unwrap_or(0);
        vested - claimed
    }

    /// Alias for `claimable_amount`.
    pub fn claimable(env: Env) -> i128 {
        Self::claimable_amount(env)
    }

    /// Return the configured admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized")
    }

    /// Return the full vesting schedule configuration and claimed total.
    pub fn get_schedule(env: Env) -> VestingSchedule {
        let store = env.storage().instance();
        VestingSchedule {
            admin: store.get(&DataKey::Admin).expect("not initialized"),
            beneficiary: store.get(&DataKey::Beneficiary).expect("not initialized"),
            token: store.get(&DataKey::Token).expect("not initialized"),
            start: store.get(&DataKey::Start).expect("not initialized"),
            cliff: store.get(&DataKey::Cliff).expect("not initialized"),
            end: store.get(&DataKey::End).expect("not initialized"),
            total: store.get(&DataKey::Total).expect("not initialized"),
            claimed: store.get(&DataKey::Claimed).unwrap_or(0),
        }
    }

    /// Rotate the beneficiary address. Gated by `admin` authorization.
    pub fn set_beneficiary(env: Env, new_beneficiary: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::Beneficiary, &new_beneficiary);
        env.events()
            .publish((symbol_short!("benefic"), admin), new_beneficiary);
    }

    /// Recover tokens sent in excess of the unvested/unclaimed schedule reserve,
    /// or tokens of a different asset accidentally transferred to this contract.
    /// Gated by `admin` authorization.
    pub fn rescue_tokens(env: Env, token: Address, to: Address, amount: i128) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();

        if amount <= 0 {
            panic!("invalid amount");
        }

        let schedule_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("not initialized");
        let token_client = token::Client::new(&env, &token);

        if token == schedule_token {
            let total: i128 = env
                .storage()
                .instance()
                .get(&DataKey::Total)
                .expect("not initialized");
            let claimed: i128 = env
                .storage()
                .instance()
                .get(&DataKey::Claimed)
                .unwrap_or(0);
            let reserved = (total - claimed).max(0);
            let balance = token_client.balance(&env.current_contract_address());
            if balance - reserved < amount {
                panic!("insufficient excess balance");
            }
        }

        token_client.transfer(&env.current_contract_address(), &to, &amount);
        env.events()
            .publish((symbol_short!("rescued"), token, to), amount);
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
        if now >= end {
            return total;
        }
        // Cliff-then-linear: nothing before the cliff, then linear from
        // `start` to `end` (the cliff only gates the first unlock).
        let elapsed = (now - start) as i128;
        let span = (end - start) as i128;
        let q = total / span;
        let r = total % span;
        q * elapsed + (((r as u128) * (elapsed as u128)) / (span as u128)) as i128
    }

    /// Transfer the currently-unlocked amount to the beneficiary.
    /// Reverts when nothing is claimable.
    pub fn claim_vested(env: Env) -> i128 {
        let amount = Self::claimable_amount(env.clone());
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
        env.events()
            .publish((symbol_short!("claimed"), beneficiary.clone()), amount);

        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &amount,
        );

        amount
    }

    /// Same as `claim_vested`, but the caller must be the beneficiary.
    pub fn claim_vested_as(env: Env, caller: Address) -> i128 {
        caller.require_auth();

        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap();
        if caller != beneficiary {
            panic!("not beneficiary");
        }

        Self::claim_vested(env)
    }
}

mod test;
