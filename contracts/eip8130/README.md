# EIP-8130 Contracts (XLayer)

Vendored EIP-8130 reference contracts used as XLayerAA system predeploys.
Source tracks [base/eip-8130](https://github.com/base/eip-8130); we
intentionally mirror bytecode + `foundry.toml` compiler settings so
that CREATE2(salt=0, creationCode) addresses match whatever Base
eventually deploys upstream.

## Scope

Only the minimum set required by the XLayerAA handler fast paths is
vendored:

| Contract                      | Purpose                                                              |
| ----------------------------- | -------------------------------------------------------------------- |
| `AccountConfiguration`        | Owner authorization, account creation, change sequencing, lock state |
| `accounts/DefaultAccount`     | Default EOA-delegated wallet                                          |
| `accounts/DefaultHighRateAccount` | Wallet variant with ETH-transfer lock for higher mempool rate limit |
| `verifiers/K1Verifier`        | secp256k1 (ECDSA) — fallback for non-native chains                   |
| `verifiers/P256Verifier`      | secp256r1 raw                                                         |
| `verifiers/WebAuthnVerifier`  | secp256r1 / WebAuthn                                                  |
| `verifiers/DelegateVerifier`  | 1-hop delegate verification                                           |

Omitted from upstream (not yet needed):
`BLSVerifier`, `SchnorrVerifier`, `MultisigVerifier`, `Groth16Verifier`,
`AlwaysValidVerifier`, `UpgradeableAccount`, `UpgradeableProxy`, and
the `payer-verifiers/*` set. Re-vendor them when the corresponding
native / pool-validation support lands.

## Layout

```
contracts/eip8130/
├── foundry.toml           # via_ir + optimizer + evm_version = "osaka"
├── lib/                   # git submodules pinned to the spec-repo commits
│   ├── forge-std/
│   ├── openzeppelin-contracts/
│   └── solady/
├── src/
│   ├── AccountConfiguration.sol
│   ├── accounts/                   # DefaultAccount, DefaultHighRateAccount
│   ├── interfaces/                 # IAccountConfiguration, INonceManager, ITxContext, IVerifier
│   └── verifiers/                  # K1, P256, WebAuthn, Delegate
├── script/Deploy.s.sol             # CREATE2(salt=0) — previews and deploys the canonical addresses
└── artifacts/*.bin-runtime         # committed runtime bytecode — consumed by crates/chainspec/src/xlayer_aa_predeploys.rs
```

## Building

Foundry is required — match `.github/workflows` or your local version
to what the pinned `foundry.toml` targets (Solidity ≥ 0.8.30,
`evm_version = "osaka"`).

```bash
forge build --root contracts/eip8130
```

## Regenerating `artifacts/*.bin-runtime`

The `.bin-runtime` files under `artifacts/` are committed alongside the
source because they're consumed at chain genesis as hardfork predeploy
payloads. Any change to a vendored `.sol` file, compiler setting, or
pinned library commit must be followed by:

```bash
# from repo root
just contracts-eip8130-build    # re-runs `forge build` and copies
                                # the runtime bytecode into artifacts/
```

(to add). Until that `just` target lands, run the equivalent manually:

```bash
forge build --root contracts/eip8130 --extra-output-files bin-runtime
# copy target/out/*/runtime.bin to contracts/eip8130/artifacts/
```

## Predeploy address table

Addresses are CREATE2(salt=0, creationCode) — deterministic functions
of the runtime bytecode — and are mirrored byte-identical in
`crates/chainspec/src/xlayer_aa_predeploys.rs`. To preview them
without deploying:

```bash
forge script script/Deploy.s.sol:Deploy --sig 'addresses()' --root contracts/eip8130
```

If the bytecode shifts, the addresses shift. The Rust-side predeploy
table has a compile-time assertion that the address constants match
the byte hash of the `bin-runtime` — a mismatch fails the build
rather than silently ships a broken genesis.

## License

MIT (matches the upstream reference implementation).
