# Business Knowledge Domain

## Product Context

X Layer is an Optimism-based Layer 2 network operated by OKX. X Layer Reth serves as the execution client for this network, replacing the default OP-Reth with X Layer-specific customizations.

## Key Stakeholders

- **X Layer Network Operators**: Run sequencer and RPC nodes using this binary
- **X Layer Users**: Interact via RPC endpoints; benefit from flashblocks for faster confirmations
- **OKX Infrastructure Team**: Maintains and deploys the node software
- **Upstream Reth/OP-Reth**: The open-source base that X Layer Reth extends

## Business Goals

1. **Fast Transaction Confirmation**: Flashblocks provide sub-block-time confirmation (~200ms intervals) to users before canonical block finalization.
2. **Seamless Migration**: Legacy RPC routing enables transparent access to pre-migration historical data without requiring users to change endpoints.
3. **Bridge Security**: Bridge transaction interception allows operators to selectively block specific bridge operations for compliance or security.
4. **Operational Visibility**: Full-link monitoring tracks transaction lifecycle from RPC receipt through block inclusion for SLA and debugging.
5. **Network Compatibility**: Custom chain specs ensure correct hardfork activation and genesis configuration for X Layer networks.

## Network Identifiers

| Network | Chain ID | Status |
|---------|----------|--------|
| X Layer Mainnet | 196 | Production |
| X Layer Testnet | 1952 | Staging |
| X Layer Devnet | 195 | Development |

## Operational Modes

- **Sequencer Mode** (`--xlayer.sequencer-mode`): Produces blocks and flashblocks; runs payload builder with P2P broadcast.
- **RPC Mode** (default): Follows chain; receives flashblocks from sequencer via WebSocket; serves user queries.
