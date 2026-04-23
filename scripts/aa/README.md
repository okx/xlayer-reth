# XLayerAA (EIP-8130) verification scripts

Manual smoke scripts for validating the XLayerAA tx path against a running
`xlayer-reth-node --chain xlayer-dev --dev` instance.

## Setup

```bash
cd scripts/aa
npm install
```

## Scripts

### `send_k1_eoa_tx.ts`

Submits a minimum-viable K1-native (EOA) EIP-8130 transaction: a single
`[to, data=0x]` call, `from = None` (sender recovered via ecrecover from
`sender_auth`), self-pay, no expiry. This exercises the smallest complete
AA path — envelope → pool structural validator → native K1 verify →
execution → receipt.

```bash
# defaults: RPC http://127.0.0.1:18545, xlayer-dev pre-funded rich key
npm run send-k1

# or directly
npx tsx send_k1_eoa_tx.ts
```

Env overrides:

| var        | default                                                           | description                       |
|------------|-------------------------------------------------------------------|-----------------------------------|
| `RPC`      | `http://127.0.0.1:18545`                                          | JSON-RPC endpoint                 |
| `PRIV`     | `0x4bbbf85c…cbf4356` (xlayer-dev rich key)                        | sender secp256k1 private key      |
| `TO`       | `0x00…01`                                                         | single-call target                 |
| `NONCE_KEY`| `0`                                                               | 2D-nonce channel (u256 decimal)   |
| `NONCE_SEQ`| (queried from node)                                               | 2D-nonce sequence                 |
| `GAS_LIMIT`| `200000`                                                          | execution gas budget              |
| `MAX_FEE`  | `1000000000`                                                      | max fee per gas (wei)             |
| `EXPIRY`   | `0`                                                               | block-ts expiry (0 disables)      |

Output: prints the signed raw tx hex, submits via `eth_sendRawTransaction`,
then polls for the receipt and prints it. Exits `0` on success, non-zero
on revert / timeout.

### Expected outcomes

| stage                                    | status with today's build                                  |
|------------------------------------------|------------------------------------------------------------|
| RLP / 2718 envelope parse (RPC → pool)   | ✅ accepted — returns a tx hash                             |
| K1 native sender auth verification       | ✅ succeeds — ecrecover picks up the signature              |
| Pool admission                           | ✅ admitted — `eth_pendingTransactions` lists it            |
| Block inclusion                          | ⚠️ pending — the dev-mode miner currently rejects the      |
|                                          | post-fork payload with "base fee missing" (see the M6c     |
|                                          | punch list in `docs/xlayer-aa.md`). The tx stays in the    |
|                                          | pool until a compatible builder path lands.                |

Getting a tx hash back (no RPC error) is the primary success signal for
this script; the receipt poll will time out until the dev-mode miner is
taught to finalize blocks containing the XLayerAA predeploy upgrade tx
emitted at genesis.
