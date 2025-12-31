use reth::version::{
    default_reth_version_metadata, try_init_version_metadata, RethCliVersionConsts,
};

pub const XLAYER_RETH_CLIENT_VERSION: &str = concat!("xlayer/v", env!("CARGO_PKG_VERSION"));

pub fn init_version_metadata() {
    // NOTE: these versions are the upstream repo default values.
    let default_version_metadata = default_reth_version_metadata();

    let upstream_version =
        format!("Upstream Reth Version: {}", default_version_metadata.vergen_git_sha);
    let long_version = format!(
        "{}\n{}\n{}\n{}\n{}",
        env!("RETH_LONG_VERSION_0"),
        upstream_version,
        env!("RETH_LONG_VERSION_1"),
        env!("RETH_LONG_VERSION_2"),
        env!("RETH_LONG_VERSION_3"),
    );

    try_init_version_metadata(RethCliVersionConsts {
        name_client: "XLayer Reth".to_string().into(),
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
        vergen_git_sha: env!("VERGEN_GIT_SHA_SHORT").into(),
        vergen_git_sha_long: env!("VERGEN_GIT_SHA").into(),
        short_version: env!("RETH_SHORT_VERSION").into(),
        long_version: long_version.into(),
        ..default_version_metadata
    })
    .expect("Unable to init version metadata");
}

#[cfg(test)]
mod tests {
    use crate::init_version_metadata;
    use reth_node_core::version::version_metadata;

    #[test]
    fn verify_version() {
        init_version_metadata();
        let version_output = version_metadata();
        let sha = std::env::var("VERGEN_GIT_SHA").unwrap();
        assert_eq!(sha, version_output.vergen_git_sha_long);
    }
}
