# resolve() resource limits and MAX_QUORUM_SIZE

`resolve()` is the only entrypoint whose cost grows with its input. Per
call it writes one `Owed` entry per matching worker and reads and writes
one `Stake` entry per staked losing worker. It also runs an O(n²)
duplicate check across both lists. Without a cap, a big enough quorum
fails at the network's resource limits. That failure happens during
simulation or submission, not as a contract error, and the only way out
for that question would be a smaller retry or a refund.

The contract now rejects `workers.len() + losing_workers.len() >
MAX_QUORUM_SIZE` with `QuorumTooLarge` (error 15). The check runs before
the duplicate check and before any storage is touched, so an oversized
call fails cheaply and leaves the question pending for a correctly sized
retry.

**MAX_QUORUM_SIZE = 64.**

## How it was measured

[`src/test_resources.rs`](../src/test_resources.rs) holds the harness:

- It runs the **real WASM build** inside the real host
  (`soroban-env-host` 23), so VM instantiation, WASM execution, storage
  and the token's own transfer cost are all metered. The in-process Rust
  build would miss most of that.
- For each quorum size it measures three splits: all matching; one
  matching plus the rest losing; and half/half. It measures each split
  twice, once with first-time workers (new `Owed` entries) and once with
  returning workers (existing entries). Every losing worker is staked,
  since a staked loser costs a write and an unstaked one only a read. Cost
  only rises with each list's length, and the duplicate check does
  T(T-1)/2 comparisons for any split of T. So these splits bound every
  other split.
- It compares the results against the **live mainnet limits** in
  [`bench/mainnet-soroban-settings.json`](../bench/mainnet-soroban-settings.json).
  That's a snapshot of the network's `ConfigSettingEntry` values, fetched
  with `scripts/fetch-network-settings.sh`. The committed snapshot is from
  2026-09-25, ledger 64,605,088, protocol 28.
- CPU and memory are counted twice: once with the host's built-in cost
  model, and once by re-pricing the host's per-cost-type counters with
  mainnet's live `ContractCostParams`. The higher number is the one that
  has to fit, so a cost-model change voted in on the network shows up once
  the snapshot is refreshed.
- Things the test host can't see are added explicitly. `mock_all_auths`
  skips signature checks, so every measurement gets +1 M instructions,
  +2 footprint entries and +1 write entry, which is enough for admin auth
  by address credentials (ed25519 verify, account read, nonce write).
  Transaction size is estimated conservatively: 1 KiB envelope, arguments
  counted twice (operation plus auth entry), and 128 bytes per footprint
  key.

## Results

Mainnet per-transaction limits at the snapshot: 400,000,000 instructions,
40 MiB memory, 400 footprint entries, 200 disk-read entries, **200 write
entries**, 200,000 disk-read bytes, 132,096 write bytes, 16,384 event bytes,
132,096 tx bytes.

Worst case per size (instructions: host / mainnet re-priced; "binding" is
the limit closest to being hit, including the auth allowance):

| quorum | worst split | instructions | memory | footprint | writes | write bytes | tx size (est.) | binding |
|---:|---|---:|---:|---:|---:|---:|---:|---|
| 1 | 1 + 0 | 0.92 M / 0.91 M | 1.27 MB | 10 | 5 | 964 | 2,712 | memory 3.0% |
| 8 | 8 + 0 returning | 1.67 M / 1.67 M | 1.33 MB | 17 | 12 | 1,944 | 4,168 | writes 6.5% |
| 32 | 32 + 0 returning | 6.18 M / 6.15 M | 1.77 MB | 41 | 36 | 5,304 | 9,160 | writes 18.5% |
| **64** | **64 + 0 returning** | **16.6 M / 16.5 M** | **2.92 MB** | **73** | **68** | **9,784** | **15,816** | **writes 34.5%** |
| 96 | 96 + 0 returning | 31.9 M / 31.7 M | 4.70 MB | 105 | 100 | 14,264 | 22,472 | writes 50.5% |
| 128 | 128 + 0 returning | 52.1 M / 51.7 M | 7.11 MB | 137 | 132 | 18,744 | 29,128 | writes 66.5% |
| 192 | 192 + 0 returning | 107 M / 106 M | 13.8 MB | 201 | 196 | 27,704 | 42,440 | writes 98.5% |
| 224 | 224 + 0 returning | 142 M / 141 M | 18.1 MB | 233 | 228 | 32,184 | 49,096 | **writes 114.5%** |
| 384 | 384 + 0 returning | 388 M / 385 M | 49.2 MB | 393 | 388 | 54,584 | 82,376 | writes 194.5%, memory 117% |

What the numbers show:

- **Write entries are the binding limit.** resolve() writes T + 4 entries,
  measured: one per worker, plus a constant 4 that includes the question
  and the contract's and platform's token balances. Add one for an
  address-credential nonce. Against mainnet's 200 that gives a hard
  ceiling of 195.
- Instructions and memory grow faster than linearly, because of the
  duplicate check and because returning workers read existing entries.
  Instructions stay under 5% of the limit at 64. They'd only bind around
  T ≈ 390, where memory also runs out.
- Disk reads are 0. Every entry resolve() touches is live Soroban state,
  which protocol 23+ serves from memory.
- Host and mainnet cost models agree to within about 1%.

Exact search over all splits (binary search in the harness):

| ceiling | size |
|---|---:|
| hard: largest size inside every limit | 195 |
| safe: largest size within 50% of every limit | 95 |
| **enforced MAX_QUORUM_SIZE** | **64** |

64 sits below the safe ceiling. At that size resolve() uses at most 34.5%
of any mainnet limit. That leaves room for network limits to shrink, for
the contract to grow (one more write per worker at 64 would be 68%, still
legal, and the CI gate would flag it), and for anything the test host
misses. It is still far above any realistic quorum, since pricing tiers
use a handful of workers.

## The CI regression gate

`resolve_at_max_quorum_stays_within_safety_margin_of_mainnet_limits` runs
resolve() at exactly `MAX_QUORUM_SIZE` on the **deployable** WASM, for
every worst-case split. It fails if any dimension exceeds
`SAFETY_FRACTION` (50%) of the mainnet limit. Anything that pushes
worst-case cost over the line fails CI: raising the cap, adding storage
work per worker, a heavier token, or a refreshed snapshot with tighter
limits. It was checked by temporarily setting the cap to 100: the gate
failed on write entries (105 > 100).

Native tests that run in plain `cargo test` pin the guard itself: exactly
64 settles, 65 is rejected with `QuorumTooLarge` in every split without
touching state, and the size check wins over the O(n²) duplicate check.

## Re-running it

```sh
scripts/fetch-network-settings.sh              # refresh bench/mainnet-soroban-settings.json
scripts/build-wasm.sh                          # deployable WASM + fixtures
cargo test bench_resolve_resource_sweep -- --ignored --nocapture   # full table + ceilings
cargo test resolve_at_max_quorum -- --ignored --nocapture          # the CI gate
```

The sweep runs on a fixture build with the cap lifted (`--features
bench-uncapped-quorum`). It also asserts `MAX_QUORUM_SIZE` is at or below
the measured safe ceiling.

Refresh the snapshot, rerun the sweep and update this file whenever the
network votes new Soroban settings in. The local stellar-cli (v26) warns
that it predates protocol 28 and may miss newer config entries. Every
limit used here was present in the snapshot.
