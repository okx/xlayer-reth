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

# ── Contract manifest ───────────────────────────────────────────────
# One entry per EIP-8130 predeploy. Fields:
#   sol           — source file path relative to `src/`
#   name          — contract name (matches Solidity class + out/ folder)
#   deployer      — op-node deployer address; fresh, nonce==0 at
#                   activation boundary
#   address       — CREATE(deployer, 0) destination. Duplicates the
#                   Rust-side table in xlayer_aa_predeploys.rs.
#   has_ac_arg    — constructor takes (address accountConfiguration);
#                   the NUT bundle appends abi.encode(AC_ADDR) to the
#                   raw creation code so the constructor fills the
#                   ACCOUNT_CONFIGURATION immutable on-chain.
#   gas_limit     — gas budget for the deposit tx; chosen from runtime
#                   size + constructor cost + 2-3x safety margin. All
#                   7 are well below Jovian's 1.75M GPO limit.
#   intent        — UpgradeDepositSource.Intent — hashed into the
#                   deposit source hash so each tx has a unique
#                   deterministic id the EL can dedup.
#
# Order matters: AccountConfiguration MUST be first because the 3
# "has_ac_arg" contracts reference its post-deploy address.
CONTRACTS = [
    {
        "sol":        "AccountConfiguration.sol",
        "name":       "AccountConfiguration",
        "deployer":   "0x4210000000000000000000000000000000000010",
        "address":    "0xA6A551b856B139B3292128F3b36ADa58025c4b27",
        "has_ac_arg": False,
        "gas_limit":  2_500_000,
        "intent":     "XLayerAA: AccountConfiguration Deployment",
    },
    {
        "sol":        "accounts/DefaultAccount.sol",
        "name":       "DefaultAccount",
        "deployer":   "0x4210000000000000000000000000000000000011",
        "address":    "0x5D82f4311f134052bb36b11BD665Ddab843ebb3D",
        "has_ac_arg": True,
        "gas_limit":  1_000_000,
        "intent":     "XLayerAA: DefaultAccount Deployment",
    },
    {
        "sol":        "accounts/DefaultHighRateAccount.sol",
        "name":       "DefaultHighRateAccount",
        "deployer":   "0x4210000000000000000000000000000000000012",
        "address":    "0x86bf4F2d426b3386a04a24fE21a0CEb34A7b806c",
        "has_ac_arg": True,
        "gas_limit":  1_000_000,
        "intent":     "XLayerAA: DefaultHighRateAccount Deployment",
    },
    {
        "sol":        "verifiers/K1Verifier.sol",
        "name":       "K1Verifier",
        "deployer":   "0x4210000000000000000000000000000000000013",
        "address":    "0x7F2c04d16c53f2be99aD1a86771637568B718dBf",
        "has_ac_arg": False,
        "gas_limit":  750_000,
        "intent":     "XLayerAA: K1Verifier Deployment",
    },
    {
        "sol":        "verifiers/P256Verifier.sol",
        "name":       "P256Verifier",
        "deployer":   "0x4210000000000000000000000000000000000014",
        "address":    "0xAfc812351BE998FB088851a79Fc68887C42D7719",
        "has_ac_arg": False,
        "gas_limit":  1_500_000,
        "intent":     "XLayerAA: P256Verifier Deployment",
    },
    {
        "sol":        "verifiers/WebAuthnVerifier.sol",
        "name":       "WebAuthnVerifier",
        "deployer":   "0x4210000000000000000000000000000000000015",
        "address":    "0x4921DCFD2541f738990767852aB925B3b9f652A2",
        "has_ac_arg": False,
        "gas_limit":  2_000_000,
        "intent":     "XLayerAA: WebAuthnVerifier Deployment",
    },
    {
        "sol":        "verifiers/DelegateVerifier.sol",
        "name":       "DelegateVerifier",
        "deployer":   "0x4210000000000000000000000000000000000016",
        "address":    "0xE89A62553fE775AFe77464969b2296dc1745CF85",
        "has_ac_arg": True,
        "gas_limit":  750_000,
        "intent":     "XLayerAA: DelegateVerifier Deployment",
    },
]

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "out"
ARTIFACTS_DIR = ROOT / "artifacts"
# NUT bundle is written directly into the op-node submodule alongside
# `upgrade_transaction.go`'s `karst_nut_bundle.json` — that file is
# what `//go:embed` in the generated Go module loads. Keeping exactly
# one copy avoids sync hazard with a mirrored `artifacts/` version.
NUT_BUNDLE_PATH = (
    ROOT.parent.parent
    / "deps/optimism/op-node/rollup/derive/xlayer_aa_nut_bundle.json"
)


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


def extract(contract: dict) -> str:
    """Extract runtime + creation bytecode for one contract from the
    forge artifact. Writes `.bin-runtime`, `.bin`, `.bin-creation`
    into `ARTIFACTS_DIR`. Returns the creation hex (no `0x` prefix)
    so the caller can stuff it into the NUT bundle without re-reading
    the file.
    """
    solidity_file = contract["sol"]
    contract_name = contract["name"]

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
    return creation_hex


def build_nut_bundle(creation_codes: dict[str, str]) -> None:
    """Write the op-node NUT (Network Upgrade Transactions) bundle.

    One JSON entry per contract. `data` contains the fully-prepared
    deposit-tx payload:

      - For contracts without constructor args: raw creation code.
      - For contracts with (address accountConfiguration) constructor:
        creation code ++ abi.encode(AC_ADDR). The on-chain constructor
        reads the arg from calldata and writes it to the immutable.

    `upgrade_transaction.go` in the op-node submodule reads this JSON
    via `//go:embed` at compile time, so any change to the bundle
    requires rebuilding op-node.
    """
    ac_arg_hex = "0" * 24 + ACCOUNT_CONFIGURATION_ADDR.removeprefix("0x").lower()
    # Sanity: 24 zero-nibbles + 40 address-nibbles = 64 nibbles = 32 bytes
    assert len(ac_arg_hex) == 64, "abi-encoded address must be 32 bytes"

    transactions = []
    for contract in CONTRACTS:
        creation_hex = creation_codes[contract["name"]]
        data_hex = creation_hex
        if contract["has_ac_arg"]:
            data_hex = data_hex + ac_arg_hex
        transactions.append({
            "intent":   contract["intent"],
            "from":     contract["deployer"],
            "to":       None,
            "data":     "0x" + data_hex,
            "gasLimit": contract["gas_limit"],
        })

    bundle = {
        "metadata": {
            "version": "1.0.0",
            # `fork` is informational; op-node's readNUTBundle takes
            # the fork name as a function arg, not from the JSON.
            "fork":    "xlayer_aa",
        },
        "transactions": transactions,
    }

    NUT_BUNDLE_PATH.parent.mkdir(parents=True, exist_ok=True)
    NUT_BUNDLE_PATH.write_text(json.dumps(bundle, indent=2) + "\n")

    total_gas = sum(tx["gasLimit"] for tx in transactions)
    print(
        f"NUT bundle: {NUT_BUNDLE_PATH.relative_to(ROOT.parent.parent)}  "
        f"({len(transactions)} txs, {total_gas:,} gas total)"
    )


def main() -> int:
    if not OUT_DIR.exists():
        print(f"error: {OUT_DIR} missing — run `forge build` first", file=sys.stderr)
        return 1

    ARTIFACTS_DIR.mkdir(exist_ok=True)
    print(f"Extracting XLayerAA predeploy bytecode to {ARTIFACTS_DIR}:")
    print(f"  AccountConfiguration address = {ACCOUNT_CONFIGURATION_ADDR}")
    creation_codes = {}
    for contract in CONTRACTS:
        creation_codes[contract["name"]] = extract(contract)
    build_nut_bundle(creation_codes)
    return 0


if __name__ == "__main__":
    sys.exit(main())
