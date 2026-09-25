#![cfg(test)]
//! Loads the WASM artifacts built by scripts/build-wasm.sh. Tests that need
//! them are #[ignore]d so a plain `cargo test` stays chain- and build-free;
//! CI builds the artifacts and runs `cargo test -- --ignored`.

extern crate std;

use std::{format, path::PathBuf, vec::Vec};

/// The deployable contract, exactly as `stellar contract build` emits it.
pub const RELEASE: &str = "target/wasm32v1-none/release/oracle_escrow.wasm";
/// Same source built with `--features upgrade-test-v2` (version() == 2).
pub const UPGRADE_V2: &str = "target/upgrade-test-v2/wasm32v1-none/release/oracle_escrow.wasm";
/// Same source built with `--features bench-uncapped-quorum`.
pub const UNCAPPED_QUORUM: &str = "target/bench-uncapped-quorum/wasm32v1-none/release/oracle_escrow.wasm";

pub fn load(relative: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), relative].iter().collect();
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}",
            format!(
                "missing WASM artifact {} ({e}); run scripts/build-wasm.sh first",
                path.display()
            )
        )
    })
}
