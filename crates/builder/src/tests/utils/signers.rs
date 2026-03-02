use super::{BUILDER_PRIVATE_KEY, FLASHBLOCKS_DEPLOY_KEY, FUNDED_PRIVATE_KEY};
use crate::tx::signer::Signer;

pub fn builder_signer() -> Signer {
    Signer::try_from_secret(
        BUILDER_PRIVATE_KEY.parse().expect("invalid hardcoded builder private key"),
    )
    .expect("Failed to create signer from hardcoded builder private key")
}

pub fn funded_signer() -> Signer {
    Signer::try_from_secret(
        FUNDED_PRIVATE_KEY.parse().expect("invalid hardcoded funded private key"),
    )
    .expect("Failed to create signer from hardcoded funded private key")
}

pub fn flashblocks_number_signer() -> Signer {
    Signer::try_from_secret(
        FLASHBLOCKS_DEPLOY_KEY
            .parse()
            .expect("invalid hardcoded flashblocks number deployer private key"),
    )
    .expect("Failed to create signer from hardcoded flashblocks number deployer private key")
}
