//! The RCS series' builder loop-level tests, run as this crate's integration-test binary
//! `rcs_series`. The module tree mirrors `src/` (`flashblocks::context::…`,
//! `flashblocks::rcs_filter_config_tests::…`), so every case keeps the exact name it had as a unit
//! test. Builder internals are reached only through the non-default `test-utils` seam
//! `xlayer_builder::flashblocks::test_seams`.

mod flashblocks;

// `crate::args::BuilderArgs`, exactly as the moved cases spell it.
use xlayer_builder::args;

/// The one harness fixture the moved cases use, copied verbatim from the crate's `#[cfg(test)]`
/// harness (`src/tests/utils/signers.rs`, key from `src/tests/utils/mod.rs`) so the harness itself
/// stays private.
mod tests {
    use xlayer_builder::flashblocks::test_seams::Signer;

    // anvil default key[0]
    pub const FUNDED_PRIVATE_KEY: &str =
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    pub fn funded_signer() -> Signer {
        Signer::try_from_secret(
            FUNDED_PRIVATE_KEY.parse().expect("invalid hardcoded funded private key"),
        )
        .expect("Failed to create signer from hardcoded funded private key")
    }
}
