#!/usr/bin/env python3
"""
Extract runtime bytecode from Foundry artifacts for the XLayerAA
predeploy set.

For each of the 7 vendored contracts:
  1. Reads `out/<Contract>.sol/<Contract>.json`.
  2. Takes `deployedBytecode.object` (runtime bytes with immutables as
     zero placeholders).
  3. Patches every immutable placeholder with the canonical
     `AccountConfiguration` CREATE2 address (left-padded to 32 bytes)
     so the resulting runtime matches what Nick's CREATE2 factory
     would plant at genesis.
  4. Writes the final hex string (no `0x` prefix, no trailing newline)
     to `artifacts/<Contract>.bin-runtime`.

Canonical addresses are derived by Foundry:
  forge script script/Deploy.s.sol:Deploy --sig 'addresses()'

If the Solidity source, lib/* submodule commits, or `foundry.toml`
compiler settings change, addresses shift in lock-step — the Rust-side
predeploy table asserts that the bytecode hash matches at compile time,
so any drift is caught there.

Run:
  python3 script/extract_runtime.py
"""
from __future__ import annotations

import json
import pathlib
import sys

# ── Canonical CREATE2(salt=0) addresses ──────────────────────────────
# Derived via `forge script script/Deploy.s.sol:Deploy --sig 'addresses()'`.
# Keep in sync with `crates/chainspec/src/xlayer_aa_predeploys.rs`.
ACCOUNT_CONFIGURATION_ADDR = "0x3621Acf9Fb8700777b69b97a648fC11944998FEe"

# ── Contract set ────────────────────────────────────────────────────
# (solidity source dir, contract name)
CONTRACTS = [
    ("AccountConfiguration.sol",        "AccountConfiguration"),
    ("accounts/DefaultAccount.sol",     "DefaultAccount"),
    ("accounts/DefaultHighRateAccount.sol", "DefaultHighRateAccount"),
    ("verifiers/K1Verifier.sol",        "K1Verifier"),
    ("verifiers/P256Verifier.sol",      "P256Verifier"),
    ("verifiers/WebAuthnVerifier.sol",  "WebAuthnVerifier"),
    ("verifiers/DelegateVerifier.sol",  "DelegateVerifier"),
]

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "out"
ARTIFACTS_DIR = ROOT / "artifacts"


def address_to_slot_bytes(addr_hex: str) -> bytes:
    """Convert 0x-prefixed address to 32-byte left-padded slot bytes."""
    raw = bytes.fromhex(addr_hex.removeprefix("0x"))
    if len(raw) != 20:
        raise ValueError(f"expected 20-byte address, got {len(raw)}: {addr_hex}")
    return b"\x00" * 12 + raw


def patch_immutables(runtime: bytearray, immutable_refs: dict) -> None:
    """Patch every immutable placeholder in-place with AccountConfiguration's
    address. All three contracts that have immutables (`DefaultAccount`,
    `DefaultHighRateAccount`, `DelegateVerifier`) reference only
    `ACCOUNT_CONFIGURATION`, so a single address suffices."""
    slot = address_to_slot_bytes(ACCOUNT_CONFIGURATION_ADDR)
    for refs in immutable_refs.values():
        for ref in refs:
            start, length = ref["start"], ref["length"]
            if length != 32:
                raise ValueError(
                    f"unexpected immutable length {length}, expected 32"
                )
            # Sanity: the placeholder slot should be zero.
            if runtime[start:start + length] != b"\x00" * 32:
                raise ValueError(
                    f"immutable slot at offset {start} was not zero; "
                    "bytecode may already be patched or layout shifted"
                )
            runtime[start:start + length] = slot


def extract(solidity_file: str, contract_name: str) -> None:
    # Forge flattens `out/` by source file basename, so e.g.
    # `src/accounts/DefaultAccount.sol` maps to
    # `out/DefaultAccount.sol/DefaultAccount.json`.
    basename = solidity_file.rsplit("/", 1)[-1]
    artifact_path = OUT_DIR / basename / f"{contract_name}.json"
    if not artifact_path.exists():
        raise FileNotFoundError(
            f"{artifact_path} missing — run `forge build` first"
        )
    artifact = json.loads(artifact_path.read_text())

    runtime_hex = artifact["deployedBytecode"]["object"]
    runtime = bytearray.fromhex(runtime_hex.removeprefix("0x"))

    immutable_refs = artifact["deployedBytecode"].get("immutableReferences", {})
    if immutable_refs:
        patch_immutables(runtime, immutable_refs)

    # Dual output:
    #  - `<Contract>.bin-runtime` — hex ASCII, foundry convention.
    #    Readable, diffs cleanly in git.
    #  - `<Contract>.bin` — raw binary, consumed by Rust-side crates
    #    via `include_bytes!` (both `crates/chainspec/` and the op-reth
    #    submodule patch read the binary form — no compile-time hex
    #    decode needed).
    hex_path = ARTIFACTS_DIR / f"{contract_name}.bin-runtime"
    hex_path.write_text(runtime.hex())
    bin_path = ARTIFACTS_DIR / f"{contract_name}.bin"
    bin_path.write_bytes(bytes(runtime))
    print(f"  wrote {hex_path.relative_to(ROOT)} + .bin ({len(runtime)} bytes)")


def main() -> int:
    if not OUT_DIR.exists():
        print(f"error: {OUT_DIR} missing — run `forge build` first", file=sys.stderr)
        return 1

    ARTIFACTS_DIR.mkdir(exist_ok=True)
    print(f"Extracting runtime bytecode to {ARTIFACTS_DIR}:")
    for solidity_file, contract_name in CONTRACTS:
        extract(solidity_file, contract_name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
