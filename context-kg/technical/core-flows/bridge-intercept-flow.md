# Core Flow: Bridge Transaction Interception

## Overview

The bridge intercept mechanism allows the sequencer to selectively exclude bridge transactions from built blocks. It operates post-execution within the flashblocks payload builder.

## Flow

```
Flashblocks Builder executing transactions
       │
       ▼
For each transaction:
  1. Execute transaction via EVM
  2. Collect execution logs
  3. Call intercept_bridge_transaction_if_need(logs, sender, config)
       │
       ├── config.enabled == false → Allow (skip all checks)
       │
       ├── For each log:
       │     ├── log.address != bridge_contract → Skip log
       │     ├── parse_bridge_event(log) fails → Skip log (debug log)
       │     │
       │     ├── config.wildcard == true:
       │     │     └── INTERCEPT: BridgeInterceptError::WildcardBlock
       │     │
       │     └── event.origin_address == target_token:
       │           └── INTERCEPT: BridgeInterceptError::TargetTokenBlock
       │
       └── No matching logs → Allow
       │
       ▼
If intercepted:
  - Transaction is excluded from the flashblock payload
  - Execution state is rolled back
  - Debug log records the interception
  - Builder continues with next transaction

If allowed:
  - Transaction is included in the flashblock payload
  - Execution state is committed
```

## BridgeEvent ABI Layout

```solidity
event BridgeEvent(
    uint8   leafType,              // slot 0  (offset   0-31)
    uint32  originNetwork,         // slot 1  (offset  32-63)
    address originAddress,         // slot 2  (offset  64-95)  → data[76..96]
    uint32  destinationNetwork,    // slot 3  (offset  96-127)
    address destinationAddress,    // slot 4  (offset 128-159)
    uint256 amount,                // slot 5  (offset 160-191) → data[160..192]
    bytes   metadata,              // slot 6  (offset 192-223, dynamic)
    uint32  depositCount           // slot 7  (offset 224-255)
);
```

Event signature: `keccak256("BridgeEvent(uint8,uint32,address,uint32,address,uint256,bytes,uint32)")`
= `0x501781209a1f8899323b96b4ef08b168df93e0a90c673d1e4cce39366cb62f9b`

## Modes

| Mode | Behavior |
|------|----------|
| **Disabled** (`enabled: false`) | No interception; all transactions pass through |
| **Wildcard** (`wildcard: true`) | Any `BridgeEvent` from the configured bridge contract triggers interception |
| **Target Token** (`wildcard: false`) | Only intercepts if `originAddress` matches `target_token_address` |

## Scope Limitations

- Only active in sequencer mode during payload building
- Does NOT reject transactions at the pool level
- Does NOT affect block validation on follower nodes
- Does NOT affect RPC transaction forwarding to sequencer
- Intercepted transactions remain in the mempool and may be retried in future blocks
