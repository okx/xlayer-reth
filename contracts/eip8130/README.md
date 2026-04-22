# EIP-8130 Contracts (XLayer)

Vendored EIP-8130 reference contracts used as XLayerAA system predeploys.
Source tracks [base/eip-8130](https://github.com/base/eip-8130); we
mirror the Solidity semantics but **not** their CREATE2 / Nick's-factory
deployment scheme — see *Installation model* below.

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
├── script/Deploy.s.sol             # reference-only: Base-style CREATE2 preview. NOT the XLayer install path.
└── artifacts/*.bin-runtime         # committed runtime bytecode — reference for `cast code` smoke tests
```

## Installation model

XLayerAA predeploys are **not** installed via an EL hook or CREATE2
factory call. They're installed by `op-node`-synthesized upgrade
deposit transactions at the `XLayerHardfork::XLayerAA` activation
boundary block — the same recipe every OP fork since Ecotone uses
(see `deps/optimism/op-node/rollup/derive/jovian_upgrade_transactions.go`
for the nearest reference implementation).

Each predeploy is assigned a fresh `0x4210...001X` deployer address,
and its canonical address is `CREATE(deployer, nonce=0)`. `op-node`
emits 7 `DepositTx { from: deployer_i, to: nil, data: creationCode_i }`
in the activation block; the EL executes them on the normal tx path.

The deployer → predeploy address map lives in
[`crates/chainspec/src/xlayer_aa_predeploys.rs`](../../crates/chainspec/src/xlayer_aa_predeploys.rs)
and is mirrored in the op-node Go patch.

## Building

Foundry is required — match `.github/workflows` or your local version
to what the pinned `foundry.toml` targets (Solidity ≥ 0.8.30,
`evm_version = "osaka"`).

```bash
forge build --root contracts/eip8130
```

## Regenerating artifacts

Any change to a vendored `.sol` file, compiler setting, or pinned
library commit must be followed by:

```bash
just contracts-eip8130-build
```

This runs `forge build` and extracts bytecode into `artifacts/`. Both
`op-node` (which embeds creation code hex into its upgrade-tx payload)
and the devnet smoke-test harness (which compares runtime bytecode
against `cast code`) consume these artifacts.

## Predeploy address table

The canonical 7 predeploy addresses are `CREATE(deployer, nonce=0)` —
deterministic functions of the deployer constant alone (independent
of the deployed bytecode). See
[`crates/chainspec/src/xlayer_aa_predeploys.rs`](../../crates/chainspec/src/xlayer_aa_predeploys.rs)
for the authoritative deployer → predeploy map, and its
`create_address_matches_deployer` test which recomputes every entry
at build time.

`script/Deploy.s.sol` (inherited from the upstream Base reference
repo) uses a Nick's-factory + CREATE2 scheme — it is **not** the
XLayer install path and its addresses will differ from those
actually installed on XLayer chains. It is kept only for local
testing convenience against an arbitrary anvil-style devnet.

## License

MIT (matches the upstream reference implementation).
