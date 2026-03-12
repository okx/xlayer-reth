# XLayer Reth - Claude Instructions

XLayer Reth is a customized Reth (Ethereum execution client) implementation for the XLayer network, an Optimism-based Layer 2 solution. It is written in Rust and builds on top of the upstream Reth and OP-Reth codebase.

## Key Commands

Use `just` as the primary task runner:

```bash
just build          # Release build
just check          # Run all checks: sweep-check, format, clippy, tests
just test           # Run unit tests (excludes e2e)
just check-format   # Check formatting (cargo fmt --all -- --check)
just fix-format     # Auto-fix formatting
just check-clippy   # Run clippy with -D warnings
just fix-clippy     # Auto-fix clippy warnings
just build-dev      # Dev build using local reth source
just build-docker   # Build Docker image (DockerfileOp)
```

## Repository Structure

```
bin/
  node/       - Main XLayer reth node binary
  tools/      - XLayer reth tools binary
crates/
  builder/    - Node builder and customization
  chainspec/  - XLayer chain specification
  flashblocks/ - Flashblocks support (fast block streaming)
  legacy-rpc/ - Legacy RPC compatibility
  monitor/    - Monitoring and metrics
  rpc/        - XLayer RPC extensions
  tests/      - Integration and e2e tests
  version/    - Version metadata
docs/
  BUILDING_ON_RETH.md           - Core extensibility guide
  subguides/CHAINSPEC_EVM_GUIDE.md
  subguides/DATABASE_GUIDE.md
  subguides/ENGINE_CONSENSUS_GUIDE.md
  subguides/EXTENSION_GUIDE.md
  subguides/NODE_BUILDER_GUIDE.md
  subguides/PAYLOAD_BUILDER_GUIDE.md
```

## Code Guidelines

Before making changes, read the relevant guide in `docs/`:
- **Chainspec/EVM changes**: `docs/subguides/CHAINSPEC_EVM_GUIDE.md`
- **Database changes**: `docs/subguides/DATABASE_GUIDE.md`
- **Engine/consensus changes**: `docs/subguides/ENGINE_CONSENSUS_GUIDE.md`
- **Adding extensions**: `docs/subguides/EXTENSION_GUIDE.md`
- **Node builder changes**: `docs/subguides/NODE_BUILDER_GUIDE.md`
- **Payload builder changes**: `docs/subguides/PAYLOAD_BUILDER_GUIDE.md`

## Coding Standards

- All Rust code must pass `cargo clippy --all-targets --workspace -- -D warnings`
- Code must be formatted with `cargo fmt --all`
- New functionality must have unit tests
- Avoid modifying upstream Reth code directly; extend via traits
- Use idiomatic Rust: prefer `Result`/`Option` over panics in library code
- Follow the PR template checklist (`.github/pull_request_template.md`)

## Development Workflow

1. Make code changes
2. Run `just fix` to auto-fix formatting and clippy warnings
3. Run `just check` to verify all checks pass
4. Run `just test` to verify tests pass
5. Commit with a descriptive message following conventional commits format

## Available Skills

- `/review` - Perform a thorough code review of changed files against XLayer conventions
- `/commit` - Generate a conventional commit message for staged changes
