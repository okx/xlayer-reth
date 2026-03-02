use reth_db::{
    init_db,
    mdbx::{DatabaseArguments, MaxReadTransactionDuration, KILOBYTE, MEGABYTE},
    test_utils::{TempDatabase, ERROR_DB_CREATION},
    ClientVersion, DatabaseEnv,
};
use reth_node_core::{args::DatadirArgs, dirs::DataDirPath, node_config::NodeConfig};
use reth_optimism_chainspec::OpChainSpec;
use std::{net::TcpListener, sync::Arc};

pub fn create_test_db(config: NodeConfig<OpChainSpec>) -> Arc<TempDatabase<DatabaseEnv>> {
    let path = reth_node_core::dirs::MaybePlatformPath::<DataDirPath>::from(
        reth_db::test_utils::tempdir_path(),
    );
    let db_config =
        config.with_datadir_args(DatadirArgs { datadir: path.clone(), ..Default::default() });
    let data_dir = path.unwrap_or_chain_default(db_config.chain.chain(), db_config.datadir.clone());
    let path = data_dir.db();
    let db = init_db(
        path.as_path(),
        DatabaseArguments::new(ClientVersion::default())
            .with_max_read_transaction_duration(Some(MaxReadTransactionDuration::Unbounded))
            .with_geometry_max_size(Some(4 * MEGABYTE))
            .with_growth_step(Some(4 * KILOBYTE)),
    )
    .expect(ERROR_DB_CREATION);
    Arc::new(TempDatabase::new(db, path))
}

/// Gets an available port by first binding to port 0 -- instructing the OS to
/// find and assign one. Then the listener is dropped when this goes out of
/// scope, freeing the port for the next time this function is called.
pub fn get_available_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("Failed to bind to random port")
        .local_addr()
        .expect("Failed to get local address")
        .port()
}
