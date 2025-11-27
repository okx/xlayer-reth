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

## Working with Local Reth Dependencies

XLayer Reth depends on the [OKX Reth fork](https://github.com/okx/reth). For development, you can work with either git dependencies or a local Reth clone.

### Using Git Dependencies (Default)

By default, dependencies are fetched from the git repository as specified in `Cargo.toml`:

```bash
# Standard build using git dependencies
just build
```

If you have a local `.cargo/config.toml` file from previous development work, remove it to use git dependencies:

```bash
rm -rf .cargo
cargo build --release
```

### Using Local Reth Dependencies

For active development on both XLayer Reth and Reth simultaneously, you can use local path dependencies:

```bash
# Setup local reth and build (creates .cargo/config.toml with patches)
just build-dev /path/to/your/local/reth

# Setup local reth WITHOUT building (useful for config-only changes)
just build-dev /path/to/your/local/reth false

# Subsequent builds will use the existing .cargo/config.toml
just build-dev

# Or just setup config without building
just build-dev "" false
```

The `build-dev` command:
1. Creates a `.cargo/config.toml` file with `[patch]` directives
2. Redirects all Reth git dependencies to your local Reth directory
3. Allows you to test local Reth changes without pushing to GitHub
4. Optionally skips the build step with the second parameter set to `false`

### Verifying Dependency Sources

To check whether your build is using local or remote dependencies:

```bash
# Check where reth dependencies are sourced from
cargo tree -i reth
```

This will show:
- **Local dependencies**: Paths like `file:///Users/you/path/to/reth/...`
- **Remote dependencies**: Git URLs like `https://github.com/okx/reth?branch=...#<commit-hash>`

### Managing the Dev Template

The `.reth-dev.toml` template defines the path mappings for local Reth dependencies. Keep it in sync with `Cargo.toml`:

```bash
# Check if .reth-dev.toml is in sync with Cargo.toml
just check-dev-template

# Auto-sync .reth-dev.toml with Cargo.toml dependencies
just sync-dev-template /path/to/your/local/reth
```

**What these commands do:**

- `check-dev-template`: Verifies that all Reth dependencies in `Cargo.toml` have corresponding entries in `.reth-dev.toml`, and flags any extra entries that should be removed
- `sync-dev-template`: Automatically updates `.reth-dev.toml` by:
  - Scanning your local Reth repository to find actual crate locations
  - Adding new dependencies from `Cargo.toml`
  - Removing dependencies no longer in `Cargo.toml`
  - Preserving correct path mappings (handles cases where crate names differ from folder names, e.g., `reth-errors` lives in `crates/errors`)

**When to use these commands:**

- After adding or removing Reth dependencies in `Cargo.toml`
- When upgrading to a new Reth version with different crate structure
- If `build-dev` fails due to missing or incorrect path mappings

### Switching Between Git and Local Dependencies

```bash
# Switch to local dependencies
just build-dev /path/to/local/reth

# Switch back to git dependencies
rm -rf .cargo
cargo build --release

# Or use the standard build command (which auto-removes .cargo)
just build
```

## Repository

- **Homepage**: [https://github.com/okx/xlayer-reth](https://github.com/okx/xlayer-reth)
- **License**: MIT

## Requirements

- Rust 1.88 or higher
- Edition 2024 features
