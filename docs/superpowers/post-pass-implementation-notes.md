# Post-PASS implementation notes — carried from the r4 approval

These two notes arrived **inside the approving Bus message** and the reviewer stated explicitly
that they are to be applied directly under the existing specification and that **no further
design round is required for them** ("两条实现说明直接按现有规范落实，无需为它们另起设计轮次").

They are therefore recorded here rather than edited into the approved Spec, because the PASS
binds to design revision **r4 / canonical sha256
`769690d4c31098c3c4dd318cdbbc8ac7d94fa111ca4675becca1f76952be4179`**. Changing the Spec body now
would change that hash and invalidate the binding. These notes are **binding implementation
guidance for the implementation stage**, not design changes.

Source: Bus reply `starport-rcs-directcallconsumer-spec-r4-pass-20261028`, record
`reczz28SlhkREAjb`, sender `starport-rcs`.

## N1 — Lifetimes at the async entry point

If the async JSON-RPC / HTTP entry point cannot safely hold a borrow, use the **owned
`Box<RawValue>`** form that §6.2.1.1 already permits, and verify both the normal and the test
dependency graphs under test 42.

**Prohibited:** converting to `serde_json::Value` and back to `RawValue` for lifetime
convenience. That round-trip is exactly the fidelity loss §6.2.1.1's hard prohibition forbids —
it would re-destroy the duplicate-alias evidence.

## N2 — Error handling granularity in Phase B, and non-object rules

- The `?` operators shown in the §6.2.1a `B1`/`B3` sketch must sit **inside the per-rule `Result`
  handling**, not propagate out of the loop. The outer loop collects the error and **continues to
  the next rule**, which is what makes test 55's full aggregation real. The sketch is
  illustrative; the loop must not short-circuit.
- `RawValue` preserves **any valid JSON value** and does **not** by itself guarantee a JSON
  object. A non-object rule element must therefore be **explicitly rejected in `B1`** and reported
  by its **0-based index**, with nothing executed, installed or committed.
- Do **not** claim that such a value necessarily fails in Phase A merely because the type is
  `RawValue`. §6.2.1b's unpromised-cases paragraph must not be read as covering this; `B1` owns
  the explicit rejection.

## Acceptance

Both notes are covered by the already-approved matrix: N1 by test 42 (compile acceptance of both
consuming crates and their test targets) and test 60 (byte-fidelity guard, which must also reject
a `Value` round-trip); N2 by test 55 (full aggregation across several failing rules) plus a
non-object rule element case added to the `B1` rejection rows, reported by index at both entry
points.
