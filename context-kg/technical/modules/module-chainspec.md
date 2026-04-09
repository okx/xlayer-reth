# Module: xlayer_chainspec

**Path**: `crates/chainspec/`
**Purpose**: X Layer network chain specifications and hardfork schedules

## Components

### Chain Specifications
- **`xlayer_mainnet.rs`**: Chain ID 196, genesis block 42810021, genesis hash `0xdc33d8...`
- **`xlayer_testnet.rs`**: Chain ID 1952, genesis block 12241700, genesis hash `0xccb16e...`
- **`xlayer_devnet.rs`**: Chain ID 195, genesis block 18696116, dynamically loaded hash/state root from resource files

### Hardfork Definitions (`lib.rs`)
Three static hardfork lists: `XLAYER_MAINNET_HARDFORKS`, `XLAYER_TESTNET_HARDFORKS`, `XLAYER_DEVNET_HARDFORKS`

All hardforks through Isthmus are activated at genesis (block 0 / timestamp 0). Jovian has network-specific activation timestamps:
- Mainnet: `1764691201` (2025-12-02 16:00:01 UTC)
- Testnet: `1764327600` (2025-11-28 11:00:00 UTC)
- Devnet: `1764327600` (2025-11-28 11:00:00 UTC)

Base fee params: denominator 100,000,000, elasticity multiplier 1.

### Parser (`parser.rs`)
`XLayerChainSpecParser` implements `ChainSpecParser`:
- Recognizes `xlayer-mainnet`, `xlayer-testnet`, `xlayer-devnet` as chain names
- Falls through to standard OP chain spec parsing for other chains
- Supports `legacyXLayerBlock` genesis config field override via environment variable
- Performs genesis header validation (hash matches expected)

## Key Rules

1. Hardfork order MUST match upstream OP-Reth exactly (see `knowledge-base.md` Rule 2).
2. Adding a new hardfork requires updating all three hardfork lists.
3. The `legacyXLayerBlock` field in genesis config determines the cutoff for legacy RPC routing.
4. Genesis numbers are non-zero because X Layer chains were migrated from a legacy chain at a specific block height.
