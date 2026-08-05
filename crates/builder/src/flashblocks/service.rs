use crate::{
    flashblocks::{
        builder::FlashblocksBuilder,
        builder_tx::FlashblocksBuilderTx,
        generator::BlockPayloadJobGenerator,
        handler::FlashblocksPayloadHandler,
        handler_ctx::FlashblockHandlerContext,
        utils::{
            cache::FlashblockPayloadsCache,
            p2p::{Message, AGENT_VERSION, FLASHBLOCKS_STREAM_PROTOCOL},
            wspub::WebSocketPublisher,
        },
        BuilderConfig,
    },
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
    traits::{NodeBounds, PoolBounds},
};
use eyre::WrapErr as _;
use std::{collections::HashMap, sync::Arc, time::Duration};

use reth_basic_payload_builder::BasicPayloadJobGeneratorConfig;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::PayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_primitives_traits::BlockBody;
use reth_provider::CanonStateSubscriptions;
use reth_transaction_pool::{TransactionListenerKind, TransactionPool};

pub struct FlashblocksServiceBuilder {
    pub config: BuilderConfig,
    pub bridge_intercept: xlayer_bridge_intercept::BridgeInterceptConfig,
}

impl FlashblocksServiceBuilder {
    /// Set the bridge intercept config to apply to the payload builder.
    pub fn with_bridge_intercept(
        &mut self,
        config: xlayer_bridge_intercept::BridgeInterceptConfig,
    ) -> &mut Self {
        self.bridge_intercept = config;
        self
    }

    fn spawn_payload_builder_service<Node, Pool>(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        builder_tx: FlashblocksBuilderTx,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>>
    where
        Node: NodeBounds,
        Pool: PoolBounds,
    {
        // TODO: is there a different global token?
        // this is effectively unused right now due to the usage of reth's `task_executor`.
        let cancel = tokio_util::sync::CancellationToken::new();

        let (incoming_message_rx, outgoing_message_tx) = if self.config.flashblocks.p2p_enabled {
            let mut builder = crate::p2p::NodeBuilder::new();

            if let Some(ref private_key_file) = self.config.flashblocks.p2p_private_key_file
                && !private_key_file.is_empty()
            {
                let private_key_hex = std::fs::read_to_string(private_key_file)
                    .wrap_err_with(|| {
                        format!("failed to read p2p private key file: {private_key_file}")
                    })?
                    .trim()
                    .to_string();
                builder = builder.with_keypair_hex_string(private_key_hex);
            }

            let known_peers: Vec<crate::p2p::Multiaddr> =
                if let Some(ref p2p_known_peers) = self.config.flashblocks.p2p_known_peers {
                    p2p_known_peers
                        .split(',')
                        .map(|s| s.to_string())
                        .filter_map(|s| s.parse().ok())
                        .collect()
                } else {
                    vec![]
                };

            let crate::p2p::NodeBuildResult { node, outgoing_message_tx, mut incoming_message_rxs } =
                builder
                    .with_agent_version(AGENT_VERSION.to_string())
                    .with_protocol(FLASHBLOCKS_STREAM_PROTOCOL)
                    .with_known_peers(known_peers)
                    .with_port(self.config.flashblocks.p2p_port)
                    .with_cancellation_token(cancel.clone())
                    .with_max_peer_count(self.config.flashblocks.p2p_max_peer_count)
                    .try_build::<Message>()
                    .wrap_err("failed to build flashblocks p2p node")?;
            let multiaddrs = node.multiaddrs();
            ctx.task_executor().spawn_task(async move {
                if let Err(e) = node.run().await {
                    tracing::error!(error = %e, "p2p node exited");
                }
            });
            tracing::info!(target: "payload_builder", multiaddrs = ?multiaddrs, "flashblocks p2p node started");

            let incoming_message_rx = incoming_message_rxs
                .remove(&FLASHBLOCKS_STREAM_PROTOCOL)
                .expect("flashblocks p2p protocol must be found in receiver map");
            (incoming_message_rx, outgoing_message_tx)
        } else {
            let (_incoming_message_tx, incoming_message_rx) = tokio::sync::mpsc::channel(16);
            let (outgoing_message_tx, _outgoing_message_rx) = tokio::sync::mpsc::channel(16);
            (incoming_message_rx, outgoing_message_tx)
        };

        let metrics = Arc::new(BuilderMetrics::default());
        let task_metrics = Arc::new(FlashblocksTaskMetrics::new());

        // Channels for built flashblock payloads
        let (built_fb_payload_tx, built_fb_payload_rx) = tokio::sync::mpsc::channel(16);
        // Channels for built full block payloads
        let (built_payload_tx, built_payload_rx) = tokio::sync::mpsc::channel(16);

        let p2p_cache = if self.config.flashblocks.replay_from_persistence_file {
            FlashblockPayloadsCache::new(Some(ctx.config().datadir()))
        } else {
            FlashblockPayloadsCache::new(None)
        };

        let ws_pub: Arc<WebSocketPublisher> = WebSocketPublisher::new(
            self.config.flashblocks.ws_addr,
            metrics.clone(),
            &task_metrics.websocket_publisher,
            self.config.flashblocks.ws_subscriber_limit,
        )
        .wrap_err("failed to create ws publisher")?
        .into();
        let rcs_tx_pool = self.config.rcs_filter.as_ref().map(|_| pool.clone());
        let mut payload_builder = FlashblocksBuilder::new(
            OpEvmConfig::optimism(ctx.chain_spec()),
            pool,
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            self.config.clone(),
            builder_tx,
            built_fb_payload_tx,
            built_payload_tx,
            p2p_cache.clone(),
            ws_pub.clone(),
            metrics.clone(),
            task_metrics.clone(),
        );
        payload_builder.bridge_intercept_config = self.bridge_intercept.clone();
        let payload_job_config = BasicPayloadJobGeneratorConfig::default();

        let payload_generator = BlockPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
            true,
            self.config.block_time_leeway,
        );

        let (payload_service, payload_builder_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        if let Some(filter) = self.config.rcs_filter.clone() {
            let mut canonical_notifications = ctx.provider().subscribe_to_canonical_state();
            ctx.task_executor().spawn_critical_task(
                "rcs filter canonical cleanup",
                Box::pin(async move {
                    loop {
                        match canonical_notifications.recv().await {
                            Ok(notification) => {
                                let hashes = notification
                                    .committed()
                                    .blocks_iter()
                                    .flat_map(|block| block.body().transactions_iter())
                                    .map(|tx| alloy_primitives::B256::from(*tx.tx_hash()))
                                    .collect::<Vec<_>>();
                                filter.remove_canonical_transactions(&hashes);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                let removed = filter.recover_after_canonical_lag(skipped);
                                tracing::warn!(
                                    target: "rcs_filter",
                                    skipped,
                                    removed,
                                    "canonical receiver lagged; invalidated reusable filter state"
                                );
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }),
            );
        }

        if let (Some(filter), Some(tx_pool)) = (self.config.rcs_filter.clone(), rcs_tx_pool) {
            let terminal_events = filter.subscribe_terminal_events();
            ctx.task_executor().spawn_critical_task(
                "rcs filter terminal discard",
                Box::pin(run_terminal_discard(filter, tx_pool, terminal_events)),
            );
        }

        let handler_ctx = FlashblockHandlerContext::new(
            &ctx.provider().clone(),
            self.config.clone(),
            OpEvmConfig::optimism(ctx.chain_spec()),
            metrics.clone(),
        )
        .wrap_err("failed to create flashblocks payload builder context")?;

        let payload_handler = FlashblocksPayloadHandler::new(
            handler_ctx,
            built_fb_payload_rx,
            built_payload_rx,
            incoming_message_rx,
            outgoing_message_tx,
            payload_service.payload_events_handle(),
            p2p_cache.clone(),
            ws_pub.clone(),
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            cancel,
            self.config.flashblocks.p2p_send_full_payload,
            self.config.flashblocks.p2p_process_full_payload && self.config.rcs_filter.is_none(),
        );

        ctx.task_executor().spawn_critical_task(
            "custom payload builder service",
            Box::pin(task_metrics.payload_builder_service.instrument(payload_service)),
        );
        ctx.task_executor().spawn_critical_task(
            "flashblocks payload handler",
            Box::pin(task_metrics.payload_handler.instrument(payload_handler.run())),
        );

        // Spawn the tokio metrics collector (records metrics every second)
        task_metrics.clone().spawn_metrics_collector(Duration::from_secs(1));

        tracing::info!(target: "payload_builder", "Flashblocks payload builder service started");
        Ok(payload_builder_handle)
    }
}

fn discard_transaction<Pool: TransactionPool>(
    filter: &rcs_filter::FilterHandle,
    tx_pool: &Pool,
    hash: alloy_primitives::B256,
    generation: u64,
) -> bool {
    let Some(removed) = filter
        .with_dropped_lifecycle(&hash, generation, || tx_pool.remove_transaction(hash).is_some())
    else {
        return false;
    };
    filter.record_txpool_discard(removed);
    true
}

async fn run_terminal_discard<Pool: TransactionPool + Unpin + 'static>(
    filter: Arc<rcs_filter::FilterHandle>,
    tx_pool: Pool,
    mut terminal_events: tokio::sync::broadcast::Receiver<rcs_filter::TerminalEvent>,
) {
    let mut new_transactions = tx_pool.new_transactions_listener_for(TransactionListenerKind::All);
    let mut dropped = filter.dropped_lifecycles().into_iter().collect::<HashMap<_, _>>();
    let mut reconciliation = tokio::time::interval(Duration::from_secs(1));
    reconciliation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            terminal = terminal_events.recv() => match terminal {
                Ok(event) => {
                    if !discard_transaction(&filter, &tx_pool, event.tx_hash, event.generation) {
                        continue;
                    }
                    dropped.insert(event.tx_hash, event.generation);
                    tracing::debug!(
                        target: "rcs_filter",
                        tx_hash = %event.tx_hash,
                        ?event.reason,
                        "applied terminal discard to txpool"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    dropped = filter.dropped_lifecycles().into_iter().collect();
                    reconcile_dropped(&filter, &tx_pool, &dropped);
                    filter.record_terminal_reconciliation(skipped, dropped.len());
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            new_tx = new_transactions.recv() => match new_tx {
                Some(event) => {
                    let hash = *event.transaction.hash();
                    if let Some(generation) = dropped.get(&hash).copied()
                        && !discard_transaction(&filter, &tx_pool, hash, generation)
                    {
                        dropped.remove(&hash);
                    }
                }
                None => break,
            },
            _ = reconciliation.tick() => {
                dropped = filter.dropped_lifecycles().into_iter().collect();
                reconcile_dropped(&filter, &tx_pool, &dropped);
                let removed = reconcile_absent_non_terminal(&filter, &tx_pool);
                if removed > 0 {
                    tracing::debug!(
                        target: "rcs_filter",
                        removed,
                        "removed non-terminal filter lifecycles absent from txpool"
                    );
                }
            }
        }
    }
}

fn reconcile_absent_non_terminal<Pool: TransactionPool>(
    filter: &rcs_filter::FilterHandle,
    tx_pool: &Pool,
) -> usize {
    filter
        .non_terminal_lifecycles()
        .into_iter()
        .filter(|(hash, generation)| {
            !tx_pool.contains(hash) && filter.remove_non_terminal_if_generation(hash, *generation)
        })
        .count()
}

fn reconcile_dropped<Pool: TransactionPool>(
    filter: &rcs_filter::FilterHandle,
    tx_pool: &Pool,
    lifecycles: &HashMap<alloy_primitives::B256, u64>,
) {
    for (hash, generation) in lifecycles {
        discard_transaction(filter, tx_pool, *hash, *generation);
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, OpEvmConfig> for FlashblocksServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        _: OpEvmConfig,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>> {
        let signer = self.config.builder_signer;

        let builder_tx = if let Some(builder_signer) = signer
            && let Some(flashblocks_number_contract_address) =
                self.config.flashblocks.number_contract_address
        {
            FlashblocksBuilderTx::new_number_contract(
                builder_signer,
                flashblocks_number_contract_address,
            )
        } else {
            FlashblocksBuilderTx::new_base(signer)
        };

        self.spawn_payload_builder_service(ctx, pool, builder_tx)
    }
}

#[cfg(test)]
mod rcs_txpool_tests {
    use super::{reconcile_absent_non_terminal, run_terminal_discard};
    use alloy_primitives::{Address, B256};
    use reth_transaction_pool::{
        test_utils::{testing_pool, MockTransaction},
        TransactionOrigin, TransactionPool,
    };
    use std::time::Duration;

    #[tokio::test]
    async fn real_txpool_discard_parks_nonce_descendant() {
        let pool = testing_pool();
        let sender = Address::repeat_byte(0x11);
        let root_hash = B256::repeat_byte(0x01);
        let child_hash = B256::repeat_byte(0x02);
        let root = MockTransaction::legacy()
            .with_sender(sender)
            .with_nonce(0)
            .with_gas_price(100)
            .with_hash(root_hash);
        let child = MockTransaction::legacy()
            .with_sender(sender)
            .with_nonce(1)
            .with_gas_price(100)
            .with_hash(child_hash);

        pool.add_transaction(TransactionOrigin::External, root).await.unwrap();
        pool.add_transaction(TransactionOrigin::External, child).await.unwrap();
        assert_eq!(pool.pending_and_queued_txn_count(), (2, 0));

        assert!(pool.remove_transaction(root_hash).is_some());
        assert!(pool.get(&root_hash).is_none());
        assert!(pool.get(&child_hash).is_some());
        assert_eq!(pool.pending_and_queued_txn_count(), (0, 1));
    }

    #[tokio::test]
    async fn terminal_consumer_removes_real_pool_tx_and_rebroadcast() {
        let pool = testing_pool();
        let sender = rcs_filter::test_support::golden::origin();
        let root_hash = rcs_filter::test_support::golden::tx_a();
        let child_hash = B256::repeat_byte(0x04);
        let make_root = || {
            MockTransaction::legacy()
                .with_sender(sender)
                .with_nonce(0)
                .with_gas_price(100)
                .with_hash(root_hash)
        };
        let child = MockTransaction::legacy()
            .with_sender(sender)
            .with_nonce(1)
            .with_gas_price(100)
            .with_hash(child_hash);
        pool.add_transaction(TransactionOrigin::External, make_root()).await.unwrap();
        pool.add_transaction(TransactionOrigin::External, child).await.unwrap();

        let mock = std::sync::Arc::new(rcs_filter::test_support::MockRcsClient::new());
        mock.set_rules_fixture(&[rcs_filter::test_support::golden::RULE_SCENARIO_A]);
        mock.register_query_state(rcs_filter::test_support::golden::TX_A, "denied", None);
        let config = rcs_filter::FilterConfig {
            enabled: true,
            rcs_base_url: "http://unused.test".into(),
            batch_window: Duration::from_millis(10),
            rules_version_poll_interval: Duration::from_secs(60),
            ..Default::default()
        };
        let filter = rcs_filter::FilterHandle::spawn(
            config,
            mock as std::sync::Arc<dyn rcs_filter::RcsClient>,
            std::sync::Arc::new(rcs_filter::SystemClock),
        );
        let terminal_rx = filter.subscribe_terminal_events();
        let filter_for_screen = filter.clone();
        let task = tokio::spawn(run_terminal_discard(filter, pool.clone(), terminal_rx));
        let logs = vec![rcs_filter::test_support::log_builder::erc20_transfer(
            rcs_filter::test_support::golden::token_x(),
            rcs_filter::test_support::golden::bridge_erc20(),
            rcs_filter::test_support::golden::recipient(),
            rcs_filter::test_support::golden::one_token(),
        )];
        let input = rcs_filter::ScreenInput {
            tx_hash: root_hash,
            origin: sender,
            tx_to: Some(rcs_filter::test_support::golden::claim_contract()),
            nonce: 0,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while !filter_for_screen.is_ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(filter_for_screen.screen_tx(&input), rcs_filter::Screen::AuditPending);

        tokio::time::timeout(Duration::from_secs(3), async {
            while pool.get(&root_hash).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(pool.get(&child_hash).is_some());
        assert_eq!(pool.pending_and_queued_txn_count(), (0, 1));

        pool.add_transaction(TransactionOrigin::External, make_root()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.get(&root_hash).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(pool.get(&child_hash).is_some());
        assert_eq!(pool.pending_and_queued_txn_count(), (0, 1));

        assert_eq!(filter_for_screen.remove_canonical_transactions(&[root_hash]), 1);
        pool.add_transaction(TransactionOrigin::External, make_root()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(pool.get(&root_hash).is_some());
        assert_eq!(pool.pending_and_queued_txn_count(), (2, 0));
        task.abort();
    }

    #[tokio::test]
    async fn non_terminal_reconciliation_tracks_real_txpool_presence() {
        let pool = testing_pool();
        let sender = rcs_filter::test_support::golden::origin();
        let root_hash = rcs_filter::test_support::golden::tx_a();
        let root = MockTransaction::legacy()
            .with_sender(sender)
            .with_nonce(0)
            .with_gas_price(100)
            .with_hash(root_hash);
        pool.add_transaction(TransactionOrigin::External, root).await.unwrap();

        let rules = rcs_filter::rules::load_rules(
            1,
            1,
            vec![serde_json::from_str(rcs_filter::test_support::golden::RULE_SCENARIO_A).unwrap()],
        );
        let filter = rcs_filter::FilterHandle::for_test(
            rcs_filter::FilterConfig::default(),
            rules,
            std::sync::Arc::new(rcs_filter::SystemClock),
        );
        let logs = vec![rcs_filter::test_support::log_builder::erc20_transfer(
            rcs_filter::test_support::golden::token_x(),
            rcs_filter::test_support::golden::bridge_erc20(),
            rcs_filter::test_support::golden::recipient(),
            rcs_filter::test_support::golden::one_token(),
        )];
        let input = rcs_filter::ScreenInput {
            tx_hash: root_hash,
            origin: sender,
            tx_to: Some(rcs_filter::test_support::golden::claim_contract()),
            nonce: 0,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
        };
        assert_eq!(filter.screen_tx(&input), rcs_filter::Screen::AuditPending);

        let old_generation = filter.non_terminal_lifecycles()[0].1;
        assert_eq!(reconcile_absent_non_terminal(&filter, &pool), 0);
        assert_eq!(filter.buffered_len(), 1);

        assert_eq!(filter.remove_canonical_transactions(&[root_hash]), 1);
        assert_eq!(filter.screen_tx(&input), rcs_filter::Screen::AuditPending);
        let new_generation = filter.non_terminal_lifecycles()[0].1;
        assert_ne!(old_generation, new_generation);
        assert!(!filter.remove_non_terminal_if_generation(&root_hash, old_generation));
        assert_eq!(filter.buffered_len(), 1, "stale reconciliation cannot remove reinsertion");

        assert!(pool.remove_transaction(root_hash).is_some());
        assert_eq!(reconcile_absent_non_terminal(&filter, &pool), 1);
        assert_eq!(filter.buffered_len(), 0);
    }

    #[tokio::test]
    async fn approved_reconciliation_removes_absent_real_txpool_entry() {
        let pool = testing_pool();
        let sender = rcs_filter::test_support::golden::origin();
        let root_hash = rcs_filter::test_support::golden::tx_a();
        let root = MockTransaction::legacy()
            .with_sender(sender)
            .with_nonce(0)
            .with_gas_price(100)
            .with_hash(root_hash);
        pool.add_transaction(TransactionOrigin::External, root).await.unwrap();

        let mock = std::sync::Arc::new(rcs_filter::test_support::MockRcsClient::new());
        mock.set_rules_fixture(&[rcs_filter::test_support::golden::RULE_SCENARIO_A]);
        mock.register_query_state(rcs_filter::test_support::golden::TX_A, "approved", None);
        let filter = rcs_filter::FilterHandle::spawn(
            rcs_filter::FilterConfig {
                enabled: true,
                rcs_base_url: "http://unused.test".into(),
                batch_window: Duration::from_millis(10),
                ..Default::default()
            },
            mock as std::sync::Arc<dyn rcs_filter::RcsClient>,
            std::sync::Arc::new(rcs_filter::SystemClock),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !filter.is_ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let logs = [rcs_filter::test_support::log_builder::erc20_transfer(
            rcs_filter::test_support::golden::token_x(),
            rcs_filter::test_support::golden::bridge_erc20(),
            rcs_filter::test_support::golden::recipient(),
            rcs_filter::test_support::golden::one_token(),
        )];
        let input = rcs_filter::ScreenInput {
            tx_hash: root_hash,
            origin: sender,
            tx_to: Some(rcs_filter::test_support::golden::claim_contract()),
            nonce: 0,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
        };
        assert_eq!(filter.screen_tx(&input), rcs_filter::Screen::AuditPending);
        tokio::time::timeout(Duration::from_secs(3), async {
            while filter.buffer_status(&root_hash) != Some(rcs_filter::BufferStatus::Approved) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(pool.remove_transaction(root_hash).is_some());
        assert_eq!(reconcile_absent_non_terminal(&filter, &pool), 1);
        assert_eq!(filter.buffered_len(), 0);
    }
}
