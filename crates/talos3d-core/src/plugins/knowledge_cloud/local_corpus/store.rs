//! Content-addressed, verified on-disk corpus cache.
//!
//! Layout under `<root>/<authority>/`, where the authority directory is derived
//! from the corpus URL and trusted public keys so a different source or key
//! never reads another source's cache:
//!
//! ```text
//! indexes/assets.json   exact signed bytes of the last verified /assets index
//! indexes/skills.json   exact signed bytes of the last verified /skills index
//! blobs/<sha256>        exact revision bytes, named by their SHA-256
//! sync.json             unix time of the last successful sync (not trusted data)
//! ```
//!
//! The store never decides trust. Index bytes are re-verified against the
//! configured public key by the caller on every load; blob bytes are re-hashed
//! on every read. Writes are atomic (temporary file, fsync, rename).
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::plugins::agent_skills::AgentCorpusConnection;

/// Default upper bound for all cached blobs and indexes together.
pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Largest single index file the store will read back.
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;

/// A cached signed index, one per corpus endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexKind {
    /// Knowledge assets (`/assets`).
    Assets,
    /// Agent skills (`/skills`).
    Skills,
}

impl IndexKind {
    const fn file(self) -> &'static str {
        match self {
            Self::Assets => "assets.json",
            Self::Skills => "skills.json",
        }
    }
}

/// Failure of a local corpus store operation.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem failure.
    #[error("corpus cache I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A revision identifier is not a 64-character hexadecimal SHA-256.
    #[error("invalid revision hash")]
    InvalidHash,
    /// Bytes do not hash to the revision they were stored under or offered for.
    #[error("corpus cache checksum mismatch for {0}")]
    HashMismatch(String),
    /// Writing would exceed the size bound.
    #[error("corpus cache size bound of {max} bytes exceeded")]
    Full {
        /// The configured bound.
        max: u64,
    },
    /// The cache root is not an absolute, host-supplied directory.
    #[error("corpus cache directory must be absolute")]
    RelativeRoot,
}

/// Bookkeeping persisted beside the data. Informational only: it never confers
/// trust, it feeds the cache-age status.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncRecord {
    /// Unix seconds of the last sync that verified and persisted a full index.
    #[serde(default)]
    pub last_success_unix: Option<u64>,
}

/// One source's cache directory.
#[derive(Clone, Debug)]
pub struct LocalCorpusStore {
    dir: PathBuf,
    max_bytes: u64,
}

fn validate_hash(hash: &str) -> Result<(), StoreError> {
    if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StoreError::InvalidHash)
    }
}

/// Stable directory name for a corpus source and its trusted keys.
fn authority_dir(connection: &AgentCorpusConnection) -> String {
    let mut keys = connection.trusted_public_keys.clone();
    keys.sort();
    keys.dedup();
    let mut hasher = Sha256::new();
    hasher.update(connection.base_url.trim_end_matches('/').as_bytes());
    for key in &keys {
        hasher.update([0]);
        hasher.update(key.as_bytes());
    }
    hex::encode(&hasher.finalize()[..12])
}

impl LocalCorpusStore {
    /// Open (creating directories) the cache of `connection` under `root`.
    ///
    /// # Errors
    /// Fails when `root` is relative or its directories cannot be created.
    pub fn open(
        root: &Path,
        connection: &AgentCorpusConnection,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        if !root.is_absolute() {
            return Err(StoreError::RelativeRoot);
        }
        let dir = root.join(authority_dir(connection));
        fs::create_dir_all(dir.join("blobs"))?;
        fs::create_dir_all(dir.join("indexes"))?;
        Ok(Self { dir, max_bytes })
    }

    pub(crate) fn blob_path(&self, hash: &str) -> PathBuf {
        self.dir.join("blobs").join(hash.to_ascii_lowercase())
    }

    fn index_path(&self, kind: IndexKind) -> PathBuf {
        self.dir.join("indexes").join(kind.file())
    }

    /// Whether a blob file exists (it is only hash-checked when read).
    #[must_use]
    pub fn has_blob(&self, hash: &str) -> bool {
        validate_hash(hash).is_ok() && self.blob_path(hash).is_file()
    }

    /// Persist `bytes` under `hash` after checking they hash to it.
    ///
    /// # Errors
    /// Fails on a hash mismatch, I/O error, or when the size bound would be
    /// exceeded. Existing blobs are never overwritten.
    pub fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), StoreError> {
        validate_hash(hash)?;
        if !hex::encode(Sha256::digest(bytes)).eq_ignore_ascii_case(hash) {
            return Err(StoreError::HashMismatch(hash.to_owned()));
        }
        let path = self.blob_path(hash);
        if path.is_file() {
            return Ok(());
        }
        if self.size_bytes()? + bytes.len() as u64 > self.max_bytes {
            return Err(StoreError::Full {
                max: self.max_bytes,
            });
        }
        write_atomic(&path, bytes)
    }

    /// Read a blob, re-verifying its hash. A corrupted file is removed so the
    /// next sync downloads it again.
    ///
    /// # Errors
    /// `Ok(None)` when absent; `HashMismatch` when the stored bytes are corrupt.
    pub fn get_blob(&self, hash: &str) -> Result<Option<Vec<u8>>, StoreError> {
        validate_hash(hash)?;
        let path = self.blob_path(hash);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if hex::encode(Sha256::digest(&bytes)).eq_ignore_ascii_case(hash) {
            Ok(Some(bytes))
        } else {
            let _ = fs::remove_file(&path);
            Err(StoreError::HashMismatch(hash.to_owned()))
        }
    }

    /// Persist the exact signed bytes of a verified index.
    ///
    /// # Errors
    /// Fails on I/O error.
    pub fn write_index(&self, kind: IndexKind, bytes: &[u8]) -> Result<(), StoreError> {
        write_atomic(&self.index_path(kind), bytes)
    }

    /// The stored signed index bytes, or `None` when never cached. The caller
    /// must verify the signature before trusting them.
    ///
    /// # Errors
    /// Fails on I/O error or an oversized file.
    pub fn read_index(&self, kind: IndexKind) -> Result<Option<Vec<u8>>, StoreError> {
        let file = match fs::File::open(self.index_path(kind)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(StoreError::Full {
                max: MAX_INDEX_BYTES,
            });
        }
        Ok(Some(bytes))
    }

    /// Record a successful sync time.
    ///
    /// # Errors
    /// Fails on I/O error.
    pub fn write_sync_record(&self, record: &SyncRecord) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic(&self.dir.join("sync.json"), &bytes)
    }

    /// The last recorded sync time; absent or unreadable records read as default.
    #[must_use]
    pub fn read_sync_record(&self) -> SyncRecord {
        fs::read(self.dir.join("sync.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Total bytes of blobs and indexes.
    ///
    /// # Errors
    /// Fails on I/O error.
    pub fn size_bytes(&self) -> Result<u64, StoreError> {
        let mut total = 0;
        for sub in ["blobs", "indexes"] {
            for entry in fs::read_dir(self.dir.join(sub))? {
                total += entry?.metadata()?.len();
            }
        }
        Ok(total)
    }

    /// Delete every blob (and stray temporary file) not in `referenced`.
    /// Returns the number of removed files.
    ///
    /// # Errors
    /// Fails on I/O error.
    pub fn evict_unreferenced(&self, referenced: &BTreeSet<String>) -> Result<usize, StoreError> {
        let mut removed = 0;
        for entry in fs::read_dir(self.dir.join("blobs"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !referenced.contains(&name) {
                fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Hashes of all stored blobs.
    ///
    /// # Errors
    /// Fails on I/O error.
    pub fn blob_hashes(&self) -> Result<BTreeSet<String>, StoreError> {
        fs::read_dir(self.dir.join("blobs"))?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect()
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().ok_or(StoreError::RelativeRoot)?;
    let temporary = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent).and_then(|dir| dir.sync_all())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(StoreError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(url: &str) -> AgentCorpusConnection {
        AgentCorpusConnection {
            base_url: url.into(),
            bearer_token: None,
            trusted_public_keys: vec!["ab".repeat(32)],
        }
    }
    fn hash(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }
    fn open(root: &Path, url: &str, max: u64) -> LocalCorpusStore {
        LocalCorpusStore::open(root, &connection(url), max).expect("open store")
    }

    #[test]
    fn blobs_round_trip_and_reject_wrong_hash() {
        let root = tempfile::tempdir().unwrap();
        let store = open(root.path(), "https://a", 1 << 20);
        let bytes = b"revision";
        store.put_blob(&hash(bytes), bytes).unwrap();
        assert_eq!(store.get_blob(&hash(bytes)).unwrap().unwrap(), bytes);
        assert!(matches!(
            store.put_blob(&hash(b"other"), bytes),
            Err(StoreError::HashMismatch(_))
        ));
        assert!(matches!(
            store.put_blob("../escape", bytes),
            Err(StoreError::InvalidHash)
        ));
        assert!(store.get_blob(&hash(b"absent")).unwrap().is_none());
    }

    #[test]
    fn corrupted_blob_is_detected_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let store = open(root.path(), "https://a", 1 << 20);
        let h = hash(b"good");
        store.put_blob(&h, b"good").unwrap();
        fs::write(store.blob_path(&h), b"evil").unwrap();
        assert!(matches!(
            store.get_blob(&h),
            Err(StoreError::HashMismatch(_))
        ));
        assert!(!store.has_blob(&h));
    }

    #[test]
    fn eviction_keeps_only_referenced_blobs() {
        let root = tempfile::tempdir().unwrap();
        let store = open(root.path(), "https://a", 1 << 20);
        for bytes in [b"one".as_slice(), b"two", b"three"] {
            store.put_blob(&hash(bytes), bytes).unwrap();
        }
        let keep = BTreeSet::from([hash(b"two")]);
        assert_eq!(store.evict_unreferenced(&keep).unwrap(), 2);
        assert_eq!(store.blob_hashes().unwrap(), keep);
    }

    #[test]
    fn size_bound_is_enforced() {
        let root = tempfile::tempdir().unwrap();
        let store = open(root.path(), "https://a", 10);
        store.put_blob(&hash(b"12345678"), b"12345678").unwrap();
        assert!(matches!(
            store.put_blob(&hash(b"abcdefgh"), b"abcdefgh"),
            Err(StoreError::Full { .. })
        ));
    }

    #[test]
    fn sources_do_not_share_a_cache() {
        let root = tempfile::tempdir().unwrap();
        let first = open(root.path(), "https://a", 1 << 20);
        first.write_index(IndexKind::Assets, b"{}").unwrap();
        let other = open(root.path(), "https://b", 1 << 20);
        assert!(other.read_index(IndexKind::Assets).unwrap().is_none());
        assert!(first.read_index(IndexKind::Assets).unwrap().is_some());
    }

    #[test]
    fn relative_root_is_refused() {
        assert!(matches!(
            LocalCorpusStore::open(Path::new("rel"), &connection("https://a"), 1),
            Err(StoreError::RelativeRoot)
        ));
    }
}
