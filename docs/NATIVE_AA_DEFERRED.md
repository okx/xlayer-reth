# Native AA — Deferred Items

Items that cannot be fully tested or implemented without external dependencies.
These do not block subsequent phases.

## Phase 6

### ~~Step 2: System Contract Bytecode Embedding~~ — DONE

Completed: Base's compiled bytecodes are now embedded in `eip8130-consensus/src/bytecode/`
via `include_str!()`. See `system_bytecodes.rs` for deployer addresses, constructor arg
encoding, and gas limits. Deployed contract addresses in `predeploys.rs` updated to
Base's deterministic `deployer.create(0)` values.

### Step 2: Upgrade Deposit TX Generation (Go op-node)

- **Item**: Go op-node must generate upgrade deposit transactions at NativeAA activation
- **Reason**: Upgrade deposit tx generation is the CL (Go op-node) responsibility, not EL. See design doc §4.2 and §12.
- **Prerequisite**: Go op-node changes in the `op-dev/optimism` repo
- **Current state**: EL accepts and executes standard `TxDeposit` transactions (OP Stack baseline); Go-side generation not yet implemented. EL now has all bytecodes and deployer constants ready for the Go side to reference.

### ~~Step 2: Post-Deployment Bytecode Validation~~ — Unblocked

- **Item**: Validate that system contract bytecodes at expected addresses match the expected compiled output
- **Reason**: Bytecodes are now embedded; full runtime validation can be implemented
- **Current state**: Bytecodes embedded, constructor arg encoding tested. Runtime bytecode equality check can be added when needed.

## Phase 7 Integration Points

### Wire `validate_block_no_aa_tx` into consensus/engine path

- **Item**: Call `validate_block_no_aa_tx` during block body validation for all incoming blocks (not just builder-produced ones)
- **Reason**: Pool-side gating in `best_transactions_merged` only prevents the local builder from producing pre-activation AA blocks. Peer-sent blocks bypass the pool and go through the consensus/engine path.
- **Where**: Either a custom `OpConsensus` wrapper or the `XLayerEngineValidator`
- **Current state**: Function defined with 6 tests; builder-side gating active; consensus path not yet wired

### Wire `Eip8130Precompiles.aa_enabled` in EVM configuration

- **Item**: Set `aa_enabled = true` when constructing the EVM for blocks where NativeAA is active
- **Reason**: `Eip8130Precompiles` defaults to `aa_enabled: false`. The integration into the node's EVM config (replacing the standard precompile provider) happens when the full AA execution pipeline is wired in.
- **Current state**: Gating mechanism complete; EVM config integration pending
