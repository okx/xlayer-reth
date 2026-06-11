---
name: "harness-gaps"
description: "Process/pipeline pitfalls: design-stage assumptions that could not be compile-verified and turned out wrong"
---
# Pitfalls: Harness / Process Gaps

## 1. Uncompiled Integration-Point Citations Are Not Ground Truth

**Symptom**: A Technical Design's "Integration Point Matrix" (or a `wiring-plan.md`) cites exact
`file:line` injection points and prescribes concrete type changes (e.g. "replace `OpEvmConfig` with
a wrapper EVM type across payload.rs / service builders / engine validator"). At implementation
time the snippets do not compile against the real upstream types, and the prescribed approach is
discovered to be structurally impossible — forcing a redesign late in the pipeline.

**Root Cause**: In this codebase the design and code-implementation stages run in memory-limited
containers where `deps/optimism` is unexpanded and the full op-reth tree (~1197 crates) cannot be
compiled without OOM. Signatures and injection points are therefore read from **call sites and
docs**, never compile-verified. A citation that is line-accurate can still be type-incorrect (see
[[upstream-component-type-pinning]]).

**Mechanism**: The TD §4.0 matrix for XLOP-1100 explicitly carried an "上游可读性声明 / R-1" caveat
that wrapper trait signatures were unverified; the wiring-plan's component-wrapper snippets were
authored but never compiled. The structural blockers only appeared once a build-capable review
stage compiled the node.

**Rule**: Treat any integration point that requires changing or wrapping an **upstream concrete
type** as *unverified* until compiled on a build-capable host. When the design stage cannot compile:
1. Prefer extending via a **proven, already-shipped precedent** (e.g. the bridge-intercept
   field-threading pattern) over a novel type-cascade.
2. Flag type-cascade injection points as `R-1`-style risks and do **not** let downstream stages
   treat them as settled.
3. Scope verification commands to a single changed crate (`cargo check -p <crate>`); never
   `--workspace` clippy/test (OOM).

**Source**: review-finding F-02 (A-09 Test & Fix Report §2; A-05 Code Implementation verification
table, XLOP-1100). **Date**: 2026-06-11. **Hit count**: 1.
