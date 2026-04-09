# Conventions: Feature Types and Conditional Compilation

## Feature Flags

### `testing`
- **Crate**: `xlayer_builder`
- **Usage**: `#[cfg(any(test, feature = "testing"))]`
- **Enables**: Test utilities module (`tests/`), mock transaction factory, test framework macros
- **Purpose**: Allows downstream crates and integration tests to use builder test utilities without being in a `#[cfg(test)]` context

### Pattern: Test-Only Modules

```rust
// In lib.rs
#[cfg(any(test, feature = "testing"))]
pub mod tests;
```

This pattern gates test utilities behind either the `test` cfg (unit tests) or the `testing` feature (integration tests, other crates).

## Conditional Compilation Patterns

### Sequencer vs RPC Mode

Not compile-time — determined at runtime via `--xlayer.sequencer-mode` flag:

```rust
// Runtime branching
if args.xlayer_args.builder.flashblocks.enabled {
    // Sequencer: flashblocks payload builder
} else {
    // RPC: standard payload builder + flashblocks receiver
}
```

### Debug/Dev Mode

```rust
let dev_mode = builder.config().dev.dev;
if dev_mode {
    // DebugNodeLauncher wrapping EngineNodeLauncher
    Either::Left(builder.launch_with(DebugNodeLauncher::new(...)))
} else {
    Either::Right(builder.launch_with(EngineNodeLauncher::new(...)))
}
```

### Optional Features

- **Flashblocks subscription**: Enabled via `--xlayer.flashblocks-subscription` flag
- **Legacy RPC**: Enabled when `--rpc.legacy-url` is provided
- **Bridge intercept**: Enabled via `--xlayer.intercept.enabled` flag
- **Monitor**: Enabled via `--xlayer.monitor.enable` flag

All are runtime-conditional, not compile-time features.

## Lint Configuration

```rust
#![allow(missing_docs, rustdoc::missing_crate_level_docs)]  // bin crates
#![cfg_attr(not(test), warn(unused_crate_dependencies))]     // lib crates
#![cfg_attr(docsrs, feature(doc_cfg))]                       // docs.rs support
#![no_std]                                                    // version crate only
```
