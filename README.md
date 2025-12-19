# XLayer Reth

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.88+-orange.svg)](https://www.rust-lang.org/)
[![GitHub release](https://img.shields.io/github/v/release/okx/xlayer-reth)](https://github.com/okx/xlayer-reth/releases)


## Overview

XLayer Reth is a customized implementation of [Reth](https://github.com/paradigmxyz/reth) optimized for the XLayer network, an Optimism-based Layer 2 solution.

This project provides a high-performance, production-ready Ethereum execution client tailored for XLayer's specific requirements. It builds upon the upstream Reth codebase with custom optimizations and features for the XLayer network.

### Why built on top of Reth

XLayer Reth is built on top of [Reth](https://github.com/paradigmxyz/reth), extending it with XLayer-specific functionality:

- **High Performance & Full OP Support**: Leverage Reth's blazing-fast execution engine and complete Optimism feature set out of the box
- **XLayer Customization**: Implement XLayer-specific features and optimizations independently without impacting upstream development
- **Seamless Upstream Sync**: Easily integrate the latest Reth updates, improvements, and security patches
- **Ecosystem Contribution**: Rapidly experiment with new features and bug fixes, contributing valuable improvements back to upstream Reth

## Getting Started

### Prerequisites

- **Rust**: Version 1.88 or higher
- **[just](https://github.com/casey/just)**: Command runner (install with `cargo install just`)
- **Docker** (optional): For containerized builds

### Building from Source

#### Standard Release Build

```bash
# Install just command runner
cargo install just

# List all available commands
just

# Standard release build
just build

# Maximum performance build (recommended for production)
just build-maxperf
```

#### Build Profiles

| Profile | Command | Description |
|---------|---------|-------------|
| `release` | `just build` | Thin LTO, optimized for fast builds |
| `maxperf` | `just build-maxperf` | Fat LTO, single codegen unit, jemalloc - ideal for production |

#### Install to System

```bash
# Install standard release build to ~/.cargo/bin
just install

# Install maximum performance build
just install-maxperf
```

After installation, run the node from anywhere:

```bash
xlayer-reth-node --help
```

### Docker Build

Build a Docker image with the following command:

```bash
# Build Docker image (tagged with git commit hash)
just build-docker

# Build with custom suffix
just build-docker mysuffix

# The image will be tagged as:
# - op-reth:<git-hash>
# - op-reth:latest
```

## Initialization

Before running the node for the first time, you need to initialize the database with the genesis block.

```bash
xlayer-reth-node init --chain /path/to/genesis.json --datadir /data/xlayer
```

> **Note**: The `init` command only needs to be run once before the first start. It creates the database and writes the genesis block.

## Configuration

XLayer Reth inherits all configuration options from [Reth](https://reth.rs/) and [OP Reth](https://github.com/paradigmxyz/reth). Run `xlayer-reth-node --help` for a complete list.

Below are the XLayer-specific configuration options:

```bash
# XLayer Options
--xlayer.enable-innertx              # Enable inner transaction capture and storage (default: false)

# Legacy RPC Routing
--rpc.legacy-url <URL>               # Legacy RPC endpoint for historical data
--rpc.legacy-timeout <DUR>           # Timeout for legacy RPC requests (default: 30s)

```

## Development

### Development Commands

```bash
# Run all checks (format + clippy + tests)
just check

# Run tests
just test

# Run tests including e2e tests
just test true

# Auto-fix formatting and clippy issues
just fix

# Watch mode - auto-run tests on file changes
just watch-test
```

## Testing
### End-to-end Testing

To run end-to-end (e2e) tests, first build ``xlayer-reth`` Docker image in this repo:
```
just build-docker
```

Next, you need to start a devnet using [xlayer-toolkit](https://github.com/okx/xlayer-toolkit/blob/main/devnet/README.md). Make sure you set the following environment variables in ``xlayer-toolkit/devnet/example.env``:
```
SEQ_TYPE=reth
RPC_TYPE=reth
ENABLE_INNERTX_RPC=true
```

After devnet is started, run the e2e test:
```
cargo test -p xlayer-e2e-test --test e2e_tests -- --nocapture --test-threads=1
# or
just test true
```

### Flashblocks Tests
Similar to e2e tests, first build ``xlayer-reth`` Docker image in this repo:
```
just build-docker
```

Next, you need to start a devnet using [xlayer-toolkit](https://github.com/okx/xlayer-toolkit/blob/main/devnet/README.md). Make sure you set the following environment variables in ``xlayer-toolkit/devnet/example.env``:
```
FLASHBLOCK_ENABLED=true
FLASHBLOCK_P2P_ENABLED=true
```

Also, start the 2nd RPC node under ``xlayer-toolkit/devnet``:
```
./scripts/run-rpc2.sh
```

Then, in this repo, run:
```
cargo test -p xlayer-e2e-test --test flashblocks_tests -- --nocapture --test-threads=1
# or
just test false true
```

To run all flashblocks tests (including ignored tests, also requires 2nd RPC node to be running), run:
```
cargo test -p xlayer-e2e-test --test flashblocks_tests -- --include-ignored --nocapture --test-threads=1
```

## Contributing

We welcome contributions! Please follow these steps:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Run checks before committing (`just check`)
4. Commit your changes (`git commit -m 'Add amazing feature'`)
5. Push to the branch (`git push origin feature/amazing-feature`)
6. Open a Pull Request

### Setup Pre-commit Hook

```bash
just xlayer
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.