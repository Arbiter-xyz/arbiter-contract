#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Env as _},
    Env, Symbol,
};

use crate::{EscrowContract, Severity};

#[test]
fn test_reward_table() {
    // Set up a test environment.
    let env = Env::default();

    // Helper to call the contract's `reward` method.
    fn get_reward(env: &Env, sev: Severity) -> i128 {
        EscrowContract::reward(env.clone(), sev)
    }

    // Verify each severity maps to the expected reward.
    assert_eq!(get_reward(&env, Severity::Low), 10);
    assert_eq!(get_reward(&env, Severity::Medium), 50);
    assert_eq!(get_reward(&env, Severity::High), 100);
    assert_eq!(get_reward(&env, Severity::Critical), 500);
}
