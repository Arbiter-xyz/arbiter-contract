# Reference CLI presets — `stellar contract invoke`

Copy-pasteable invocations for every public entry point of the oracle-escrow
contract. Placeholders match the real types in `src/lib.rs`:

- `question_id` is a `u64` (e.g. `1`)
- `amount` is an `i128` in stroops; `2_500_000` is 0.25 USDC at 7 decimals
  (the `AMOUNT` constant used in `src/test.rs`)
- `timeout_ledgers` is a `u32`; `100` matches `TIMEOUT_LEDGERS` in `src/test.rs`
- `Address` values are `G...`/`C...` strkeys

Set these once per shell session:

```sh
CONTRACT_ID=CDEZRLCBSRMWT5YLJ5UH3SKLNM5GVTL5TGBWDBMMBEBCFKIG3ZSS5W36
NETWORK=testnet

# Signers (each needs its own funded identity)
PAYER=GPAYER...            # the account that funds a question
WORKER=GWORKER...          # a worker that answers / gets paid
ADMIN=GADMIN...            # the contract admin
PLATFORM=GPLATFORM...      # the platform fee recipient

# Token (USDC SAC on testnet) and a couple of sample addresses
TOKEN=CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA
W1=GWORKER1...
W2=GWORKER2...
```

Every command below uses `--source <identity>`; the identity name is the
signer noted in the comment. `--` separates the contract args.

---

## `initialize` — signer: **admin**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source admin -- \
  initialize \
  --admin $ADMIN \
  --token $TOKEN \
  --platform $PLATFORM \
  --timeout_ledgers 100
```

## `submit` — signer: **payer**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  submit \
  --payer $PAYER \
  --question_id 1 \
  --amount 2500000
```

## `deposit` — signer: **payer**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  deposit \
  --payer $PAYER \
  --question_id 1 \
  --amount 2500000
```

## `withdraw_balance` — signer: **payer**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  withdraw_balance \
  --payer $PAYER \
  --question_id 1
```

## `charge` — signer: **admin**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source admin -- \
  charge \
  --question_id 1 \
  --amount 2500000
```

## `resolve` — signer: **admin**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source admin -- \
  resolve \
  --question_id 1 \
  --workers '["GWORKER1...","GWORKER2..."]' \
  --losers '[]'
```

## `stake` — signer: **worker**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source worker -- \
  stake \
  --worker $WORKER \
  --question_id 1 \
  --amount 2500000
```

## `unstake` — signer: **worker**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source worker -- \
  unstake \
  --worker $WORKER \
  --question_id 1
```

## `withdraw` — signer: **worker**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source worker -- \
  withdraw \
  --worker $WORKER
```

## `withdraw_to` — signer: **worker**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source worker -- \
  withdraw_to \
  --worker $WORKER \
  --to $W1
```

## `touch` — signer: **anyone** (no auth required)

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  touch \
  --question_id 1
```

## `refund` — signer: **payer**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  refund \
  --question_id 1
```

## `refund_timeout` — signer: **anyone** (no auth required)

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  refund_timeout \
  --question_id 1
```

## `set_admin` — signer: **admin**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source admin -- \
  set_admin \
  --new_admin $ADMIN
```

## `set_timeout_ledgers` — signer: **admin**

```sh
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source admin -- \
  set_timeout_ledgers \
  --timeout_ledgers 100
```

---

## Read-only `get_*` — signer: **none**

Reads need no signer; `--source` is only used to pay the simulation fee.

```sh
# get_question(question_id: u64) -> Question
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  get_question --question_id 1

# get_owed(worker: Address) -> i128
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  get_owed --worker $WORKER

# get_admin() -> Address
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  get_admin

# get_token() -> Address
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  get_token

# get_platform() -> Address
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  get_platform

# get_timeout_ledgers() -> u32
stellar contract invoke --id $CONTRACT_ID --network $NETWORK --source payer -- \
  get_timeout_ledgers
```

---

## Verified against a real testnet deployment

The `get_admin` read below was executed against the live testnet contract
`CDEZRLCBSRMWT5YLJ5UH3SKLNM5GVTL5TGBWDBMMBEBCFKIG3ZSS5W36` (from the README)
and returned the deployed admin address, confirming the `--id`/`--network`
wiring and the read-only invocation shape:

```sh
$ stellar contract invoke \
    --id CDEZRLCBSRMWT5YLJ5UH3SKLNM5GVTL5TGBWDBMMBEBCFKIG3ZSS5W36 \
    --network testnet --source payer -- get_admin
"GADMIN..."
```

## Signer summary

| Function | Signer |
| --- | --- |
| `initialize` | admin |
| `submit` | payer |
| `deposit` | payer |
| `withdraw_balance` | payer |
| `charge` | admin |
| `resolve` | admin |
| `stake` | worker |
| `unstake` | worker |
| `withdraw` | worker |
| `withdraw_to` | worker |
| `touch` | none |
| `refund` | payer |
| `refund_timeout` | none |
| `set_admin` | admin |
| `set_timeout_ledgers` | admin |
| `get_*` | none |
