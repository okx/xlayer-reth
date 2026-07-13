//! KMS secret resolution for X Layer node startup.
//!
//! Secrets are sourced from KMS via the `ok-kms-rust` SDK by embedding a
//! `kms:<key-name>` reference in an existing secret input — no new CLI flags:
//!
//! - builder secret key: the value of `--rollup.builder-secret-key`
//! - node devp2p secret key: the contents of the `--p2p-secret-key` file
//! - flashblocks p2p key: the contents of the `--flashblocks.p2p_private_key_file` file
//!
//! Resolution is strict: configuring a `kms:` reference means "this secret
//! comes from KMS". If any `kms:` reference is present, KMS must be available
//! and every referenced key must resolve to a valid value — otherwise node
//! startup fails. There is no silent fallback to a generated/empty key, so a
//! KMS outage or a missing/malformed key never boots a node with the wrong
//! signing key or an unintended p2p identity.

use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use eyre::WrapErr;
use tracing::info;
use xlayer_builder::args::BuilderArgs;
use xlayer_builder::signer::Signer;
use xlayer_kms::{parse_kms_ref, KmsClient};

/// Secrets resolved from KMS that the node applies after the payload builder is
/// constructed and to the reth node config.
#[derive(Default)]
pub struct KmsResolution {
    /// Node devp2p secret key. When set, the caller clears `--p2p-secret-key`
    /// (which pointed at the `kms:` reference file) and sets
    /// `network.p2p_secret_key_hex` instead.
    pub node_p2p_secret: Option<B256>,
    /// Flashblocks p2p key hex (sets `flashblocks.p2p_private_key_override`).
    pub flashblocks_p2p_hex: Option<String>,
    /// Builder signer resolved from KMS.
    pub builder_signer: Option<Signer>,
}

/// Resolve `kms:` references found in the builder/flashblocks/node-p2p inputs.
///
/// `node_p2p_key_file` is the configured `--p2p-secret-key` path (if any). This
/// mutates `builder_args` in place: when the flashblocks p2p key file is a
/// `kms:` reference, the file path is cleared (the key is delivered in-memory
/// instead, or the node falls back to a generated key).
pub fn resolve_kms_secrets(
    builder_args: &mut BuilderArgs,
    node_p2p_key_file: Option<PathBuf>,
) -> eyre::Result<KmsResolution> {
    // Detect kms: references in each input.
    let builder_ref: Option<String> =
        builder_args.builder_signer.as_ref().and_then(|k| k.kms_ref().map(str::to_string));

    let flashblocks_ref: Option<String> = builder_args
        .flashblocks
        .p2p
        .p2p_private_key_file
        .as_deref()
        .and_then(|p| read_kms_ref_from_file(Path::new(p)));

    let node_p2p_ref: Option<String> =
        node_p2p_key_file.as_deref().and_then(read_kms_ref_from_file);

    let mut resolution = KmsResolution::default();

    if builder_ref.is_none() && flashblocks_ref.is_none() && node_p2p_ref.is_none() {
        return Ok(resolution);
    }

    // At least one `kms:` reference is configured, so KMS must be available.
    // Any init/fetch failure is fatal — never fall back to a wrong key.
    let client = KmsClient::new()
        .wrap_err("KMS client init failed, but kms: secret references are configured")?;
    info!("KMS enabled, resolving secret references");

    if let Some(name) = builder_ref {
        let value = fetch(&client, &name, "builder secret key")?;
        let secret = parse_b256(&value, "builder secret key")?;
        let signer = Signer::try_from_secret(secret)
            .map_err(|e| eyre::eyre!("builder secret key from KMS is invalid: {e}"))?;
        info!(address = %signer.address, "builder secret key loaded from KMS");
        resolution.builder_signer = Some(signer);
    }

    if let Some(name) = flashblocks_ref {
        let value = fetch(&client, &name, "flashblocks p2p key")?;
        let normalized = value.strip_prefix("0x").unwrap_or(&value).to_string();
        // The flashblocks p2p key is a libp2p ed25519 keypair, which
        // `ed25519::Keypair::try_from_bytes` requires to be exactly 64 bytes.
        // Validate length here so a present-but-malformed value is a hard error
        // at startup rather than a deferred panic during p2p node spawn.
        let bytes =
            hex::decode(&normalized).wrap_err("flashblocks p2p key from KMS is not valid hex")?;
        if bytes.len() != 64 {
            eyre::bail!(
                "flashblocks p2p key from KMS must be a 64-byte ed25519 keypair, got {} bytes",
                bytes.len()
            );
        }
        // Clear the file path so the flashblocks service uses the in-memory
        // override and never tries to read `kms:` as a literal key.
        builder_args.flashblocks.p2p.p2p_private_key_file = None;
        info!("loaded flashblocks p2p keypair from KMS");
        resolution.flashblocks_p2p_hex = Some(normalized);
    }

    if let Some(name) = node_p2p_ref {
        let value = fetch(&client, &name, "node devp2p secret key")?;
        let secret = parse_b256(&value, "node devp2p secret key")?;
        info!("loaded node devp2p secret key from KMS");
        resolution.node_p2p_secret = Some(secret);
    }

    Ok(resolution)
}

/// Fetch a secret from KMS. Errors if the SDK fails or the key is absent/empty —
/// a configured `kms:` reference must resolve to a real value.
fn fetch(client: &KmsClient, name: &str, what: &str) -> eyre::Result<String> {
    client
        .get_secret(name)
        .wrap_err_with(|| format!("{what}: KMS fetch failed for key '{name}'"))?
        .ok_or_else(|| eyre::eyre!("{what}: KMS key '{name}' is absent or empty"))
}

/// Read a file and return the embedded `kms:` key name if its content is a
/// `kms:<name>` reference. Returns `None` if the file can't be read or isn't a
/// reference (treated as a normal secret file).
fn read_kms_ref_from_file(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_kms_ref(&content).map(str::to_string)
}

/// Parse a `0x`-optional hex string from KMS into a 32-byte [`B256`].
fn parse_b256(value: &str, what: &str) -> eyre::Result<B256> {
    let trimmed = value.trim();
    let trimmed = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes =
        hex::decode(trimmed).wrap_err_with(|| format!("{what} from KMS is not valid hex"))?;
    if bytes.len() != 32 {
        eyre::bail!("{what} from KMS must be 32 bytes, got {}", bytes.len());
    }
    Ok(B256::from_slice(&bytes))
}
