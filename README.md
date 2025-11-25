# XLayer Reth

XLayer Reth is a customized implementation of [Reth](https://github.com/paradigmxyz/reth) optimized for the XLayer network, an Optimism-based Layer 2 solution.

## Overview

This project provides a high-performance, production-ready Ethereum execution client tailored for XLayer's specific requirements. It builds upon the upstream Reth codebase with custom optimizations and features for the XLayer network.

### Why We Maintain a Fork

XLayer Reth uses a [fork of Reth](https://github.com/okx/reth) instead of depending directly on upstream for the following reasons:

- **Custom Features**: Ability to implement XLayer-specific features and optimizations that may not be suitable for upstream
- **Rapid Development**: Control over the codebase allows us to merge critical changes quickly without waiting for upstream review cycles
- **Flexibility**: Direct access to modify internal code when needed for urgent fixes or network-specific requirements
- **Stability**: Independence from upstream breaking changes while still being able to selectively integrate improvements

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

We use [just](https://github.com/casey/just) as our command runner. Install it with:

```bash
cargo install just
```

### Build Commands

```bash
# List all available commands
just

# Standard release build
just build

# Maximum performance build (with jemalloc)
just build-maxperf

# Clean build artifacts
just clean
```

### Install Commands

Install the binary to `~/.cargo/bin` (or `$CARGO_HOME/bin`):

```bash
# Install standard release build
just install

# Install maximum performance build
just install-maxperf
```

After installation, you can run the node from anywhere:

```bash
xlayer-reth-node --help
```

### Development Commands

```bash
# Run all checks (format + clippy + tests)
just check

# Run tests
just test

# Auto-fix formatting and clippy issues
just fix

# Watch mode - auto-run tests on file changes
just watch-test
```

## Other Tests

For other tests, please read [tests/README.md](tests/README.md).

## Repository

- **Homepage**: [https://github.com/okx/xlayer-reth](https://github.com/okx/xlayer-reth)
- **License**: MIT

## Requirements

- Rust 1.88 or higher
- Edition 2024 features
