use crate::error::KmsError;
use ok_kms_rust::KmsClient as SdkClient;

/// Thin wrapper around the `ok-kms-rust` SDK.
///
/// The SDK loads the embedded `kms_*.so` and is gated by the `KMS_ENABLED`
/// environment variable; secrets are addressed by key name within the
/// configured KMS secret bundle.
pub struct KmsClient {
    inner: SdkClient,
}

impl KmsClient {
    /// Initialize the KMS client via the `ok-kms-rust` SDK.
    ///
    /// Requires `KMS_ENABLED=true`; otherwise the SDK returns
    /// `ok_kms_rust::KmsError::Disabled` (wrapped as [`KmsError::Sdk`]) and the
    /// caller should fall back to local configuration.
    pub fn new() -> Result<Self, KmsError> {
        Ok(Self { inner: SdkClient::new()? })
    }

    /// Fetch a secret by its KMS key name.
    ///
    /// Returns `Ok(None)` when the key is absent or its value is empty (the
    /// caller should fall back to local defaults), or `Ok(Some(value))` with the
    /// trimmed plaintext value otherwise.
    pub fn get_secret(&self, name: &str) -> Result<Option<String>, KmsError> {
        let value = self.inner.get_value_by_key(name)?;
        let value = value.trim();
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value.to_string()))
        }
    }
}

/// If `s` (after trimming) is a `kms:<key-name>` reference, returns the trimmed
/// `<key-name>`; otherwise returns `None`.
///
/// Used to detect KMS references embedded in existing secret inputs (the value
/// of `--rollup.builder-secret-key`, or the contents of the `--p2p-secret-key`
/// / `--flashblocks.p2p_private_key_file` files) without introducing new flags.
pub fn parse_kms_ref(s: &str) -> Option<&str> {
    s.trim().strip_prefix("kms:").map(str::trim).filter(|name| !name.is_empty())
}
