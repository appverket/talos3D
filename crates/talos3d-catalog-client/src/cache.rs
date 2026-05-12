//! Workspace-local content-addressed cache for remote catalog artifacts.
//!
//! Layout:
//! ```text
//! <cache-root>/
//!   blobs/<hash[0:2]>/<hash>.bin   (immutable — same hash = same bytes forever)
//!   resolution/<kind>/<canonical_id-url-encoded>.json   (mutable, atomic rename)
//!   cursor                          (mutable, atomic rename — last-polled cursor)
//! ```
//!
//! The cache root is computed by [`WorkspaceRemoteCache::discover_default_cache_root`]
//! or can be overridden by the caller.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use etcetera::{AppStrategy, AppStrategyArgs};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// A pinned pointer to a resolved artifact stored in the blob cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionPointer {
    pub revision: i32,
    /// Lowercase hex sha256 content hash.
    pub content_hash: String,
    pub scope: String,
    pub kind: String,
}

/// Content-addressed, workspace-local cache for remote catalog artifacts.
///
/// Cloning this struct is cheap — all paths are derived from the root.
#[derive(Debug, Clone)]
pub struct WorkspaceRemoteCache {
    root: PathBuf,
}

impl WorkspaceRemoteCache {
    /// Open (and create if necessary) the cache at `root`.
    ///
    /// Creates `blobs/`, `resolution/`, and the root directory itself.
    pub fn open(root: PathBuf) -> Result<Self, std::io::Error> {
        fs::create_dir_all(root.join("blobs"))?;
        fs::create_dir_all(root.join("resolution"))?;
        Ok(Self { root })
    }

    /// Return the last successfully written cursor, or `0` on missing/corrupt.
    pub fn read_cursor(&self) -> i64 {
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

    /// Persist `cursor` atomically via a temp-then-rename.
    pub fn write_cursor(&self, cursor: i64) -> Result<(), std::io::Error> {
        atomic_write(&self.root.join("cursor"), cursor.to_string().as_bytes())
    }

    /// Read a cached blob by its hex content hash.
    ///
    /// Returns `None` if the blob is not in cache (cache miss).
    pub fn read_blob(&self, content_hash_hex: &str) -> Option<Vec<u8>> {
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

    /// Write `bytes` to the blob cache for `content_hash_hex`.
    ///
    /// Blobs are **immutable**: if the file already exists, this is a no-op.
    /// Existing bytes are never overwritten because a given hash always
    /// corresponds to the same content.
    pub fn write_blob(&self, content_hash_hex: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
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
        // Write to a temp path then rename for atomicity.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Read the resolution pointer for `(kind, canonical_id)`.
    ///
    /// Returns `None` on cache miss or parse error.
    pub fn read_resolution(&self, kind: &str, canonical_id: &str) -> Option<ResolutionPointer> {
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

    /// Write the resolution pointer for `(kind, canonical_id)` atomically.
    pub fn write_resolution(
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

    /// Derive the default OS cache root for this application.
    ///
    /// On macOS: `~/Library/Caches/com.appverket.talos3d/libraries/remote/`
    /// On Linux: `~/.cache/talos3d/libraries/remote/`
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

    // ---- private helpers ----------------------------------------------------

    fn blob_path(&self, content_hash_hex: &str) -> PathBuf {
        let prefix = &content_hash_hex[..2.min(content_hash_hex.len())];
        self.root
            .join("blobs")
            .join(prefix)
            .join(format!("{content_hash_hex}.bin"))
    }

    fn resolution_path(&self, kind: &str, canonical_id: &str) -> PathBuf {
        // Encode slashes in canonical_id so it stays a single filename component.
        let encoded = canonical_id.replace('/', "%2F");
        self.root
            .join("resolution")
            .join(kind)
            .join(format!("{encoded}.json"))
    }
}

/// Write `bytes` to `path` atomically via temp-then-rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---- unit tests -------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
        cache.write_blob(hash, second).unwrap(); // no-op

        let stored = cache.read_blob(hash).unwrap();
        assert_eq!(
            stored, first,
            "second write must not overwrite an immutable blob"
        );
    }

    #[test]
    fn workspace_cache_atomic_cursor_write() {
        let (_dir, cache) = make_cache();

        // Place a stray .tmp file where the cursor would land.
        let tmp_path = cache.root.join("cursor.tmp");
        fs::write(&tmp_path, b"garbage").unwrap();

        // Write a real cursor — rename should overwrite the stray tmp.
        cache.write_cursor(42).unwrap();
        assert_eq!(cache.read_cursor(), 42);

        // The .tmp file should be gone (renamed to cursor).
        assert!(
            !tmp_path.exists(),
            ".tmp file should have been renamed away"
        );
    }

    #[test]
    fn cache_path_layout() {
        let (_dir, cache) = make_cache();
        let hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.write_blob(hash, b"data").unwrap();

        let expected = cache
            .root
            .join("blobs")
            .join("ab")
            .join(format!("{hash}.bin"));
        assert!(
            expected.exists(),
            "blob must be at blobs/<hash[0:2]>/<hash>.bin"
        );
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
            .expect("resolution pointer should be readable after write");

        assert_eq!(loaded.revision, ptr.revision);
        assert_eq!(loaded.content_hash, ptr.content_hash);
        assert_eq!(loaded.scope, ptr.scope);
        assert_eq!(loaded.kind, ptr.kind);
    }

    #[test]
    fn discover_default_cache_root_returns_existing_or_creatable_path() {
        // This test simply checks that etcetera resolves without panicking.
        // The resulting path need not exist yet.
        let root = WorkspaceRemoteCache::discover_default_cache_root()
            .expect("should derive a cache root from etcetera");
        // Path should contain the app name somewhere.
        let display = root.display().to_string();
        assert!(
            display.contains("talos3d") || display.contains("appverket"),
            "cache root should contain app identifier, got: {display}"
        );
    }

    #[test]
    fn cursor_defaults_to_zero_when_missing() {
        let (_dir, cache) = make_cache();
        assert_eq!(cache.read_cursor(), 0);
    }

    #[test]
    fn cursor_survives_corrupt_file() {
        let (_dir, cache) = make_cache();
        fs::write(cache.root.join("cursor"), b"not-a-number").unwrap();
        assert_eq!(cache.read_cursor(), 0, "corrupt cursor must fall back to 0");
    }

    #[test]
    fn resolution_canonical_id_with_slash_is_encoded() {
        let (_dir, cache) = make_cache();
        let ptr = ResolutionPointer {
            revision: 1,
            content_hash: "aabbcc".to_owned(),
            scope: "shipped".to_owned(),
            kind: "recipe.v1".to_owned(),
        };

        cache
            .write_resolution("recipe.v1", "light_frame/exterior_wall", &ptr)
            .unwrap();

        // The filename on disk should use %2F not /.
        let kind_dir = cache.root.join("resolution").join("recipe.v1");
        let entries: Vec<_> = fs::read_dir(kind_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().any(|n| n.contains("%2F")),
            "slash in canonical_id must be percent-encoded in filename, got: {entries:?}"
        );
    }
}
