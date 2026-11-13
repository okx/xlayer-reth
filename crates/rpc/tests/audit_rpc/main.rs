//! Integration-test binary for the series' audit-RPC tests.
mod audit {
    mod rpc {
        #[allow(unused_imports)]
        use alloy_consensus::transaction::Recovered;
        #[allow(unused_imports)]
        use alloy_consensus::Transaction;
        #[allow(unused_imports)]
        use alloy_eips::BlockId;
        #[allow(unused_imports)]
        use alloy_evm::{Evm, IntoTxEnv};
        #[allow(unused_imports)]
        use alloy_op_evm::OpEvmFactory;
        #[allow(unused_imports)]
        use jsonrpsee::{core::RpcResult, proc_macros::rpc};
        #[allow(unused_imports)]
        use rcs_filter::audit::{
            callstack_only_inspector, callstack_only_projection, deposit_known_allowed_kind,
            observations_from, resolve_known_allowed_kind, unadjudicable_observations,
            unlabelled_known_allowed, KnownAllowedResolution,
        };
        #[allow(unused_imports)]
        use rcs_filter::callstack::{ObservationUnavailable, Observations};
        #[allow(unused_imports)]
        use rcs_filter::capture::{
            CallstackCaptureConfig, CaptureGuard, CaptureOutcome, RcsCaptureControl,
            RcsCaptureScope, RcsInspector,
        };
        #[allow(unused_imports)]
        use rcs_filter::config::{
            is_supported_protocol, MIN_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
        };
        #[allow(unused_imports)]
        use rcs_filter::rules::{load_rules, RawRule, RuleSet, UnknownFields};
        #[allow(unused_imports)]
        use reth_evm::{block::BlockExecutorFactory, ConfigureEvm};
        #[allow(unused_imports)]
        use reth_optimism_primitives::OpTransactionSigned;
        #[allow(unused_imports)]
        use reth_rpc_eth_api::{
            helpers::{Call, LoadState},
            FromEvmError, RpcNodeCore,
        };
        #[allow(unused_imports)]
        use reth_storage_api::BlockReaderIdExt;
        #[allow(unused_imports)]
        use revm::{context_interface::result::ResultAndState, DatabaseCommit};
        #[allow(unused_imports)]
        use std::collections::{BTreeMap, HashSet};
        #[allow(unused_imports)]
        use std::sync::Arc;
        #[allow(unused_imports)]
        use xlayer_rpc::audit::deposit::{build_deposit_tx, DepositTxRequest};
        #[allow(unused_imports)]
        use xlayer_rpc::audit::metrics::{
            KnownAllowedResolutionMetrics, SyntheticBanIdentificationMetrics,
        };
        #[allow(unused_imports)]
        use xlayer_rpc::audit::rpc::test_seams::*;
        #[allow(unused_imports)]
        use xlayer_rpc::audit::rpc::*;
        #[allow(unused_imports)]
        use xlayer_rpc::audit::verdict::AuditResult;
        #[allow(unused_imports)]
        use xlayer_rpc::audit::verdict::CallstackIdentification;
        #[allow(unused_imports)]
        use xlayer_rpc::audit::verdict::ExecutedTx;
        #[allow(unused_imports)]
        use xlayer_rpc::audit::verdict::Verdict;
        mod capture_path_tests;
        mod identification_reporting_tests;
        mod raw_request_wire_tests;
    }
}
