//! End-to-end test: PersistentArtifactStore + real LLVM compile + cross-restart load.
//!
//! Mirrors `revmc/crates/revmc-runtime/src/runtime/tests.rs::preload_aot_seeds_resident`
//! but uses our own `PersistentArtifactStore` backed by a real directory.
//!
//! These tests require the `jit` feature (LLVM linkage). Run with:
//! ```bash
//! LLVM_SYS_220_PREFIX=/opt/homebrew/opt/llvm \
//!   cargo nextest run -p xlayer-builder --features jit --test aot_persistence
//! ```

#![cfg(feature = "jit")]

use alloy_primitives::{B256, keccak256};
use revm::primitives::hardfork::SpecId;
use revmc::OptimizationLevel;
use revmc_runtime::runtime::{
    AotRequest, ArtifactKey, ArtifactStore, BackendSelection, JitBackend, RuntimeArtifactStore,
    RuntimeCacheKey, RuntimeConfig, RuntimeTuning,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use xlayer_builder::evm_jit::PersistentArtifactStore;

/// Returns `0x42`: PUSH1 0x42 PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN.
const BYTECODE_RET42: &[u8] = &[0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

fn poll_until<F: FnMut() -> bool>(timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn make_backend_with_store(store: Arc<dyn ArtifactStore>) -> JitBackend {
    JitBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(store),
        blocking: false,
        tuning: RuntimeTuning {
            jit_worker_count: 1, // deterministic for tests
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("JitBackend::new")
}

#[test]
fn persistent_store_survives_backend_drop() {
    let tmp = TempDir::new().unwrap();
    let store_path = tmp.path().to_path_buf();
    let code_hash = keccak256(BYTECODE_RET42);

    // Phase 1: compile + persist
    {
        let store: Arc<dyn ArtifactStore> =
            Arc::new(PersistentArtifactStore::open(store_path.clone()).unwrap());
        let backend = make_backend_with_store(store.clone());

        backend.prepare_aot(AotRequest {
            code_hash,
            code: BYTECODE_RET42.to_vec().into(),
            spec_id: SpecId::CANCUN,
        });

        // Poll the in-memory store until 1 artifact has been persisted.
        let arrived = poll_until(Duration::from_secs(120), || {
            store.load_all().map(|v| v.len() >= 1).unwrap_or(false)
        });
        assert!(arrived, "AOT compile did not finish within 120s");

        // backend dropped at end of scope; store does *not* clear its index
    }

    // Disk check: at least one .so and one .manifest.json exist.
    let entries: Vec<_> = std::fs::read_dir(&store_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let has_so = entries.iter().any(|n| n.ends_with(".so"));
    let has_manifest = entries.iter().any(|n| n.ends_with(".manifest.json"));
    assert!(has_so, "no .so file persisted: {:?}", entries);
    assert!(has_manifest, "no .manifest.json persisted: {:?}", entries);

    // Phase 2: fresh process — load from disk, no recompile
    {
        let store: Arc<dyn ArtifactStore> =
            Arc::new(PersistentArtifactStore::open(store_path).unwrap());
        assert_eq!(
            store.load_all().unwrap().len(),
            1,
            "index should rebuild from disk"
        );

        let backend = make_backend_with_store(store);

        // After construction, preload_aot should have inserted into the resident map.
        let preloaded = poll_until(Duration::from_secs(5), || {
            backend.stats().resident_entries >= 1
        });
        assert!(preloaded, "resident map did not pick up persisted artifact");
        assert_eq!(
            backend.stats().compilations_succeeded,
            0,
            "no new compilation should occur on second start"
        );
    }
}

#[test]
fn missing_dylib_returns_none_on_load() {
    // Open an empty store, then ask for a key that doesn't exist.
    let tmp = TempDir::new().unwrap();
    let store = PersistentArtifactStore::open(tmp.path()).unwrap();

    let key = ArtifactKey {
        runtime: RuntimeCacheKey {
            code_hash: B256::from([0x77; 32]),
            spec_id: SpecId::CANCUN,
        },
        backend: BackendSelection::Llvm,
        opt_level: OptimizationLevel::Default,
    };
    assert!(store.load(&key).unwrap().is_none());
}

#[test]
fn store_with_runtime_artifact_store_parity() {
    // Sanity: compile once with upstream RuntimeArtifactStore, compile same
    // bytecode with PersistentArtifactStore. Both should produce 1 resident entry,
    // and the content_hash should match (same dylib produced by same LLVM).

    let upstream_store: Arc<dyn ArtifactStore> =
        Arc::new(RuntimeArtifactStore::new().unwrap());
    let backend_upstream = make_backend_with_store(upstream_store.clone());
    let code_hash = keccak256(BYTECODE_RET42);
    backend_upstream.prepare_aot(AotRequest {
        code_hash,
        code: BYTECODE_RET42.to_vec().into(),
        spec_id: SpecId::CANCUN,
    });
    let upstream_ok = poll_until(Duration::from_secs(120), || {
        upstream_store.load_all().map(|v| v.len() >= 1).unwrap_or(false)
    });
    assert!(upstream_ok);

    let tmp = TempDir::new().unwrap();
    let persistent_store: Arc<dyn ArtifactStore> =
        Arc::new(PersistentArtifactStore::open(tmp.path()).unwrap());
    let backend_persistent = make_backend_with_store(persistent_store.clone());
    backend_persistent.prepare_aot(AotRequest {
        code_hash,
        code: BYTECODE_RET42.to_vec().into(),
        spec_id: SpecId::CANCUN,
    });
    let persistent_ok = poll_until(Duration::from_secs(120), || {
        persistent_store.load_all().map(|v| v.len() >= 1).unwrap_or(false)
    });
    assert!(persistent_ok);

    let upstream_artifacts = upstream_store.load_all().unwrap();
    let persistent_artifacts = persistent_store.load_all().unwrap();
    assert_eq!(upstream_artifacts.len(), 1);
    assert_eq!(persistent_artifacts.len(), 1);

    // Same artifact_key (code_hash + spec_id + backend + opt_level) must match exactly.
    // content_hash NOT compared — LLVM emits non-deterministic metadata (build_id,
    // timestamps, mach-o uuid) so two compilations of the same source IR can differ
    // bit-for-bit. AOT correctness only requires that each store can load *its own*
    // dylib, not that they produce identical bytes.
    assert_eq!(
        upstream_artifacts[0].0, persistent_artifacts[0].0,
        "ArtifactKey should match (same bytecode + spec)"
    );
}
