# XLayer Reth

XLayer Reth is a customized implementation of [Reth](https://github.com/paradigmxyz/reth) optimized for the XLayer network, an Optimism-based Layer 2 solution.

## Overview

This project provides a high-performance, production-ready Ethereum execution client tailored for XLayer's specific requirements. It builds upon the upstream Reth codebase with custom optimizations and features for the XLayer network.

## Architecture

XLayer Reth is structured as a Rust workspace with the following components:

- **xlayer-reth-node**: The main binary crate that runs the XLayer Reth node

## Dependencies

### Core Components

- **Reth**: Based on [OKX's Reth fork](https://github.com/okx/reth) at version 1.9.2
- **Revm**: EVM implementation (v31.0.2)
- **Alloy**: Ethereum library primitives (v1.0.41)
- **OP Alloy**: Optimism-specific extensions (v0.22.0)

### Key Features

- Optimism rollup support via `reth-optimism-*` crates
- Full async runtime powered by Tokio
- JSON-RPC support via `jsonrpsee`
- Comprehensive metrics and tracing capabilities

## Build Profiles

### Release Profile
Mimics the upstream Reth release profile:
- Thin LTO for faster builds
- Optimized for production use
- Stripped symbols for smaller binaries

### Maxperf Profile
Maximum performance build:
- Fat LTO for maximum optimization
- Single codegen unit
- Ideal for production deployments where build time is not a concern

## Building

```bash
# Standard release build
cargo build --release

# Maximum performance build
cargo build --profile maxperf

# Check the project
cargo check -p xlayer-reth-node
```

## Repository

- **Homepage**: [https://github.com/okx/xlayer-reth](https://github.com/okx/xlayer-reth)
- **License**: MIT

## Requirements

- Rust 1.88 or higher
- Edition 2024 features
