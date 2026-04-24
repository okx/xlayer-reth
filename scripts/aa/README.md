# XLayerAA (EIP-8130) verification scripts

Manual smoke scripts for validating the XLayerAA tx path.

---

## Quick start — single EL (`--dev` mode)

The fastest path: one node, no L1, uses the built-in dev chain.

```bash
# build
cargo build -p xlayer-reth-node

# run
target/debug/xlayer-reth-node node --chain xlayer-dev --dev

# send AA tx (defaults to http://127.0.0.1:18545, xlayer-dev rich key)
cd scripts/aa
npm install
npm run send-k1
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

---

## Full L1 + L2 devnet (op-devstack, no Docker)

`devstack/holdalive_test.go` boots an in-process op-stack devnet — L1 geth,
both L2 EL nodes (sequencer + validator, **both running xlayer-reth-node**),
op-node CL on each, op-batcher, op-proposer — and blocks on `SIGINT` so you
can drive it manually with `cast` / `curl`. Useful for A/B/C/D blocker
triage: submit a 0x7B tx to the sequencer and watch both the sequencer's
txpool and the validator's `engine_newPayload` result.

### Quick start

- stop the devnet if it's running
```
ps -eo pid,cmd | grep -E "devstack\.test|xlayer-reth-node" | grep -v grep
kill related process
```

```bash
# step 1 — build the node (mold avoids OOM on large debug binaries)
RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo build -p xlayer-reth-node -j4

# step 2 — start the devnet (new terminal)
cd scripts/aa/devstack
./run.sh          # first run downloads Go deps (~minutes); wait for banner

# step 3 — send an AA tx once the banner appears (another terminal) to the sequencer
#   copy port + key from the banner printed by run.sh
cd scripts/aa
PRIV="" # copy from banner
RPC="http://127.0.0.1:{L2 sequencer  EL UserRPC}" PRIV=""$PRIV" npx tsx send_k1_eoa_tx.ts

# step 4 - check receipt from the validator
hash="0x..."
val_port="46647"  #L2 validator  EL UserRPC
curl -s "http://127.0.0.1:$val_port" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getTransactionReceipt","params":["'"$hash"'"],"id":1}' \
  | python3 -m json.tool
```

The banner looks like:

```
======================================================================
XLAYER AA DEVNET READY — press Ctrl+C to tear down
======================================================================
L2 sequencer  EL UserRPC   ws://127.0.0.1:40025
L2 validator  EL UserRPC   ws://127.0.0.1:34465
Funded EOA priv hex  0x<key>
======================================================================
```

Use `http://` not `ws://` for the RPC env var — the port serves both protocols.

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
```

### `run.sh` environment variables

| var                                 | default                                               | description                         |
|-------------------------------------|-------------------------------------------------------|-------------------------------------|
| `RUST_BINARY_PATH_OP_RETH`          | `<repo>/target/debug/xlayer-reth-node`                | xlayer-reth binary used by both ELs |
| `SYSGO_GETH_EXEC_PATH`              | `~/.local/share/mise/installs/.../geth/1.16.4/bin/geth` | L1 EL binary                        |
| `OP_DEVSTACK_PROOF_SEQUENCER_EL`    | `op-reth`                                             | `op-reth` or `op-geth`              |
| `OP_DEVSTACK_PROOF_VALIDATOR_EL`    | `op-reth`                                             | `op-reth` or `op-geth`              |
| `DISABLE_OP_E2E_LEGACY`             | `true`                                                | skip legacy op-e2e artifacts        |

`Ctrl+C` tears the whole stack down.

### Verify the validator

```bash
cast tx <hash> --rpc-url "http://127.0.0.1:<val-port>"
```

### Triage map

| failure point                                       | blocker | likely fix area                       |
|-----------------------------------------------------|---------|---------------------------------------|
| `eth_sendRawTransaction` rejected at sequencer      | A       | xlayer-reth tx-pool / chainspec fork  |
| sequencer mines; validator returns `INVALID` on newPayload | B | EVM handler non-determinism / state root divergence |
| both accept; batcher or L1 derivation stalls        | C       | derive pipeline / calldata encoding   |
| everything canonical                                | D — ✅  | —                                     |

### Known caveats

**AA fork activation** — the AA handler runs unconditionally (no fork gate),
so 0x7B txs are accepted on the stock OP-Stack genesis generated by
op-devstack without any chainspec override.

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



