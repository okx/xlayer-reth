#!/usr/bin/env python3
"""
Extract XLayerAA predeploy bytecode from Foundry artifacts.

For each of the 7 vendored contracts, this writes three files into
`artifacts/`:

  - `<Name>.bin-runtime` (hex)   — runtime bytecode with every
      `AccountConfiguration`-typed immutable **already patched** to
      the canonical AC predeploy address. Consumed by:
        * `reth --dev` genesis alloc (a predeploy needs to appear
          fully-formed at block 0 because `--dev` has no op-node
          to synthesize upgrade transactions).
        * `cast code <addr>` smoke tests that compare post-install
          runtime against the expected hex.

  - `<Name>.bin` (raw binary)    — byte-identical to `.bin-runtime`
      minus the hex encoding. Consumed by Rust via `include_bytes!`
      when the `AA_PREDEPLOYS` table needs to embed runtime code
      for genesis alloc.

  - `<Name>.bin-creation` (hex)  — **creation code** (constructor
      prelude + runtime + metadata), with immutables left as zero
      placeholders. Consumed by op-node's upgrade-deposit-tx
      payload: op-node constructs
          data = <this hex> ++ abi.encode(AccountConfiguration.address)
      so the on-chain-executing constructor fills the immutable
      slots during `CREATE(deployer, 0)`. Paste the hex verbatim
      into the Go constants in
      `deps/optimism/op-node/rollup/derive/xlayer_aa_upgrade_transactions.go`.

Why three files? The two install paths need different payloads:

    --dev (genesis alloc)   → runtime bytecode, immutables patched
                              (constructor is never executed at block 0)
    op-node (upgrade tx)    → creation code, immutables unpatched
                              (constructor runs on-chain and sets them)

The canonical `AccountConfiguration` address must stay in sync with
[`crates/chainspec/src/xlayer_aa_predeploys.rs`](../../crates/chainspec/src/xlayer_aa_predeploys.rs)
(see the `ACCOUNT_CONFIGURATION_ADDRESS` constant there).

Run:
  python3 contracts/eip8130/script/extract_runtime.py
"""
from __future__ import annotations

import json
import pathlib
import sys

# ── Canonical CREATE(deployer=0x4210...0010, nonce=0) address ───────
# Keep in sync with `ACCOUNT_CONFIGURATION_ADDRESS` in
# `crates/chainspec/src/xlayer_aa_predeploys.rs`. A mismatch makes
# `.bin-runtime` files embed the wrong immutable — `--dev` genesis
# alloc ends up with accounts calling into a non-existent
# AccountConfiguration address.
ACCOUNT_CONFIGURATION_ADDR = "0xA6A551b856B139B3292128F3b36ADa58025c4b27"

# ── Contract set ────────────────────────────────────────────────────
# (solidity source path relative to src/, contract name). Order
# matches AA_PREDEPLOYS in the chainspec — keep them aligned so both
# sides index-match.
CONTRACTS = [
    ("AccountConfiguration.sol",            "AccountConfiguration"),
    ("accounts/DefaultAccount.sol",         "DefaultAccount"),
    ("accounts/DefaultHighRateAccount.sol", "DefaultHighRateAccount"),
    ("verifiers/K1Verifier.sol",            "K1Verifier"),
    ("verifiers/P256Verifier.sol",          "P256Verifier"),
    ("verifiers/WebAuthnVerifier.sol",      "WebAuthnVerifier"),
    ("verifiers/DelegateVerifier.sol",      "DelegateVerifier"),
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
    """Patch every immutable placeholder with the canonical
    `AccountConfiguration` predeploy address, in place.

    All three contracts that carry immutables (`DefaultAccount`,
    `DefaultHighRateAccount`, `DelegateVerifier`) reference only
    `ACCOUNT_CONFIGURATION`, so a single address covers every slot.
    """
    slot = address_to_slot_bytes(ACCOUNT_CONFIGURATION_ADDR)
    for refs in immutable_refs.values():
        for ref in refs:
            start, length = ref["start"], ref["length"]
            if length != 32:
                raise ValueError(
                    f"unexpected immutable length {length}, expected 32"
                )
            # Sanity: placeholder slot must be zero. Non-zero means
            # either the bytecode was already patched (double-run)
            # or the ABI layout shifted, which we want to fail on
            # loudly rather than silently corrupt the runtime.
            if runtime[start:start + length] != b"\x00" * 32:
                raise ValueError(
                    f"immutable slot at offset {start} was not zero; "
                    "bytecode may already be patched or layout shifted"
                )
            runtime[start:start + length] = slot


def extract(solidity_file: str, contract_name: str) -> None:
    # Forge flattens `out/` by source-file basename, so e.g.
    # `src/accounts/DefaultAccount.sol` maps to
    # `out/DefaultAccount.sol/DefaultAccount.json`.
    basename = solidity_file.rsplit("/", 1)[-1]
    artifact_path = OUT_DIR / basename / f"{contract_name}.json"
    if not artifact_path.exists():
        raise FileNotFoundError(
            f"{artifact_path} missing — run `forge build` first"
        )
    artifact = json.loads(artifact_path.read_text())

    # Runtime bytecode (post-constructor, immutables zero-placeholder).
    runtime_hex = artifact["deployedBytecode"]["object"]
    runtime = bytearray.fromhex(runtime_hex.removeprefix("0x"))
    immutable_refs = artifact["deployedBytecode"].get("immutableReferences", {})
    if immutable_refs:
        patch_immutables(runtime, immutable_refs)

    # Creation bytecode (constructor prelude + runtime). op-node
    # appends constructor args and lets the on-chain constructor set
    # immutables, so we leave this hex exactly as forge emitted it.
    creation_hex = artifact["bytecode"]["object"].removeprefix("0x")

    runtime_hex_path = ARTIFACTS_DIR / f"{contract_name}.bin-runtime"
    runtime_bin_path = ARTIFACTS_DIR / f"{contract_name}.bin"
    creation_hex_path = ARTIFACTS_DIR / f"{contract_name}.bin-creation"

    runtime_hex_path.write_text(runtime.hex())
    runtime_bin_path.write_bytes(bytes(runtime))
    creation_hex_path.write_text(creation_hex)

    creation_bytes = len(creation_hex) // 2
    print(
        f"  {contract_name:<24s}  "
        f"runtime={len(runtime):>5d}B  creation={creation_bytes:>5d}B"
    )


def main() -> int:
    if not OUT_DIR.exists():
        print(f"error: {OUT_DIR} missing — run `forge build` first", file=sys.stderr)
        return 1

    ARTIFACTS_DIR.mkdir(exist_ok=True)
    print(f"Extracting XLayerAA predeploy bytecode to {ARTIFACTS_DIR}:")
    print(f"  AccountConfiguration address = {ACCOUNT_CONFIGURATION_ADDR}")
    for solidity_file, contract_name in CONTRACTS:
        extract(solidity_file, contract_name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
