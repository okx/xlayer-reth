# Skill: review

Perform a thorough code review of changed files, checking for correctness, Rust best practices, XLayer-specific conventions, security issues, and test coverage.

TRIGGER when: user asks to "review" code, requests a code review, or uses `/review`.
DO NOT TRIGGER when: user only asks a question about code without requesting a review.

## Instructions

When invoked, perform the following code review steps in order:

### 1. Identify Changed Files

Read the diff or changed files. Focus on:
- New or modified Rust source files (`*.rs`)
- Configuration changes (`Cargo.toml`, `justfile`)
- Documentation updates

### 2. Check Rust Code Quality

For each changed Rust file:
- **Correctness**: Look for logic errors, off-by-one errors, incorrect type conversions, or incorrect use of async/await
- **Error handling**: Ensure errors are propagated with `?` or handled explicitly; avoid `unwrap()`/`expect()` in library code (only acceptable in tests or with a clear invariant comment)
- **Clippy compliance**: Flag patterns that would trigger `cargo clippy -- -D warnings`
- **Formatting**: Check that code follows `rustfmt` style (consistent indentation, trailing commas in multi-line expressions)
- **Idiomatic Rust**: Prefer `Option`/`Result` combinators, iterators over manual loops, and `From`/`Into` for type conversions

### 3. Check XLayer-Specific Conventions

- **Upstream compatibility**: Changes must not break upstream Reth compatibility; extensions should use trait implementations, not modifications to upstream types
- **Component isolation**: Changes in `crates/` should be isolated to their crate; cross-crate dependencies should be minimal and intentional
- **Database changes** (if any): Must follow `docs/subguides/DATABASE_GUIDE.md` — new tables need proper `Compress`/`Decompress` implementations
- **RPC extensions** (if any): Must follow `docs/subguides/EXTENSION_GUIDE.md` — use the `EthApiExt` pattern
- **Chainspec changes** (if any): Must follow `docs/subguides/CHAINSPEC_EVM_GUIDE.md`
- **Node builder changes** (if any): Must follow `docs/subguides/NODE_BUILDER_GUIDE.md`

### 4. Check Security

- No hardcoded secrets, API keys, or private keys
- Inputs from external sources (RPC, network) must be validated before use
- Integer arithmetic that could overflow should use checked operations (`checked_add`, `saturating_add`) or explicitly handle overflow
- No unsafe code without a `// SAFETY:` comment explaining the invariant
- Dependencies added to `Cargo.toml` should be from trusted sources

### 5. Check Tests

- New public functions or changed behavior must have corresponding unit tests
- Tests should cover both happy path and error/edge cases
- E2e tests (in `crates/tests/`) should be added for significant new features
- Test names should be descriptive (`test_<what>_<condition>_<expected>`)

### 6. Check PR Checklist Compliance

Verify against `.github/pull_request_template.md`:
- [ ] Code follows coding standards (clippy, fmt)
- [ ] Self-review performed
- [ ] Hard-to-understand areas are commented
- [ ] Tests prove the fix/feature works
- [ ] Existing tests still pass

### 7. Provide Review Summary

Structure feedback as:

**Summary**: Brief overview of the change and its purpose.

**Issues** (if any):
- 🔴 **Critical**: Must fix before merging (bugs, security issues, broken tests)
- 🟡 **Warning**: Should fix (style violations, missing tests, anti-patterns)
- 🔵 **Suggestion**: Consider improving (performance, readability, idiomatic Rust)

**Positives**: Highlight good patterns or improvements worth noting.

**Verdict**: `LGTM` / `Request Changes` / `Needs Discussion`
