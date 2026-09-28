# Security Policy and Bug Bounty Scope

## Overview
Arbiter Escrow (`oracle-escrow`) is a Soroban smart contract custodying USDC for human-intelligence oracle questions settled on Stellar. This document establishes the formal bug bounty scope, severity classification rubric, reward tiers, reproduction criteria, and coordinated disclosure process.

## Scope and Target Assets

### In-Scope Contract
| Target Asset | Language / Platform | Description |
| :--- | :--- | :--- |
| `oracle-escrow` (`src/lib.rs`) | Rust / Soroban v23 | Core escrow contract custodying USDC tokens, managing worker stakes, and enforcing question settlement |

### In-Scope Storage Keys and State Transitions
- `DataKey::Admin`: Instance storage key tracking the contract administrator address
- `DataKey::Token`: Instance storage key tracking the custody token address (USDC)
- `DataKey::Platform`: Instance storage key tracking the platform fee recipient address
- `DataKey::TimeoutLedgers`: Instance storage key tracking the default question timeout window
- `DataKey::Question(u64)`: Persistent storage entry tracking question status, amount, payer, and creation ledger
- `DataKey::Stake(Address)`: Persistent storage entry tracking worker staking bond
- `DataKey::Owed(Address)`: Persistent storage entry tracking accrued worker payouts
- `DataKey::Balance(Address)`: Persistent storage entry tracking prepaid payer balances

### In-Scope Public Entrypoints
The following contract entrypoints are subject to security review:
- Lifecycle Initialization: `initialize`
- Escrow Lifecycle: `submit`, `charge`, `resolve`, `refund`, `refund_timeout`
- Balance Management: `deposit`, `withdraw_balance`
- Worker Operations: `stake`, `unstake`, `withdraw`, `withdraw_to`, `touch`
- Administration & Upgrades: `set_admin`, `set_timeout_ledgers`
- State Queries: `get_question`, `get_owed`, `get_stake`, `get_balance`, `get_timeout_ledgers`

### Out-of-Scope Assets and Vectors
The following systems and threat vectors are excluded from this bug bounty program:
- Off-chain backend services (`arbiter-backend`) and off-chain dispatch consensus logic
- Client user interfaces (`arbiter-app`, landing pages, worker consoles)
- Core Stellar protocol, Horizon, RPC infrastructure, and Soroban environment host implementation errors
- Standard Stellar Asset Contract (SAC) token behavior outside direct contract interface interactions
- Known centralization risks documented in the architectural specifications (single-admin settlement authority for `charge`, `resolve`, `refund`, and `set_admin`, bounded by the permissionless `refund_timeout` mechanism)
- Volumetric network denial of service (transaction spamming, fee escalation)
- Vulnerabilities requiring physical access, social engineering, or compromise of off-chain keys without contract logic flaws

## Severity Classification Rubric

Security findings are evaluated based on their concrete financial and operational impact on the `oracle-escrow` smart contract.

| Severity | Definition | Contract Impact Criteria |
| :--- | :--- | :--- |
| Critical | Direct, permanent, or unauthorized loss of funds, or permanent freezing of contract state affecting all users | 1. Unauthorized transfer of contract-held tokens bypassing `require_auth` checks<br>2. Permanent freezing of escrowed question amounts, worker stakes, or balances without recovery<br>3. Invalidation of the permissionless `refund_timeout` mechanism, trapping payer capital<br>4. State desynchronization leading to double withdrawals |
| High | Temporary freezing of funds or systematic economic extraction without complete insolvency | 1. Bypassing worker uniqueness checks in `validate_worker_lists` leading to improper fund allocation<br>2. Arithmetic overflow/underflow or precision errors in fee and slash calculations causing cumulative fund leakage<br>3. Premature execution of `refund_timeout` prior to the snapshotted sequence deadline<br>4. Slashing calculation errors resulting in unauthorized stake forfeiture |
| Medium | State machine inconsistency or operational griefing without direct capital loss | 1. Logic flaws causing questions to enter inconsistent terminal states<br>2. Griefing of persistent storage TTLs leading to premature ledger entry archival<br>3. Partial denial of service affecting an isolated account's ability to withdraw<br>4. State mismatch between `DataKey::Balance` tracking and actual token custody |
| Low / Informational | Minor accounting discrepancies or optimizations with negligible security risk | 1. Inefficient Soroban compute resource usage<br>2. Missing parameter validations that cannot cause state corruption or loss<br>3. Event emission omissions or documentation inaccuracies |

## Severity and Reward Table

Rewards are denominated in USDC. Payouts are sized relative to the economic value at risk and the verified severity rating.

| Severity Level | Reward Range (USDC) | Maximum Cap (% of TVL at Risk) | Required Verification Deliverable |
| :--- | :--- | :--- | :--- |
| Critical | $2,500 – $10,000 | 10% of TVL at Risk | Deterministic unit test demonstrating unauthorized fund transfer or permanent bricking |
| High | $1,000 – $2,500 | 5% of TVL at Risk | Deterministic unit test demonstrating economic extraction, fee distortion, or premature timeout |
| Medium | $250 – $1,000 | N/A | Reproduction unit test proving state corruption or isolated operational disruption |
| Low | $50 – $250 | N/A | Detailed analysis with unit test demonstrating the logic edge case |

## Proof of Concept and Verification Requirements

To qualify for a bounty reward, all submissions must satisfy the following verification criteria:
1. Native Test Suite: Include an automated Rust test case using `soroban-sdk` test utilities (matching the fixture structure in `src/test.rs`).
2. No Mocked Assertions: Reproduction tests must execute against real Soroban SDK contract clients and genuine Stellar asset test configurations (`env.register_stellar_asset_contract_v2`). Altering contract constants or bypassing assertions is strictly prohibited.
3. Minimal State Transition: Provide the exact sequence of contract invocations and ledger sequences required to produce the anomalous state.
4. Impact Assessment: State the affected functions, economic impact calculations, and proposed remediation diffs.

## Coordinated Disclosure Process

1. Submission: Report findings via GitHub Private Vulnerability Reporting or by email to `security@arbiter.xyz`.
2. Acknowledgment: Submissions will be acknowledged within 48 hours of receipt.
3. Triage & Assessment: Technical validity and severity classification will be completed within 5 business days.
4. Remediation & Patch: Fixes will be prepared, audited, and deployed to testnet prior to production settlement.
5. Settlement: Rewards are disbursed upon successful validation and deployment of the remediation patch.
6. Public Disclosure: Coordinated public release occurs 30 days following production deployment.

## Safe Harbor Terms
Security researchers who adhere to this policy, test exclusively against local sandboxes or authorized testnets, and do not disrupt active protocol users are protected under safe harbor terms. Arbiter will not initiate legal action against researchers acting in good faith.
