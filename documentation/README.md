# X Layer Reth Node Documentation

This folder contains architecture and optimization documentation for the `xlayer-reth` node.

## Contents

- [bin/node Architecture & Optimization Guide](./bin-node-architecture.md) — Deep-dive into the `bin/node` code structure, key design patterns, and concrete recommendations for optimizing the architecture.

## Background

`xlayer-reth` is an [OKX X Layer](https://www.okx.com/xlayer) Optimism rollup node built on top of [Reth](https://github.com/paradigmxyz/reth). The binary entry point lives in `bin/node` and wires together a set of modular crates into a fully operational execution client.

The design philosophy closely mirrors projects like [base/base](https://github.com/base/base/tree/main/bin/node): keep `main.rs` as a thin orchestration layer while pushing every distinct concern into its own crate.
