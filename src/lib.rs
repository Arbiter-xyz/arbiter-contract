//! # Arbiter Escrow Contract
//!
//! This crate implements the Soroban escrow contract used by the Arbiter
//! oracle system.  The contract is deliberately fail‑closed: every execution
//! path ends in either a `resolve` or a `refund` to guarantee that funds are
//! never left in an indeterminate state.
//!
//! ## New Feature – Severity/Reward Table
//!
//! This addition introduces a public function `reward` that returns the
//! reward amount (in the contract's native token units) associated with a
//! given severity level.  The mapping is defined as follows:
//!
//! | Severity   | Reward |
//! |------------|--------|
//! | Low        | 10     |
//! | Medium     | 50     |
//! | High       | 100    |
//! | Critical   | 500    |
//!
//! The table is immutable and can be queried by any caller.  It is useful
//! for front‑ends and off‑chain services that need to display the bounty
//! amounts without hard‑coding them.

use soroban_sdk::{contractimpl, contracttype, Env, Symbol};

/// Existing contract implementation (placeholder).
/// The real contract code resides elsewhere in this file.
#[contractimpl]
pub struct EscrowContract;

/// Existing methods would be here...
/// ...

/// ---------------------------------------------------------------------------
/// Severity / Reward Table
/// ---------------------------------------------------------------------------

/// Represents the severity level of a question/bounty.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Internal helper that maps a `Severity` to its reward amount.
/// The amounts are expressed in the smallest unit of the token used by the
/// contract (e.g., stroops for XLM or the smallest unit of USDC).
fn severity_to_reward(severity: &Severity) -> i128 {
    match severity {
        Severity::Low => 10,
        Severity::Medium => 50,
        Severity::High => 100,
        Severity::Critical => 500,
    }
}

/// Public contract method that returns the reward for a given severity.
///
/// # Arguments
///
/// * `env` – The Soroban environment.
/// * `severity` – The severity level for which the reward is requested.
///
/// # Returns
///
/// The reward amount as an `i128`.  The function never fails.
#[contractimpl]
impl EscrowContract {
    /// Returns the reward associated with the supplied `severity`.
    ///
    /// This method is `pub` and can be called by any account.  It does not
    /// modify contract state.
    pub fn reward(env: Env, severity: Severity) -> i128 {
        // The environment is not used for the current static table, but we
        // keep it in the signature to conform to Soroban's contract method
        // conventions and to allow future extensions (e.g., dynamic tables).
        let _ = env; // silence unused‑variable warning
        severity_to_reward(&severity)
    }

    // Existing contract methods would continue below...
    // ...
}
