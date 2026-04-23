# XLayerAA (EIP-8130) verification scripts

Manual smoke scripts for validating the XLayerAA tx path against a running
`xlayer-reth-node --chain xlayer-dev --dev` instance.

## Setup

### build and runt the node
```bash
cargo b -p xlayer-reth-node node 
target/debug/xlayer-reth-node node --chain xlayer-dev --dev
```

### send the X Layer AA tx
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
| `GAS_PRICE`| `1000000000`                                                      | legacy-style gas price (wei)      |
| `EXPIRY`   | `0`                                                               | block-ts expiry (0 disables)      |

Output: prints the signed raw tx hex, submits via `eth_sendRawTransaction`,
then polls for the receipt (up to 30s) and prints it. Exits `0` on mined
success, non-zero on revert / timeout / type or sender mismatch.

### Expected outcomes

| stage                                    | status with today's build                                  |
|------------------------------------------|------------------------------------------------------------|
| RLP / 2718 envelope parse (RPC → pool)   | ✅ accepted — returns a tx hash                             |
| K1 native sender auth verification       | ✅ succeeds — ecrecover picks up the signature              |
| Pool admission                           | ✅ admitted — `eth_pendingTransactions` lists it            |
| Block inclusion                          | ✅ mined — receipt carries `type=0x7b`, `status=0x1`        |

On success the script prints:
`==> AA tx mined (block=…, idx=…, gasUsed=…, status=success)`.

## Running against a full L1 + L2 devnet (op-devstack, no Docker)

`devstack/holdalive_test.go` boots an in-process op-stack devnet — L1 geth,
both L2 EL nodes (sequencer + validator, **both running xlayer-reth-node**),
op-node CL on each, op-batcher, op-proposer — and blocks on `SIGINT` so you
can drive it manually with `cast` / `curl`. Useful for A/B/C/D blocker
triage: submit a 0x7B tx to the sequencer and watch both the sequencer's
txpool and the validator's `engine_newPayload` result.

### One-time setup

Tools are pinned via the optimism submodule's `mise.toml`:

| tool  | version  | needed for                            |
|-------|----------|---------------------------------------|
| go    | 1.24.x   | compiling + running the Go test       |
| geth  | 1.16.x   | L1 execution layer (sub-process)      |
| forge | 1.2.3    | compiling op-stack Solidity contracts |
| rust  | 1.92.x   | (already have — for xlayer-reth-node) |

```bash
# one-time: install mise (user-local, safe)
curl -fsSL -o /tmp/mise-install.sh https://mise.run
sh /tmp/mise-install.sh

# pin tools per the monorepo's mise.toml
cd deps/optimism
~/.local/bin/mise trust
~/.local/bin/mise install           # downloads go / geth / rust / forge / ...

# optional: activate mise globally — add to ~/.zshrc:
#   eval "$(~/.local/bin/mise activate zsh)"
#   mise use -g go@1.24.10 "go:github.com/ethereum/go-ethereum/cmd/geth@1.16.4"

# one-time: compile op-stack contracts
cd deps/optimism/packages/contracts-bedrock
forge build --skip '/**/test/**'

# one-time: build xlayer-reth-node (release or debug is fine)
cd /path/to/xlayer-reth
cargo build -p xlayer-reth-node
```

### Launch the devnet

```bash
cd scripts/aa/devstack
./run.sh                    # first run: downloads Go deps (~minutes)
```

Environment variables honored by `run.sh`:

| var                                 | default                                               | description                         |
|-------------------------------------|-------------------------------------------------------|-------------------------------------|
| `RUST_BINARY_PATH_OP_RETH`          | `<repo>/target/debug/xlayer-reth-node`                | xlayer-reth binary used by both ELs |
| `SYSGO_GETH_EXEC_PATH`              | `~/.local/share/mise/installs/.../geth/1.16.4/bin/geth` | L1 EL binary                        |
| `OP_DEVSTACK_PROOF_SEQUENCER_EL`    | `op-reth`                                             | `op-reth` or `op-geth`              |
| `OP_DEVSTACK_PROOF_VALIDATOR_EL`    | `op-reth`                                             | `op-reth` or `op-geth`              |
| `DISABLE_OP_E2E_LEGACY`             | `true`                                                | skip legacy op-e2e artifacts        |

On startup the test prints the L1/L2 user RPCs, Engine API URLs, op-node
RPCs, plus a pre-funded EOA (address + hex private key) you can sign
transactions with. `Ctrl+C` tears the whole stack down.

### Example: send an AA tx to the sequencer

After the devnet prints its endpoints:

```bash
SEQ_RPC="ws://127.0.0.1:xxxxx"     # copy from the banner
VAL_RPC="ws://127.0.0.1:yyyyy"
PRIV="0x..."                         # copy from the banner

# reuse the k1 AA script against the sequencer's RPC
cd scripts/aa
RPC="$SEQ_RPC" PRIV="$PRIV" npx tsx send_k1_eoa_tx.ts

# and verify the validator sees the same receipt
cast tx <hash> --rpc-url "$VAL_RPC"
```

### Triage map

| failure point                                       | blocker | likely fix area                       |
|-----------------------------------------------------|---------|---------------------------------------|
| `eth_sendRawTransaction` rejected at sequencer      | A       | xlayer-reth tx-pool / chainspec fork  |
| sequencer mines; validator returns `INVALID` on newPayload | B | EVM handler non-determinism / state root divergence |
| both accept; batcher or L1 derivation stalls        | C       | derive pipeline / calldata encoding   |
| everything canonical                                | D — ✅  | —                                     |

### Known caveats

**AA fork activation** — op-devstack generates a stock OP-Stack L2 genesis.
XLayerAA (EIP-8130) will only activate if the chainspec code treats it as
part of a fork that is enabled in the generated genesis (or an override is
injected). Until then, a 0x7B tx is expected to fail at blocker A — which
is exactly what this harness is for: verifying the failure mode before we
decide whether to bind AA to an always-on fork or inject a deployer
override.

**Validator `proof-history` panic** — on the very first run the validator
node (which op-devstack starts with `--proofs-history` enabled, see
[sysgo/mixed_runtime.go:313-319](deps/optimism/op-devstack/sysgo/mixed_runtime.go#L313))
panics repeatedly from `crates/chain-state/src/deferred_trie.rs:316` with
`wait_cloned must not be called from a rayon worker thread`. The devnet
keeps serving RPCs (sequencer is unaffected), but the validator's Merkle
proof worker pool is broken. This is an xlayer-reth bug in the proof-history
path, not an op-devstack integration problem. Until it's fixed, rely on the
sequencer RPC for A-stage (tx-pool) triage; for B-stage (follower accepts
the payload) we'll need to fork the preset to start the validator without
`--proofs-history`.
