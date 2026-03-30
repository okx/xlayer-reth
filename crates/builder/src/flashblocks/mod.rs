use crate::{args::BuilderArgs, signer::Signer};
use alloy_primitives::Address;
use core::{
    convert::TryFrom,
    fmt::Debug,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};

mod best_txs;
mod builder;
pub(crate) mod builder_tx;
mod context;
mod generator;
mod handler;
mod handler_ctx;
mod service;
mod timing;
pub(crate) mod utils;

pub use context::FlashblocksBuilderCtx;
pub use service::{FlashblocksServiceBuilder, PayloadEventsSender};
pub use utils::cache::FlashblockPayloadsCache;

/// Configuration values that are specific to the flashblocks builder.
#[derive(Debug, Clone)]
pub struct FlashblocksConfig {
    /// The address of the websockets endpoint that listens for subscriptions to
    /// new flashblocks updates.
    pub ws_addr: SocketAddr,

    /// How often a flashblock is produced. This is independent of the block time of the chain.
    /// Each block will contain one or more flashblocks. On average, the number of flashblocks
    /// per block is equal to the block time divided by the flashblock interval.
    pub interval: Duration,

    /// Should we disable state root calculation for each flashblock
    pub disable_state_root: bool,

    /// Should we disable async state root calculation on full payload resolution
    pub disable_async_calculate_state_root: bool,

    /// The address of the flashblocks number contract.
    ///
    /// If set a builder tx will be added to the start of every flashblock instead of the regular builder tx.
    pub number_contract_address: Option<Address>,

    /// Offset in milliseconds for when to send flashblocks.
    /// Positive values send late, negative values send early.
    pub send_offset_ms: i64,

    /// Time in milliseconds to build the last flashblock early before the end of the slot.
    /// This serves as a buffer time to account for the last flashblock being delayed.
    pub end_buffer_ms: u64,

    /// Port for the p2p node
    pub p2p_port: u16,

    /// Optional hex-encoded private key file path for the p2p node
    pub p2p_private_key_file: Option<String>,

    /// Comma-separated list of multiaddresses of known peers to connect to
    pub p2p_known_peers: Option<String>,

    /// Maximum number of peers for the p2p node
    pub p2p_max_peer_count: u32,

    /// Optional flag to send the full payload to peers
    pub p2p_send_full_payload: bool,

    /// Optional flag to process the full payload received by peers
    pub p2p_process_full_payload: bool,

    /// Maximum number of concurrent WebSocket subscribers
    pub ws_subscriber_limit: Option<u16>,

    /// Whether to replay from the persistence file on startup
    pub replay_from_persistence_file: bool,
}

impl Default for FlashblocksConfig {
    fn default() -> Self {
        Self {
            ws_addr: SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 1111),
            interval: Duration::from_millis(250),
            disable_state_root: false,
            disable_async_calculate_state_root: false,
            number_contract_address: None,
            send_offset_ms: 0,
            end_buffer_ms: 0,
            p2p_port: 9009,
            p2p_private_key_file: None,
            p2p_known_peers: None,
            p2p_max_peer_count: 50,
            p2p_send_full_payload: false,
            p2p_process_full_payload: false,
            ws_subscriber_limit: None,
            replay_from_persistence_file: false,
        }
    }
}

/// Configuration values for the X Layer builder.
#[derive(Clone)]
pub struct BuilderConfig {
    /// Secret key of the builder that is used to sign the end of block transaction.
    pub builder_signer: Option<Signer>,

    /// The interval at which blocks are added to the chain.
    /// This is also the frequency at which the builder will be receiving FCU requests from the
    /// sequencer.
    pub block_time: Duration,

    /// Data Availability configuration for the OP builder
    /// Defines constraints for the maximum size of data availability transactions.
    pub da_config: OpDAConfig,

    /// Gas limit configuration for the payload builder
    pub gas_limit_config: OpGasLimitConfig,

    // The deadline is critical for payload availability. If we reach the deadline,
    // the payload job stops and cannot be queried again. With tight deadlines close
    // to the block number, we risk reaching the deadline before the node queries the payload.
    //
    // Adding 0.5 seconds as wiggle room since block times are shorter here.
    // TODO: A better long-term solution would be to implement cancellation logic
    // that cancels existing jobs when receiving new block building requests.
    //
    // When batcher's max channel duration is big enough (e.g. 10m), the
    // sequencer would send an avalanche of FCUs/getBlockByNumber on
    // each batcher update (with 10m channel it's ~800 FCUs at once).
    // At such moment it can happen that the time b/w FCU and ensuing
    // getPayload would be on the scale of ~2.5s. Therefore we should
    // "remember" the payloads long enough to accommodate this corner-case
    // (without it we are losing blocks). Postponing the deadline for 5s
    // (not just 0.5s) because of that.
    pub block_time_leeway: Duration,

    /// Maximum gas a transaction can use before being excluded.
    pub max_gas_per_txn: Option<u64>,

    /// Configuration values that are specific to the flashblocks builder.
    pub flashblocks: FlashblocksConfig,
}

impl core::fmt::Debug for BuilderConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Config")
            .field(
                "builder_signer",
                &match self.builder_signer.as_ref() {
                    Some(signer) => signer.address.to_string(),
                    None => "None".into(),
                },
            )
            .field("block_time", &self.block_time)
            .field("block_time_leeway", &self.block_time_leeway)
            .field("da_config", &self.da_config)
            .field("gas_limit_config", &self.gas_limit_config)
            .field("flashblocks", &self.flashblocks)
            .field("max_gas_per_txn", &self.max_gas_per_txn)
            .finish()
    }
}

impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            builder_signer: None,
            block_time: Duration::from_secs(2),
            block_time_leeway: Duration::from_millis(500),
            da_config: OpDAConfig::default(),
            gas_limit_config: OpGasLimitConfig::default(),
            flashblocks: FlashblocksConfig::default(),
            max_gas_per_txn: None,
        }
    }
}

impl TryFrom<BuilderArgs> for BuilderConfig {
    type Error = eyre::Report;

    fn try_from(args: BuilderArgs) -> Result<Self, Self::Error> {
        let interval = Duration::from_millis(args.flashblocks.flashblocks_block_time);

        let ws_addr = SocketAddr::new(
            args.flashblocks.flashblocks_addr.parse()?,
            args.flashblocks.flashblocks_port,
        );
        let disable_state_root = args.flashblocks.flashblocks_disable_state_root;
        let disable_async_calculate_state_root =
            args.flashblocks.flashblocks_disable_async_calculate_state_root;
        let number_contract_address = args.flashblocks.flashblocks_number_contract_address;

        Ok(Self {
            builder_signer: args.builder_signer,
            block_time: Duration::from_millis(args.chain_block_time),
            block_time_leeway: Duration::from_secs(args.extra_block_deadline_secs),
            da_config: Default::default(),
            gas_limit_config: Default::default(),
            max_gas_per_txn: args.max_gas_per_txn,
            flashblocks: FlashblocksConfig {
                ws_addr,
                interval,
                disable_state_root,
                disable_async_calculate_state_root,
                number_contract_address,
                send_offset_ms: args.flashblocks.flashblocks_send_offset_ms,
                end_buffer_ms: args.flashblocks.flashblocks_end_buffer_ms,
                p2p_port: args.flashblocks.p2p.p2p_port,
                p2p_private_key_file: args.flashblocks.p2p.p2p_private_key_file,
                p2p_known_peers: args.flashblocks.p2p.p2p_known_peers,
                p2p_max_peer_count: args.flashblocks.p2p.p2p_max_peer_count,
                p2p_send_full_payload: args.flashblocks.p2p.p2p_send_full_payload,
                p2p_process_full_payload: args.flashblocks.p2p.p2p_process_full_payload,
                ws_subscriber_limit: args.flashblocks.ws_subscriber_limit,
                replay_from_persistence_file: args.flashblocks.replay_from_persistence_file,
            },
        })
    }
}

impl BuilderConfig {
    fn flashblocks_per_block(&self) -> u64 {
        if self.block_time.as_millis() == 0 || self.flashblocks.interval.as_millis() == 0 {
            return 0;
        }
        self.block_time.as_millis().div_ceil(self.flashblocks.interval.as_millis()) as u64
    }
}
