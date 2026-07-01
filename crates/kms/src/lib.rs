pub mod client;
pub mod error;

pub use client::{parse_kms_ref, KmsClient};
pub use error::KmsError;
