# X Layer Reth - Context Knowledge Graph

This directory contains structured knowledge about the X Layer Reth project, organized into three domains.

## Domains

| Domain | Path | Description |
|--------|------|-------------|
| [Business](business/README.md) | `business/` | Business context, stakeholders, product goals |
| [Technical](technical/README.md) | `technical/` | Architecture, modules, flows, APIs, conventions |
| [Quality](quality/README.md) | `quality/` | Testing strategy, CI/CD, code quality standards |

## Project Summary

X Layer Reth is a customized Reth (Ethereum execution client) implementation for the X Layer network, an Optimism-based Layer 2 solution. It extends upstream Reth/OP-Reth with:

- **Flashblocks**: Sub-block-time transaction confirmation via incremental block building
- **Legacy RPC Routing**: Transparent routing of historical queries to legacy chain endpoints
- **Bridge Interception**: Selective blocking of PolygonZkEVMBridge transactions in payload building
- **Full-Link Monitoring**: End-to-end transaction lifecycle tracking for observability
- **Custom Chain Specs**: X Layer mainnet (196), testnet (1952), devnet (195) configurations

## How to Use This Knowledge Graph

- Start with `technical/knowledge-base.md` for authoritative rules and constraints.
- Use `technical/terminology.md` when encountering domain-specific terms.
- Refer to `technical/modules/` for per-crate design details.
- Check `technical/pitfalls/` before making changes in sensitive areas.
- Follow `technical/conventions/` for consistent code patterns.
