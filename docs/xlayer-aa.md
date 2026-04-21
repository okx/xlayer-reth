# XLayerAA — Implementation Notes

## Mistakes

Running log of mistakes made during XLayerAA implementation, so we don't repeat them. Append new entries at the top.

### 2026-04-21 — Phase execution loop did not skip remaining phases on failure

EIP-8130 Block execution: *"if any call in a phase reverts, all state changes for that phase are discarded and remaining phases are skipped."*

The initial port reverted the failing phase's checkpoint but let the outer `for phase in &parts.call_phases` loop continue, so subsequent phases still executed:

```rust
for phase in &parts.call_phases {
    // ...inner call loop sets phase_ok = false on revert and breaks...
    if phase_ok {
        accumulated_refunds += phase_refunds;
    } else {
        evm.ctx().journal_mut().checkpoint_revert(checkpoint);
    }
    phase_results.push(...);
    // ⚠️ no break — next phase runs anyway
}
```

**Why it's wrong:** Sponsor-gated flows rely on this: phase 0 performs a sponsor-paid setup call, phase 1 does the user's actual action. If phase 1 reverts, phase 2..N must NOT run — they may depend on phase 1's writes or repeat its side effects. Continuing silently violates the atomicity contract the spec gives to verifier contracts.

**Fix:** after pushing the failed phase's result, `break` out of the outer loop.

**Lesson:** EIP-8130 phase semantics are "atomic per phase, sequential across phases, halt on first failure". When implementing a phased execution loop, the halt-on-failure needs an explicit `break` — revert-but-continue is not equivalent.

### 2026-04-21 — `thread_local!` as handler → precompile bridge when the data is derivable from `tx`

Initial port cargo-culted base-revm's pattern: a thread-local `XLayerAATxContext` that the handler populated in `validate_against_state_and_deduct_caller` and the TxContext precompile read in `run_tx_context_precompile` for `getMaxCost()` / `getGasLimit()`.

```rust
thread_local! { static XLAYERAA_TX_CONTEXT: RefCell<Option<XLayerAATxContext>> = ... }

// handler:
set_xlayeraa_tx_context(XLayerAATxContext::new(&parts, execution_gas_limit, known_intrinsic, max_fee));

// precompile:
let max_cost = get_xlayeraa_tx_context().map_or(U256::ZERO, |ctx| ctx.max_cost);
```

**Why it's wrong:** I defended the thread-local as necessary because the handler computes `max_cost` and `gas_limit` and the precompile can't reach handler state. But those values are just deterministic formulas over `tx.xlayeraa_parts()` and `tx.max_fee_per_gas()`:

```
gas_limit = tx.gas_limit - parts.aa_intrinsic_gas
max_cost  = (gas_limit + parts.aa_intrinsic_gas - parts.payer_intrinsic_gas
             + parts.custom_verifier_gas_cap) × tx.max_fee_per_gas
```

All inputs are inclusion-time immutable data available to the precompile through `context.tx()`. There is no dynamic handler state involved. Tempo's AA design avoids this entirely — derived values live on the tx env and are computed on demand.

**Fix:** delete the thread-local, the `XLayerAATxContext` struct, and the `set_*` / `clear_*` / `get_*` functions. Compute on demand in the precompile via two small helpers (`aa_execution_gas_limit`, `aa_max_cost`).

**Lesson for future "handler → precompile" plumbing:** before reaching for a thread-local / transient-storage bridge, ask whether the value is already a pure function of `tx + parts`. If yes, compute it at the call site — no global state, no clear-before-every-tx invariant to maintain, no cross-tx pollution risk.

### 2026-04-21 — `U256::from_limbs([N, 0, 0, 0])` for small-integer constants

```rust
pub const NONCE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);
pub const EXPIRING_SEEN_BASE_SLOT: U256 = U256::from_limbs([2, 0, 0, 0]);
```

**Why it's wrong:** same readability problem as byte-array addresses. `from_limbs` exposes alloy's internal little-endian limb representation to every reader; nobody wants to decode that.

**Fix:** use the `uint!` macro (re-exported from `revm::primitives::uint`).

```rust
pub const NONCE_BASE_SLOT: U256 = uint!(1_U256);
pub const EXPIRING_SEEN_BASE_SLOT: U256 = uint!(2_U256);
```

### 2026-04-21 — Hand-rolled ABI encoding / selectors instead of `sol!`

The first port of the precompile layer hand-wrote `abi.encode` for every return type:

```rust
pub fn selector(sig: &[u8]) -> [u8; 4] { let h = keccak256(sig); [h[0], h[1], h[2], h[3]] }
pub fn encode_address(a: Address) -> Bytes { /* left-pad 20 bytes to 32 */ }
pub fn encode_calls_abi(phases: &[Vec<XLayerAACall>]) -> Bytes { /* 60-line head/tail offset math */ }

// dispatch:
if selector_bytes == selector(b"getNonce(address,uint256)") { /* manually slice 4+12..4+32 */ }
```

And the storage-slot helpers were written byte-by-byte into fixed buffers:

```rust
pub fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = { let mut buf = [0u8; 64]; buf[12..32].copy_from_slice(account.as_slice()); /* ... */ };
    // etc.
}
```

**Why it's wrong:**

- every encoder is a re-derivation of the Solidity ABI spec, which is exactly what `alloy-sol-types` already generates from a `sol!` block — bugs in offset math, padding, or selector case are uniquely painful because the EVM just sees "wrong bytes";
- manual dispatch via `selector(b"getNonce(...)")` re-hashes on every precompile call, and loses type information about arguments;
- storage-slot helpers are Solidity mapping layout (`keccak256(key ‖ slot)`), which `SolValue::abi_encode((key, slot))` expresses in one line.

**Fix:** declare the interface once with `sol!`, then use `SolCall::SELECTOR`, `SolCall::abi_decode`, `SolCall::abi_encode_returns`, and `SolValue::abi_encode` for keys.

```rust
sol! {
    struct CallTuple { address target; bytes data; }
    interface INonceManager {
        function getNonce(address account, uint256 nonceKey) external view returns (uint256);
    }
    interface ITxContext {
        function getCalls() external view returns (CallTuple[][] memory);
        // ...
    }
}

// dispatch:
match selector {
    ITxContext::getSenderCall::SELECTOR => ITxContext::getSenderCall::abi_encode_returns(&sender),
    // ...
}

// slot helpers:
pub fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = keccak256((account, NONCE_BASE_SLOT).abi_encode());
    U256::from_be_bytes(keccak256((nonce_key, inner).abi_encode()).0)
}
```

The `sol!` block becomes the single source of truth; encoder/decoder/selector all derive from it.

### 2026-04-21 — `Address::new([byte, ...])` instead of hex `address!("0x...")`

When porting base-revm's handler and precompile addresses, I used the byte-array form:

```rust
pub const NONCE_MANAGER_ADDRESS: Address =
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xaa, 0x02]);
```

**Why it's wrong:** byte arrays are unreadable at a glance — you have to count nibbles to see the actual address. The ecosystem (alloy, reth, op-revm) uniformly uses the `address!("0x...")` macro for constant addresses, which is a checksummed hex literal that survives review.

**Fix:** always use `address!("0x...")` for constant addresses.

```rust
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");
```
