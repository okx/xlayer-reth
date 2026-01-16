# XLayer Poseidon Precompile Integration

## 1. Background

XLayer is an Optimism-based L2 that requires custom precompiled contracts (Poseidon hash at address `0x100`). 
The challenge is to integrate these custom precompiles into a Reth-based node **without modifying upstream Reth code**.

### Why This Matters
- **Upstream Compatibility**: Keep the ability to sync with upstream Reth updates
- **Clean Separation**: Custom logic lives in workspace crates, not in forked dependencies
- **Maintainability**: Changes are isolated and easier to review

### Key Constraint
Reth's EVM configuration is tightly coupled through the `ConfigureEvm` trait. We need to inject custom 
precompiles at EVM creation time without touching the upstream `reth-optimism-evm` crate.

## 2. Solution Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  bin/node/src/main.rs                                               │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ Node Builder                                                   │  │
│  │   .with_components(                                            │  │
│  │       xlayer_node.components()                                 │  │
│  │           .executor(XLayerExecutorBuilder)  ◄─── Custom!       │  │
│  │   )                                                             │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              │ build_evm()
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/node/src/lib.rs                                             │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ XLayerExecutorBuilder::build_evm()                             │  │
│  │   returns: XLayerEvmConfig                                     │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/evm/src/config.rs                                           │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ XLayerEvmConfig                                                │  │
│  │   wraps: OpEvmConfig<..., XLayerEvmFactory>  ◄─── Key!         │  │
│  │                                                                 │  │
│  │   OpEvmConfig::new_with_evm_factory(                           │  │
│  │       chain_spec,                                              │  │
│  │       receipt_builder,                                         │  │
│  │       XLayerEvmFactory ◄─── Custom EVM factory                 │  │
│  │   )                                                             │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              │ create_evm() / create_evm_with_inspector()
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/evm/src/evm_factory.rs                                      │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ XLayerEvmFactory::create_evm()                                 │  │
│  │   1. let mut evm = OpEvmFactory::default().create_evm(...)     │  │
│  │   2. *evm.components_mut().2 = xlayer_precompiles_map(spec)    │  │
│  │      ▲                                                          │  │
│  │      └── Replaces precompiles without touching OpEvm!          │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              │ xlayer_precompiles(hardfork)
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/evm/src/factory.rs                                          │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ xlayer_precompiles(hardfork: OpHardfork)                       │  │
│  │   returns: &'static Precompiles                                │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/evm/src/precompiles/mod.rs                                  │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ XLayerPrecompiles::precompiles()                               │  │
│  │   if hardfork >= Jovian:                                       │  │
│  │       xlayer_with_poseidon()  ◄── Adds Poseidon                │  │
│  │   else:                                                         │  │
│  │       Precompiles::latest()                                    │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/evm/src/precompiles/poseidon.rs                             │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ pub const POSEIDON: Precompile                                 │  │
│  │   address: 0x0000000000000000000000000000000000000100          │  │
│  │   run: poseidon_run()                                          │  │
│  │                                                                 │  │
│  │ fn poseidon_run(input, gas_limit) -> PrecompileResult          │  │
│  │   - Validates input length (multiple of 32)                    │  │
│  │   - Calculates gas: BASE + PER_INPUT * num_inputs              │  │
│  │   - Executes: poseidon_hash(input)                             │  │
│  │   - Returns 32-byte hash                                       │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

### Key Design Points

1. **No Upstream Changes**: All custom code lives in `crates/evm/`, `crates/node/`, and `bin/node/`
2. **Wrapper Pattern**: `XLayerEvmConfig` wraps `OpEvmConfig` with a custom EVM factory
3. **Factory Injection**: `XLayerEvmFactory` delegates to `OpEvmFactory` then swaps precompiles
4. **Static Precompiles**: `xlayer_precompiles()` returns `&'static Precompiles` for performance
5. **Hardfork Activation**: Poseidon is only active for `OpHardfork::Jovian` and later

## 3. Key Code Call Stack

### Startup Path (Node Launch → EVM Creation)

```
main()
  ├─ NodeBuilder::with_components(xlayer_node.components().executor(XLayerExecutorBuilder))
  │
  └─ XLayerExecutorBuilder::build_evm(ctx)
      │
      └─ xlayer_evm_config(ctx.chain_spec())
          │
          └─ XLayerEvmConfig::new(chain_spec)
              │
              └─ OpEvmConfig::new_with_evm_factory(
                     chain_spec,
                     OpRethReceiptBuilder::default(),
                     XLayerEvmFactory::default()  ◄── Custom factory injected here
                 )
```

### Transaction Execution Path (TX → Precompile)

```
BlockExecutor::execute_and_verify_one()
  │
  ├─ EvmFactory::create_evm(db, env)
  │   │
  │   └─ XLayerEvmFactory::create_evm(db, env)
  │       ├─ let mut evm = OpEvmFactory::default().create_evm(db, env)
  │       ├─ let spec_id = env.cfg_env.spec()
  │       └─ *evm.components_mut().2 = xlayer_precompiles_map(spec_id)
  │           │
  │           └─ xlayer_precompiles(hardfork_from_spec_id(spec_id))
  │               │
  │               └─ XLayerPrecompiles::new(hardfork).precompiles()
  │                   │
  │                   └─ if hardfork >= Jovian:
  │                          xlayer_with_poseidon()  ◄── Poseidon added here
  │                       else:
  │                          Precompiles::latest()
  │
  └─ evm.transact()
      │
      └─ [if tx.to == 0x100]
          │
          └─ poseidon_run(input, gas_limit)
              ├─ Validate input (multiple of 32 bytes)
              ├─ Calculate gas: 60 + 6 * num_inputs
              ├─ poseidon_hash(input) → 32-byte output
              └─ return PrecompileOutput::new(gas_used, output)
```

### Verification

Test the precompile with:

```bash
# Call Poseidon at 0x100 with single input (0x01)
cast call 0x0000000000000000000000000000000000000100 \
  0x0000000000000000000000000000000000000000000000000000000000000001 \
  --rpc-url http://localhost:8124 \
  --gas-limit 100000

# Estimate gas
cast rpc eth_estimateGas \
  '{"to":"0x0000000000000000000000000000000000000100","data":"0x0000000000000000000000000000000000000000000000000000000000000001"}' \
  --rpc-url http://localhost:8124
```

Or run the automated test:

```bash
./tests/test-precompile.sh
```