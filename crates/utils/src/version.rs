use reth::version::{try_init_version_metadata, RethCliVersionConsts};

pub const XLAYER_RETH_CLIENT_VERSION: &str = concat!("xlayer/v", env!("CARGO_PKG_VERSION"));

pub fn init_version_metadata(
    default_version_metadata: RethCliVersionConsts,
) -> Result<(), RethCliVersionConsts> {
    try_init_version_metadata(RethCliVersionConsts {
        name_client: "XLayer Reth Export".to_string().into(),
        cargo_pkg_version: format!(
            "{}/{}",
            default_version_metadata.cargo_pkg_version,
            env!("CARGO_PKG_VERSION")
        )
        .into(),
        p2p_client_version: format!(
            "{}/{}",
            default_version_metadata.p2p_client_version, XLAYER_RETH_CLIENT_VERSION
        )
        .into(),
        extra_data: format!(
            "{}/{}",
            default_version_metadata.extra_data, XLAYER_RETH_CLIENT_VERSION
        )
        .into(),
        ..default_version_metadata
    })?;
    Ok(())
}
