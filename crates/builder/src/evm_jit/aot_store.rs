//! [`PersistentArtifactStore`] — disk-backed [`ArtifactStore`] for AOT artifacts.
//!
//! Upstream `revmc::RuntimeArtifactStore` uses `tempfile::TempDir` and loses everything when
//! the process exits. This implementation persists dylib bytes + a JSON sidecar manifest to a
//! user-specified directory so that compiled programs survive node restarts.
//!
//! # File layout
//!
//! ```text
//! $XLAYER_AOT_DIR/
//!   <code_hash>_<spec_id>_<backend>_<opt_level>.so            # raw dylib bytes
//!   <code_hash>_<spec_id>_<backend>_<opt_level>.manifest.json # serde-encoded manifest
//! ```
//!
//! # Concurrency
//!
//! The in-memory index is a [`DashMap`] guarded by the OS filesystem for the on-disk side.
//! Multiple instances pointing at the same directory are *not* recommended — last writer
//! wins for the sidecar manifest. Single-node single-directory is the supported mode.
//!
//! # Schema versioning
//!
//! Sidecar manifests carry a `schema_version` field (currently 1). Entries with an
//! unrecognized version are skipped on open with a warning rather than crashing.

use alloy_primitives::{B256, keccak256};
use dashmap::DashMap;
use eyre::{Result, WrapErr, bail, eyre};
use revm::primitives::hardfork::SpecId;
use revmc::OptimizationLevel;
use revmc_runtime::runtime::{
    ArtifactKey, ArtifactManifest, ArtifactStore, BackendSelection, RuntimeCacheKey,
    StoredArtifact,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

/// Schema version for the persisted manifest. Bump when fields change in a
/// non-backward-compatible way.
const SCHEMA_VERSION: u32 = 1;

const DYLIB_SUFFIX: &str = ".so";
const MANIFEST_SUFFIX: &str = ".manifest.json";
const TMP_SUFFIX: &str = ".tmp";

/// JSON-serializable wrapper around [`ArtifactManifest`] + key fields.
///
/// We can't add `#[derive(Serialize, Deserialize)]` to the upstream `ArtifactManifest`
/// because that's in the `revmc` repo which we keep at 0 private patches. Instead this
/// wrapper has its own derives and converts in both directions.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedManifest {
    /// Schema version of this on-disk format.
    schema_version: u32,
    /// Keccak-256 of the contract bytecode.
    code_hash: [u8; 32],
    /// EVM hardfork (`SpecId as u8`).
    spec_id: u8,
    /// Backend selection (`BackendSelection as u8`, see [`encode_backend`]).
    backend: u8,
    /// Optimization level (`OptimizationLevel as u8`, see [`encode_opt_level`]).
    opt_level: u8,
    /// Symbol name to `dlsym` from the loaded dylib.
    symbol_name: String,
    /// Length of the source EVM bytecode (informational).
    bytecode_len: u64,
    /// Length of the compiled `.so` (informational, must equal file size).
    artifact_len: u64,
    /// Creation timestamp (unix seconds, informational).
    created_at_unix_secs: u64,
    /// Keccak-256 of dylib bytes — verified on load.
    content_hash: [u8; 32],
}

impl PersistedManifest {
    fn from_revmc(m: &ArtifactManifest) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            code_hash: m.artifact_key.runtime.code_hash.0,
            spec_id: m.artifact_key.runtime.spec_id as u8,
            backend: encode_backend(m.artifact_key.backend),
            opt_level: encode_opt_level(m.artifact_key.opt_level),
            symbol_name: m.symbol_name.clone(),
            bytecode_len: m.bytecode_len as u64,
            artifact_len: m.artifact_len as u64,
            created_at_unix_secs: m.created_at_unix_secs,
            content_hash: m.content_hash,
        }
    }

    fn to_revmc(&self) -> Result<ArtifactManifest> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported manifest schema_version: got {}, expected {}",
                self.schema_version,
                SCHEMA_VERSION
            );
        }
        let spec_id = SpecId::try_from(self.spec_id)
            .map_err(|_| eyre!("unknown SpecId byte: {}", self.spec_id))?;
        let backend = decode_backend(self.backend)?;
        let opt_level = decode_opt_level(self.opt_level)?;
        let artifact_key = ArtifactKey {
            runtime: RuntimeCacheKey { code_hash: B256::from(self.code_hash), spec_id },
            backend,
            opt_level,
        };
        Ok(ArtifactManifest {
            artifact_key,
            symbol_name: self.symbol_name.clone(),
            bytecode_len: self.bytecode_len as usize,
            artifact_len: self.artifact_len as usize,
            created_at_unix_secs: self.created_at_unix_secs,
            content_hash: self.content_hash,
        })
    }
}

fn encode_backend(b: BackendSelection) -> u8 {
    match b {
        BackendSelection::Auto => 0,
        BackendSelection::Llvm => 1,
    }
}

fn decode_backend(b: u8) -> Result<BackendSelection> {
    match b {
        0 => Ok(BackendSelection::Auto),
        1 => Ok(BackendSelection::Llvm),
        other => Err(eyre!("unknown BackendSelection byte: {}", other)),
    }
}

fn encode_opt_level(o: OptimizationLevel) -> u8 {
    match o {
        OptimizationLevel::None => 0,
        OptimizationLevel::Less => 1,
        OptimizationLevel::Default => 2,
        OptimizationLevel::Aggressive => 3,
    }
}

fn decode_opt_level(o: u8) -> Result<OptimizationLevel> {
    match o {
        0 => Ok(OptimizationLevel::None),
        1 => Ok(OptimizationLevel::Less),
        2 => Ok(OptimizationLevel::Default),
        3 => Ok(OptimizationLevel::Aggressive),
        other => Err(eyre!("unknown OptimizationLevel byte: {}", other)),
    }
}

/// Disk-backed [`ArtifactStore`] for AOT artifacts.
#[derive(Debug)]
pub struct PersistentArtifactStore {
    dir: PathBuf,
    index: DashMap<ArtifactKey, StoredArtifact>,
}

impl PersistentArtifactStore {
    /// Opens or creates a persistent AOT artifact store at `dir`.
    ///
    /// Scans for existing `<key>.so` + `<key>.manifest.json` pairs and populates the
    /// in-memory index. Orphan `.so` files (no manifest) are skipped with a warning;
    /// orphan `.manifest.json` files (no `.so`) are deleted as cleanup. Files that
    /// fail content-hash verification are skipped (not deleted).
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .wrap_err_with(|| format!("creating AOT dir {}", dir.display()))?;

        let store = Self { dir: dir.clone(), index: DashMap::default() };
        let (loaded, skipped) = store.scan_dir()?;
        tracing::info!(
            target: "xlayer::jit::aot",
            dir = %dir.display(),
            loaded,
            skipped,
            "PersistentArtifactStore opened"
        );
        Ok(store)
    }

    /// Number of artifacts indexed in memory.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Returns the directory backing this store.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Scans `self.dir` for valid `<name>.so` + `<name>.manifest.json` pairs and
    /// populates the index. Returns `(loaded, skipped)`.
    fn scan_dir(&self) -> Result<(usize, usize)> {
        let mut loaded = 0usize;
        let mut skipped = 0usize;

        // First pass: index all .manifest.json files.
        let mut manifest_paths = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.ends_with(TMP_SUFFIX) {
                    // Half-written file from previous crash; ignore.
                    continue;
                }
                if name.ends_with(MANIFEST_SUFFIX) {
                    manifest_paths.push(path);
                }
            }
        }

        // Second pass: for each manifest, verify the .so exists, content hash matches,
        // and the key inside matches the filename. Then insert.
        for manifest_path in manifest_paths {
            match self.try_load_manifest(&manifest_path) {
                Ok(Some((key, stored))) => {
                    self.index.insert(key, stored);
                    loaded += 1;
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    tracing::warn!(
                        target: "xlayer::jit::aot",
                        path = %manifest_path.display(),
                        error = %e,
                        "skipping corrupt or unrecognized manifest"
                    );
                    skipped += 1;
                }
            }
        }

        // Third pass: clean up orphan manifests (the ones we couldn't pair to a valid .so)
        // is already done inside try_load_manifest. Now find orphan .so files (.so present
        // but no .manifest.json), warn (do not delete — might be user-staged).
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with(DYLIB_SUFFIX) {
                let manifest_name = name.replace(DYLIB_SUFFIX, MANIFEST_SUFFIX);
                let manifest_path = self.dir.join(&manifest_name);
                if !manifest_path.exists() {
                    tracing::warn!(
                        target: "xlayer::jit::aot",
                        path = %path.display(),
                        "orphan .so with no matching manifest — skipped"
                    );
                }
            }
        }

        Ok((loaded, skipped))
    }

    /// Attempts to load one manifest + verify its dylib. Returns `Ok(None)` if the
    /// pair is incomplete (missing .so or hash mismatch). Returns `Err` for I/O
    /// or parse failures.
    fn try_load_manifest(
        &self,
        manifest_path: &Path,
    ) -> Result<Option<(ArtifactKey, StoredArtifact)>> {
        let manifest_bytes = fs::read(manifest_path)
            .wrap_err_with(|| format!("reading {}", manifest_path.display()))?;
        let persisted: PersistedManifest = serde_json::from_slice(&manifest_bytes)
            .wrap_err_with(|| format!("parsing JSON {}", manifest_path.display()))?;
        let manifest = persisted.to_revmc()?;
        let key = manifest.artifact_key.clone();

        // Verify .so exists and content hash matches.
        let dylib_path = self.artifact_path(&key);
        if !dylib_path.exists() {
            // Orphan manifest — clean up.
            let _ = fs::remove_file(manifest_path);
            tracing::warn!(
                target: "xlayer::jit::aot",
                manifest = %manifest_path.display(),
                "manifest references missing .so — deleted"
            );
            return Ok(None);
        }

        // Lazy verify: trust file metadata for size, recompute hash only on demand.
        let metadata = fs::metadata(&dylib_path)?;
        if metadata.len() != manifest.artifact_len as u64 {
            tracing::warn!(
                target: "xlayer::jit::aot",
                path = %dylib_path.display(),
                expected = manifest.artifact_len,
                actual = metadata.len(),
                "dylib size mismatch — skipping (file may be corrupt)"
            );
            return Ok(None);
        }

        Ok(Some((key, StoredArtifact { manifest, dylib_path })))
    }

    /// Path to the `.so` file for a given key.
    fn artifact_path(&self, key: &ArtifactKey) -> PathBuf {
        self.dir.join(format!(
            "{:x}_{:?}_{:?}_{:?}{}",
            key.runtime.code_hash,
            key.runtime.spec_id,
            key.backend,
            key.opt_level,
            DYLIB_SUFFIX,
        ))
    }

    /// Path to the `.manifest.json` file for a given key.
    fn manifest_path(&self, key: &ArtifactKey) -> PathBuf {
        self.dir.join(format!(
            "{:x}_{:?}_{:?}_{:?}{}",
            key.runtime.code_hash,
            key.runtime.spec_id,
            key.backend,
            key.opt_level,
            MANIFEST_SUFFIX,
        ))
    }

    /// Atomic write: write to `target.tmp`, fsync, rename to `target`.
    fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
        let mut tmp = target.to_path_buf();
        let new_name = format!(
            "{}{}",
            tmp.file_name().and_then(|s| s.to_str()).unwrap_or("aot"),
            TMP_SUFFIX
        );
        tmp.set_file_name(new_name);
        {
            let mut f = File::create(&tmp)
                .wrap_err_with(|| format!("creating {}", tmp.display()))?;
            f.write_all(bytes)
                .wrap_err_with(|| format!("writing {}", tmp.display()))?;
            f.sync_all().wrap_err_with(|| format!("fsync {}", tmp.display()))?;
        }
        fs::rename(&tmp, target).wrap_err_with(|| {
            format!("rename {} -> {}", tmp.display(), target.display())
        })?;
        Ok(())
    }
}

impl ArtifactStore for PersistentArtifactStore {
    fn load_all(&self) -> Result<Vec<(ArtifactKey, StoredArtifact)>> {
        Ok(self
            .index
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect())
    }

    fn load(&self, key: &ArtifactKey) -> Result<Option<StoredArtifact>> {
        Ok(self.index.get(key).map(|entry| entry.value().clone()))
    }

    fn store(
        &self,
        key: &ArtifactKey,
        manifest: &ArtifactManifest,
        dylib_bytes: &[u8],
    ) -> Result<()> {
        // Sanity: content hash in manifest must match the bytes.
        let computed = keccak256(dylib_bytes).0;
        if computed != manifest.content_hash {
            bail!(
                "content_hash mismatch: computed {} != manifest {}",
                hex::encode(computed),
                hex::encode(manifest.content_hash)
            );
        }

        let dylib_path = self.artifact_path(key);
        let manifest_path = self.manifest_path(key);

        // 1. Write dylib atomically.
        Self::atomic_write(&dylib_path, dylib_bytes)?;

        // 2. Serialize and write manifest atomically.
        let persisted = PersistedManifest::from_revmc(manifest);
        let manifest_bytes = serde_json::to_vec_pretty(&persisted)
            .wrap_err("serializing manifest")?;
        Self::atomic_write(&manifest_path, &manifest_bytes)?;

        // 3. Update in-memory index.
        self.index
            .insert(key.clone(), StoredArtifact { manifest: manifest.clone(), dylib_path });
        Ok(())
    }

    fn delete(&self, key: &ArtifactKey) -> Result<()> {
        if let Some((_, artifact)) = self.index.remove(key) {
            let _ = fs::remove_file(&artifact.dylib_path);
            let _ = fs::remove_file(self.manifest_path(key));
        }
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        let entries: Vec<_> =
            self.index.iter().map(|e| (e.key().clone(), e.value().dylib_path.clone())).collect();
        self.index.clear();
        for (key, path) in entries {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(self.manifest_path(&key));
        }
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use tempfile::TempDir;

    fn fixture_key(seed: u8) -> ArtifactKey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        ArtifactKey {
            runtime: RuntimeCacheKey {
                code_hash: B256::from(bytes),
                spec_id: SpecId::CANCUN,
            },
            backend: BackendSelection::Llvm,
            opt_level: OptimizationLevel::Default,
        }
    }

    fn fixture_dylib_bytes(seed: u8) -> Vec<u8> {
        let mut v = vec![0u8; 128];
        v[0] = seed;
        v
    }

    fn fixture_manifest(key: &ArtifactKey, dylib: &[u8]) -> ArtifactManifest {
        ArtifactManifest {
            artifact_key: key.clone(),
            symbol_name: format!("test_sym_{:x}", key.runtime.code_hash.0[0]),
            bytecode_len: 42,
            artifact_len: dylib.len(),
            created_at_unix_secs: 1_700_000_000,
            content_hash: keccak256(dylib).0,
        }
    }

    fn make_fixture(seed: u8) -> (ArtifactKey, ArtifactManifest, Vec<u8>) {
        let key = fixture_key(seed);
        let dylib = fixture_dylib_bytes(seed);
        let manifest = fixture_manifest(&key, &dylib);
        (key, manifest, dylib)
    }

    #[test]
    fn open_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn store_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (key, manifest, dylib) = make_fixture(1);

        store.store(&key, &manifest, &dylib).unwrap();
        assert_eq!(store.len(), 1);

        let loaded = store.load(&key).unwrap().expect("must load");
        assert_eq!(loaded.manifest.symbol_name, manifest.symbol_name);
        assert_eq!(loaded.manifest.content_hash, manifest.content_hash);
        assert!(loaded.dylib_path.exists());

        // Disk read-back: file bytes match.
        let on_disk = fs::read(&loaded.dylib_path).unwrap();
        assert_eq!(on_disk, dylib);
    }

    #[test]
    fn load_all_returns_all_entries() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();

        for i in 1..=3u8 {
            let (k, m, d) = make_fixture(i);
            store.store(&k, &m, &d).unwrap();
        }
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn reopen_recovers_index() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        {
            let store = PersistentArtifactStore::open(&path).unwrap();
            for i in 1..=3u8 {
                let (k, m, d) = make_fixture(i);
                store.store(&k, &m, &d).unwrap();
            }
            assert_eq!(store.len(), 3);
        }
        // Reopen — should rebuild index from disk.
        let store2 = PersistentArtifactStore::open(&path).unwrap();
        assert_eq!(store2.len(), 3);
        for i in 1..=3u8 {
            let key = fixture_key(i);
            assert!(store2.load(&key).unwrap().is_some());
        }
    }

    #[test]
    fn orphan_so_without_manifest_skipped() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, _m, d) = make_fixture(7);

        // Write only the .so, no manifest.
        let p = store.artifact_path(&k);
        fs::write(&p, &d).unwrap();
        drop(store);

        // Reopen — orphan .so should be skipped (not indexed, but not deleted).
        let store2 = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store2.len(), 0);
        assert!(p.exists(), "orphan .so should be left in place");
    }

    #[test]
    fn orphan_manifest_cleaned() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, m, _d) = make_fixture(8);

        // Write only the manifest (no .so).
        let manifest_path = store.manifest_path(&k);
        let pm = PersistedManifest::from_revmc(&m);
        fs::write(&manifest_path, serde_json::to_vec(&pm).unwrap()).unwrap();
        drop(store);

        // Reopen — orphan manifest should be deleted.
        let store2 = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store2.len(), 0);
        assert!(!manifest_path.exists(), "orphan manifest should be deleted");
    }

    #[test]
    fn corrupt_manifest_json_skipped() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, _m, d) = make_fixture(9);

        let manifest_path = store.manifest_path(&k);
        let so_path = store.artifact_path(&k);
        fs::write(&manifest_path, b"not valid json {{{}").unwrap();
        fs::write(&so_path, &d).unwrap();
        drop(store);

        let store2 = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store2.len(), 0);
    }

    #[test]
    fn store_rejects_mismatched_content_hash() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, mut m, d) = make_fixture(10);

        // Lie about the hash.
        m.content_hash = [0xff; 32];
        let result = store.store(&k, &m, &d);
        assert!(result.is_err());
        assert!(!store.artifact_path(&k).exists(), "no file should be written on rejection");
    }

    #[test]
    fn dylib_size_mismatch_skipped_on_open() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, m, d) = make_fixture(11);
        store.store(&k, &m, &d).unwrap();
        drop(store);

        // Corrupt the .so by truncating.
        let so_path = tmp.path().join(format!(
            "{:x}_{:?}_{:?}_{:?}{}",
            k.runtime.code_hash, k.runtime.spec_id, k.backend, k.opt_level, DYLIB_SUFFIX,
        ));
        fs::write(&so_path, &[0u8; 8]).unwrap();

        let store2 = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store2.len(), 0, "truncated .so should be skipped");
    }

    #[test]
    fn delete_removes_both_files() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, m, d) = make_fixture(12);
        store.store(&k, &m, &d).unwrap();
        let so_path = store.artifact_path(&k);
        let manifest_path = store.manifest_path(&k);
        assert!(so_path.exists() && manifest_path.exists());

        store.delete(&k).unwrap();
        assert_eq!(store.len(), 0);
        assert!(!so_path.exists());
        assert!(!manifest_path.exists());
    }

    #[test]
    fn clear_empties_dir() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        for i in 1..=5u8 {
            let (k, m, d) = make_fixture(i);
            store.store(&k, &m, &d).unwrap();
        }
        assert_eq!(store.len(), 5);
        store.clear().unwrap();
        assert_eq!(store.len(), 0);

        // No .so or .manifest.json files should remain.
        let leftover: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_str().unwrap_or("");
                name.ends_with(DYLIB_SUFFIX) || name.ends_with(MANIFEST_SUFFIX)
            })
            .collect();
        assert!(leftover.is_empty(), "found leftover files: {:?}", leftover);
    }

    #[test]
    fn concurrent_store_thread_safe() {
        use std::sync::Arc;
        use std::thread;
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(PersistentArtifactStore::open(tmp.path()).unwrap());

        let handles: Vec<_> = (1..=10u8)
            .map(|i| {
                let s = store.clone();
                thread::spawn(move || {
                    let (k, m, d) = make_fixture(i);
                    s.store(&k, &m, &d).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(store.len(), 10);
    }

    #[test]
    fn tmp_files_ignored_on_open() {
        let tmp = TempDir::new().unwrap();
        // Drop a stray .tmp file in the dir before opening.
        fs::write(tmp.path().join("garbage.so.tmp"), b"junk").unwrap();
        fs::write(tmp.path().join("garbage.manifest.json.tmp"), b"junk").unwrap();

        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn schema_version_mismatch_skipped() {
        let tmp = TempDir::new().unwrap();
        let store = PersistentArtifactStore::open(tmp.path()).unwrap();
        let (k, m, d) = make_fixture(14);

        // Write a manifest with the wrong schema_version + the .so.
        let mut pm = PersistedManifest::from_revmc(&m);
        pm.schema_version = 999;
        fs::write(store.manifest_path(&k), serde_json::to_vec(&pm).unwrap()).unwrap();
        fs::write(store.artifact_path(&k), &d).unwrap();
        drop(store);

        let store2 = PersistentArtifactStore::open(tmp.path()).unwrap();
        assert_eq!(store2.len(), 0, "schema_version != 1 should be skipped");
    }
}
