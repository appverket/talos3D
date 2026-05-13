//! Catalog cache abstractions and implementations.
//!
//! [`CatalogCache`] is the storage interface used by [`crate::ChangePoller`] and
//! by consumers of the catalog client that want to persist cursor, blobs, and
//! resolution pointers across sessions. Two implementations ship:
//!
//! - [`WorkspaceRemoteCache`] — native-only, filesystem-backed under either an
//!   explicit workspace root or the OS cache directory. This is the durable
//!   cache used by the desktop binary.
//! - [`InMemoryCatalogCache`] — works on every target (including
//!   `wasm32-unknown-unknown`). Holds everything in process memory; loses state
//!   on reload. Useful for tests and as a wasm bootstrap before a richer
//!   storage backend (IndexedDB, localStorage) is wired up.
//!
//! Layout for [`WorkspaceRemoteCache`]:
//! ```text
//! <cache-root>/
//!   blobs/<hash[0:2]>/<hash>.bin   (immutable — same hash = same bytes forever)
//!   resolution/<kind>/<canonical_id-url-encoded>.json   (mutable, atomic rename)
//!   cursor                          (mutable, atomic rename — last-polled cursor)
//! ```

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        RwLock,
    },
};

use serde::{Deserialize, Serialize};

// ---- ResolutionPointer ------------------------------------------------------

/// A pinned pointer to a resolved artifact stored in the blob cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionPointer {
    pub revision: i32,
    /// Lowercase hex sha256 content hash.
    pub content_hash: String,
    pub scope: String,
    pub kind: String,
}

// ---- CatalogCache trait -----------------------------------------------------

/// Storage interface for cursor, blobs, and resolution pointers.
///
/// All methods are synchronous and infallible-on-cache-miss: `read_*` returns
/// `None`/`0` on a missing entry rather than erroring. Writes can fail (I/O
/// errors on the native filesystem impl); errors propagate verbatim.
///
/// Implementations must be cheap to clone via `Arc` (they're consumed as
/// `Arc<dyn CatalogCache>` by [`crate::ChangePoller`]).
pub trait CatalogCache: Send + Sync + 'static {
    /// Last successfully written cursor, or `0` on missing/corrupt.
    fn read_cursor(&self) -> i64;

    /// Persist `cursor` durably. Native impls write atomically.
    fn write_cursor(&self, cursor: i64) -> Result<(), std::io::Error>;

    /// Read cached blob bytes by hex content hash, or `None` on cache miss.
    fn read_blob(&self, content_hash_hex: &str) -> Option<Vec<u8>>;

    /// Write blob bytes for `content_hash_hex`.
    ///
    /// Blobs are **immutable**: if the entry already exists, this is a
    /// no-op. The hash addresses one canonical byte string.
    fn write_blob(&self, content_hash_hex: &str, bytes: &[u8]) -> Result<(), std::io::Error>;

    /// Read the resolution pointer for `(kind, canonical_id)`.
    fn read_resolution(&self, kind: &str, canonical_id: &str) -> Option<ResolutionPointer>;

    /// Write the resolution pointer for `(kind, canonical_id)`.
    fn write_resolution(
        &self,
        kind: &str,
        canonical_id: &str,
        ptr: &ResolutionPointer,
    ) -> Result<(), std::io::Error>;
}

// ---- In-memory implementation (any target, including wasm32) ----------------

/// Process-memory `CatalogCache`. Loses state on drop; works on every target.
///
/// Cheap to construct via [`InMemoryCatalogCache::new`]; thread-safe via
/// internal `RwLock`s.
#[derive(Debug, Default)]
pub struct InMemoryCatalogCache {
    cursor: AtomicI64,
    blobs: RwLock<HashMap<String, Vec<u8>>>,
    resolutions: RwLock<HashMap<(String, String), ResolutionPointer>>,
}

impl InMemoryCatalogCache {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CatalogCache for InMemoryCatalogCache {
    fn read_cursor(&self) -> i64 {
        self.cursor.load(Ordering::Acquire)
    }

    fn write_cursor(&self, cursor: i64) -> Result<(), std::io::Error> {
        self.cursor.store(cursor, Ordering::Release);
        Ok(())
    }

    fn read_blob(&self, content_hash_hex: &str) -> Option<Vec<u8>> {
        self.blobs.read().ok()?.get(content_hash_hex).cloned()
    }

    fn write_blob(&self, content_hash_hex: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
        let mut guard = self
            .blobs
            .write()
            .map_err(|_| std::io::Error::other("blob lock poisoned"))?;
        // Immutability: don't overwrite an existing entry.
        guard
            .entry(content_hash_hex.to_owned())
            .or_insert_with(|| bytes.to_vec());
        Ok(())
    }

    fn read_resolution(&self, kind: &str, canonical_id: &str) -> Option<ResolutionPointer> {
        self.resolutions
            .read()
            .ok()?
            .get(&(kind.to_owned(), canonical_id.to_owned()))
            .cloned()
    }

    fn write_resolution(
        &self,
        kind: &str,
        canonical_id: &str,
        ptr: &ResolutionPointer,
    ) -> Result<(), std::io::Error> {
        let mut guard = self
            .resolutions
            .write()
            .map_err(|_| std::io::Error::other("resolution lock poisoned"))?;
        guard.insert((kind.to_owned(), canonical_id.to_owned()), ptr.clone());
        Ok(())
    }
}

// ---- Filesystem-backed implementation (native only) -------------------------

#[cfg(not(target_arch = "wasm32"))]
mod fs_impl {
    use super::{CatalogCache, ResolutionPointer};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use anyhow::Context;
    use etcetera::{AppStrategy, AppStrategyArgs};
    use tracing::{debug, warn};

    /// Content-addressed, workspace-local cache for remote catalog artifacts.
    ///
    /// Cloning this struct is cheap — all paths are derived from the root.
    #[derive(Debug, Clone)]
    pub struct WorkspaceRemoteCache {
        pub(super) root: PathBuf,
    }

    impl WorkspaceRemoteCache {
        /// Open (and create if necessary) the cache at `root`.
        pub fn open(root: PathBuf) -> Result<Self, std::io::Error> {
            fs::create_dir_all(root.join("blobs"))?;
            fs::create_dir_all(root.join("resolution"))?;
            Ok(Self { root })
        }

        /// Derive the default OS cache root for this application.
        ///
        /// On macOS: `~/Library/Caches/com.appverket.talos3d/libraries/remote/`.
        /// On Linux: `~/.cache/talos3d/libraries/remote/`.
        pub fn discover_default_cache_root() -> Result<PathBuf, anyhow::Error> {
            let strategy = etcetera::choose_app_strategy(AppStrategyArgs {
                top_level_domain: "com".to_owned(),
                author: "appverket".to_owned(),
                app_name: "talos3d".to_owned(),
            })
            .context("failed to determine OS cache directory")?;

            let root = strategy.cache_dir().join("libraries").join("remote");
            Ok(root)
        }

        fn blob_path(&self, content_hash_hex: &str) -> PathBuf {
            let prefix = &content_hash_hex[..2.min(content_hash_hex.len())];
            self.root
                .join("blobs")
                .join(prefix)
                .join(format!("{content_hash_hex}.bin"))
        }

        fn resolution_path(&self, kind: &str, canonical_id: &str) -> PathBuf {
            let encoded = canonical_id.replace('/', "%2F");
            self.root
                .join("resolution")
                .join(kind)
                .join(format!("{encoded}.json"))
        }
    }

    impl CatalogCache for WorkspaceRemoteCache {
        fn read_cursor(&self) -> i64 {
            let path = self.root.join("cursor");
            match fs::read_to_string(&path) {
                Ok(s) => s.trim().parse::<i64>().unwrap_or_else(|_| {
                    warn!(path = %path.display(), "cursor file is corrupt, resetting to 0");
                    0
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => {
                    warn!(error = %e, "failed to read cursor, resetting to 0");
                    0
                }
            }
        }

        fn write_cursor(&self, cursor: i64) -> Result<(), std::io::Error> {
            atomic_write(&self.root.join("cursor"), cursor.to_string().as_bytes())
        }

        fn read_blob(&self, content_hash_hex: &str) -> Option<Vec<u8>> {
            let path = self.blob_path(content_hash_hex);
            match fs::read(&path) {
                Ok(bytes) => {
                    debug!(hash = content_hash_hex, "cache hit");
                    Some(bytes)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    warn!(hash = content_hash_hex, error = %e, "failed to read blob from cache");
                    None
                }
            }
        }

        fn write_blob(
            &self,
            content_hash_hex: &str,
            bytes: &[u8],
        ) -> Result<(), std::io::Error> {
            let path = self.blob_path(content_hash_hex);
            if path.exists() {
                debug!(
                    hash = content_hash_hex,
                    "blob already cached, skipping write"
                );
                return Ok(());
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("tmp");
            fs::write(&tmp, bytes)?;
            fs::rename(&tmp, &path)?;
            Ok(())
        }

        fn read_resolution(
            &self,
            kind: &str,
            canonical_id: &str,
        ) -> Option<ResolutionPointer> {
            let path = self.resolution_path(kind, canonical_id);
            match fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| {
                        warn!(path = %path.display(), error = %e, "corrupt resolution pointer, ignoring");
                    })
                    .ok(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to read resolution pointer");
                    None
                }
            }
        }

        fn write_resolution(
            &self,
            kind: &str,
            canonical_id: &str,
            ptr: &ResolutionPointer,
        ) -> Result<(), std::io::Error> {
            let path = self.resolution_path(kind, canonical_id);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let json = serde_json::to_vec(ptr)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            atomic_write(&path, &json)
        }
    }

    fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use fs_impl::WorkspaceRemoteCache;

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- In-memory cache tests (run on all targets) -------------------------

    #[test]
    fn in_memory_cursor_round_trip() {
        let cache = InMemoryCatalogCache::new();
        assert_eq!(cache.read_cursor(), 0);
        cache.write_cursor(42).unwrap();
        assert_eq!(cache.read_cursor(), 42);
    }

    #[test]
    fn in_memory_blob_is_immutable() {
        let cache = InMemoryCatalogCache::new();
        let hash = "abc";
        cache.write_blob(hash, b"first").unwrap();
        cache.write_blob(hash, b"second").unwrap();
        assert_eq!(cache.read_blob(hash).unwrap(), b"first");
    }

    #[test]
    fn in_memory_resolution_round_trip() {
        let cache = InMemoryCatalogCache::new();
        let ptr = ResolutionPointer {
            revision: 7,
            content_hash: "deadbeef".into(),
            scope: "shipped".into(),
            kind: "material_def.v1".into(),
        };
        cache
            .write_resolution("material_def.v1", "com.example/wall", &ptr)
            .unwrap();
        let got = cache
            .read_resolution("material_def.v1", "com.example/wall")
            .unwrap();
        assert_eq!(got, ptr);
    }

    // ---- Filesystem cache tests (native only) -------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    mod fs_tests {
        use super::super::*;
        use std::fs;
        use tempfile::TempDir;

        fn make_cache() -> (TempDir, WorkspaceRemoteCache) {
            let dir = TempDir::new().unwrap();
            let cache = WorkspaceRemoteCache::open(dir.path().to_path_buf()).unwrap();
            (dir, cache)
        }

        #[test]
        fn workspace_cache_immutable_blobs() {
            let (_dir, cache) = make_cache();
            let hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

            let first = b"first payload";
            let second = b"second payload -- should NOT overwrite";

            cache.write_blob(hash, first).unwrap();
            cache.write_blob(hash, second).unwrap();

            let stored = cache.read_blob(hash).unwrap();
            assert_eq!(
                stored, first,
                "second write must not overwrite an immutable blob"
            );
        }

        #[test]
        fn workspace_cache_atomic_cursor_write() {
            let (dir, cache) = make_cache();
            let tmp_path = dir.path().join("cursor.tmp");
            fs::write(&tmp_path, b"garbage").unwrap();
            cache.write_cursor(42).unwrap();
            assert_eq!(cache.read_cursor(), 42);
            assert!(
                !tmp_path.exists(),
                ".tmp file should have been renamed away"
            );
        }

        #[test]
        fn cache_path_layout() {
            let (dir, cache) = make_cache();
            let hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
            cache.write_blob(hash, b"data").unwrap();
            let expected = dir
                .path()
                .join("blobs")
                .join("ab")
                .join(format!("{hash}.bin"));
            assert!(
                expected.exists(),
                "blob must be at blobs/<hash[0:2]>/<hash>.bin"
            );
        }

        #[test]
        fn resolution_canonical_id_with_slash_is_encoded() {
            let (dir, cache) = make_cache();
            let ptr = ResolutionPointer {
                revision: 1,
                content_hash: "ab".into(),
                scope: "shipped".into(),
                kind: "material_def.v1".into(),
            };
            cache
                .write_resolution("material_def.v1", "com.example/wall", &ptr)
                .unwrap();
            let path = dir
                .path()
                .join("resolution")
                .join("material_def.v1")
                .join("com.example%2Fwall.json");
            assert!(path.exists(), "encoded resolution file should exist");
        }

        #[test]
        fn cursor_defaults_to_zero_when_missing() {
            let (_dir, cache) = make_cache();
            assert_eq!(cache.read_cursor(), 0);
        }

        #[test]
        fn cursor_survives_corrupt_file() {
            let (dir, cache) = make_cache();
            fs::write(dir.path().join("cursor"), b"not a number").unwrap();
            assert_eq!(cache.read_cursor(), 0);
        }

        #[test]
        fn resolution_pointer_round_trip() {
            let (_dir, cache) = make_cache();
            let ptr = ResolutionPointer {
                revision: 3,
                content_hash: "deadbeef".to_owned(),
                scope: "shipped".to_owned(),
                kind: "material_def.v1".to_owned(),
            };
            cache
                .write_resolution("material_def.v1", "com.example/wall", &ptr)
                .unwrap();
            let loaded = cache
                .read_resolution("material_def.v1", "com.example/wall")
                .unwrap();
            assert_eq!(loaded, ptr);
        }

        #[test]
        fn discover_default_cache_root_returns_existing_or_creatable_path() {
            // Should not error; the directory may not yet exist.
            let root = WorkspaceRemoteCache::discover_default_cache_root().unwrap();
            assert!(root.ends_with("libraries/remote"));
        }
    }
}
