# Module: xlayer_version

**Path**: `crates/version/`
**Purpose**: Version metadata initialization composing X Layer + upstream Reth versions

## Components

### Version Initialization (`lib.rs`)
- `init_version!()` macro: Captures `CARGO_PKG_NAME` from calling crate and passes to `init_version_metadata()`
- `init_version_metadata(name)`: Constructs `RethCliVersionConsts` combining:
  - X Layer package version (`CARGO_PKG_VERSION`)
  - Upstream Reth version (`default_reth_version_metadata()`)
  - Git SHA from build environment (`VERGEN_GIT_SHA`, `VERGEN_GIT_SHA_SHORT`)
  - Long version string with upstream version info

### Version String Format
- `cargo_pkg_version`: `"{reth_version}/{xlayer_version}"`
- `p2p_client_version`: `"{reth_p2p_version}/{xlayer_client_version}"`
- `extra_data`: `"{reth_extra_data}/{xlayer_client_version}"`
- Client version constant: `xlayer/v{CARGO_PKG_VERSION}`

## Usage

MUST be called exactly once at the start of every binary's `main()`:
```rust
xlayer_version::init_version!();
```

Both `bin/node/src/main.rs` and `bin/tools/main.rs` call this before any other initialization.

## Build-Time Environment Variables

| Variable | Source |
|----------|--------|
| `CARGO_PKG_VERSION` | Cargo package version |
| `CARGO_PKG_NAME` | Cargo package name |
| `VERGEN_GIT_SHA` | Full git commit SHA |
| `VERGEN_GIT_SHA_SHORT` | Short git commit SHA (8 chars) |
| `RETH_SHORT_VERSION` | Short version string |
| `RETH_LONG_VERSION_0..3` | Long version string segments |

Note: `#![no_std]` crate — uses `alloc` only.
