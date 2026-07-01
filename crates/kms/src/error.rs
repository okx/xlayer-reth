#[derive(Debug, thiserror::Error)]
pub enum KmsError {
    /// Error originating from the underlying `ok-kms-rust` SDK (init, FFI, disabled, etc.).
    #[error(transparent)]
    Sdk(#[from] ok_kms_rust::KmsError),
}
