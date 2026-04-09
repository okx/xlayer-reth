# Quality Knowledge Domain

## Build & Check Commands

All quality gates are managed via `just` (justfile):

```bash
just check          # Run all checks: sweep-check, format, clippy, tests
just test           # Run unit tests (excludes e2e)
just check-format   # Check formatting (cargo fmt --all -- --check)
just fix-format     # Auto-fix formatting
just check-clippy   # Run clippy with -D warnings
just fix-clippy     # Auto-fix clippy warnings
just build          # Release build
```

## Code Quality Standards

- **Clippy**: All code must pass `cargo clippy --all-targets --workspace -- -D warnings`
- **Formatting**: Code must be formatted with `cargo fmt --all`
- **Tests**: New functionality must have unit tests
- **No Panics in Libraries**: Use `Result`/`Option` over panics in library code
- **Extend, Don't Modify**: Avoid modifying upstream Reth code directly; extend via traits

## Testing Strategy

### Unit Tests
- Located alongside source code in `#[cfg(test)]` modules
- Run via `just test` (uses `cargo nextest` with timeout configuration)
- Cover: argument parsing/validation, bridge event parsing, block parameter routing, cache operations, sequence validation, timing logic

### Integration / E2E Tests
- Located in `crates/tests/` with separate test binaries under `e2e-tests/` and `flashblocks-tests/`
- Cover: basic RPC operations, debug tracing, transaction lifecycle, flashblocks confirmation benchmarks
- Require running node infrastructure (L1 + L2)

### Builder Tests
- Located in `crates/builder/src/tests/`
- Feature-gated behind `#[cfg(any(test, feature = "testing"))]`
- Include: smoke tests, flashblock building, data availability limits, miner gas limits, transaction pool, fork behavior

## CI/CD

- CI uses `cargo nextest` for parallel test execution with timeout configuration
- Docker builds via `DockerfileOp` (node) and `DockerfileTools` (tools)
- PR template checklist enforced (`.github/pull_request_template.md`)
- Conventional commits format required

## Security Considerations

- JSON injection prevention in legacy RPC routing (validated via `is_valid_32_bytes_string`)
- Bridge intercept configuration validation (address format, required fields)
- Input validation at system boundaries (CLI args, RPC parameters)
- No hardcoded secrets; signing keys provided via CLI flags or environment
