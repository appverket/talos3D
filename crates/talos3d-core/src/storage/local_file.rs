//! [`LocalFileArtifactStore`] — filesystem-backed [`ArtifactStore`] using
//! a CAS-style layout under the OS cache root. The default store; no
//! cloud connectivity required.
//!
//! On-disk layout:
//!
//! ```text
//! <root>/local-store/
//!   blobs/<2-char-hash-prefix>/<full-hash>.bin   # body bytes, content-addressed
//!   manifest/<kind>/<canonical_id-escaped>.json  # {revision, artifact_id, ...}
//!   events.log                                    # one JSON object per line
//!   cursor                                        # latest cursor (decimal i64)
//! ```
//!
//! Concurrency: a single in-process [`std::sync::Mutex`] serialises
//! writes. Inter-process locking is *not* attempted (the desktop binary
//! is single-instance per workspace).
//!
//! Subscription semantics: on `run_subscription` the store replays
//! `events.log` from `since_cursor` once, then idles waiting for
//! `shutdown`. There is no live update channel because no other actor
//! writes to a local-only store.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Mutex},
    thread,
    time::Duration,
};

use bevy::prelude::*;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::artifact_store::{
    ArtifactStore, ArtifactStoreError, ChangeEvent, PublishArtifactRequest, PutResolution,
    StoreEventCallback,
};

/// JSON record persisted alongside every artifact. Mirrors enough of
/// [`ChangeEvent`] to reconstruct a replay event without re-reading the
/// blob body.
#[derive(Debug, Serialize, Deserialize)]
struct ManifestEntry {
    cursor: i64,
    artifact_id: Uuid,
    canonical_id: String,
    kind: String,
    revision: i32,
    scope: String,
    jurisdiction: Vec<String>,
    content_hash: String,
    owner_org_id: Option<Uuid>,
    published_at: String, // RFC 3339 to avoid the chrono::DateTime dance here.
}

pub struct LocalFileArtifactStore {
    root: PathBuf,
    write_lock: Mutex<()>,
    description: String,
}

impl LocalFileArtifactStore {
    /// Open (or create) a local store rooted at `root`. The root and its
    /// subdirectories are created on first call.
    pub fn open(root: PathBuf) -> Result<Self, ArtifactStoreError> {
        fs::create_dir_all(&root).map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
        fs::create_dir_all(root.join("blobs")).map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
        fs::create_dir_all(root.join("manifest"))
            .map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
        // Touch events.log so subscription replay has a file to open.
        if !root.join("events.log").exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("events.log"))
                .map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
        }
        let description = format!("local-file://{}", root.display());
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
            description,
        })
    }

    /// Open at the canonical OS cache root.
    ///
    /// On macOS: `~/Library/Caches/com.appverket.talos3d/libraries/local-store/`.
    /// On Linux: `~/.cache/talos3d/libraries/local-store/`.
    ///
    /// (Sibling of the cloud-side `libraries/remote/` directory used by
    /// the catalog-client's `WorkspaceRemoteCache`.)
    pub fn open_default() -> Result<Self, ArtifactStoreError> {
        use etcetera::AppStrategy;
        let strategy = etcetera::choose_app_strategy(etcetera::AppStrategyArgs {
            top_level_domain: "com".to_owned(),
            author: "appverket".to_owned(),
            app_name: "talos3d".to_owned(),
        })
        .map_err(|e| ArtifactStoreError::Io(format!("cache dir: {e}")))?;
        let local_root = strategy.cache_dir().join("libraries").join("local-store");
        Self::open(local_root)
    }

    fn blob_path(&self, content_hash: &str) -> PathBuf {
        let prefix = &content_hash[..2.min(content_hash.len())];
        self.root.join("blobs").join(prefix).join(format!(
            "{}.bin",
            content_hash
        ))
    }

    fn manifest_path(&self, kind: &str, canonical_id: &str) -> PathBuf {
        let escaped = canonical_id.replace(['/', ':'], "%2F");
        self.root
            .join("manifest")
            .join(kind)
            .join(format!("{escaped}.json"))
    }

    fn events_log_path(&self) -> PathBuf {
        self.root.join("events.log")
    }

    fn cursor_path(&self) -> PathBuf {
        self.root.join("cursor")
    }

    /// Allocate the next monotonic cursor. Caller must hold `write_lock`.
    fn next_cursor(&self) -> Result<i64, ArtifactStoreError> {
        let current = self.read_cursor();
        let next = current.saturating_add(1);
        self.write_cursor(next)?;
        Ok(next)
    }

    fn read_existing_manifest(
        &self,
        kind: &str,
        canonical_id: &str,
    ) -> Option<ManifestEntry> {
        let path = self.manifest_path(kind, canonical_id);
        let bytes = fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ArtifactStoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
    Ok(())
}

fn content_hash_of(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);
    // Use 32-byte blake3 output rendered as hex. talos-catalog uses sha256
    // hex on the wire; the local store is free to use blake3 since its
    // hashes are only ever compared to themselves. The hex length
    // happens to match (64 chars) which keeps directory layouts
    // compatible.
    hex_encode(hasher.finalize().as_bytes())
}

fn hex_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(ALPHABET[(byte >> 4) as usize] as char);
        out.push(ALPHABET[(byte & 0xF) as usize] as char);
    }
    out
}

impl ArtifactStore for LocalFileArtifactStore {
    fn description(&self) -> &str {
        &self.description
    }

    fn put(&self, req: &PublishArtifactRequest) -> Result<PutResolution, ArtifactStoreError> {
        let body_bytes =
            serde_json::to_vec(&req.body).map_err(|e| ArtifactStoreError::Other(e.to_string()))?;
        let content_hash = content_hash_of(&body_bytes);

        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| ArtifactStoreError::Other(format!("local store lock poisoned: {e}")))?;

        // Write blob (idempotent — same hash means identical bytes).
        let blob_path = self.blob_path(&content_hash);
        if !blob_path.exists() {
            atomic_write(&blob_path, &body_bytes)?;
        }

        // Determine revision via existing manifest, if any.
        let existing = self.read_existing_manifest(&req.kind, &req.canonical_id);
        let (artifact_id, revision) = match existing {
            Some(prev) => {
                if prev.content_hash == content_hash {
                    // No-op republish — return prior resolution.
                    return Ok(PutResolution {
                        artifact_id: prev.artifact_id,
                        revision: prev.revision,
                        content_hash,
                    });
                }
                (prev.artifact_id, prev.revision + 1)
            }
            None => (Uuid::new_v4(), 1),
        };

        let cursor = self.next_cursor()?;
        let published_at = Utc::now().to_rfc3339();

        let entry = ManifestEntry {
            cursor,
            artifact_id,
            canonical_id: req.canonical_id.clone(),
            kind: req.kind.clone(),
            revision,
            scope: req.scope.clone(),
            jurisdiction: req.jurisdiction.clone(),
            content_hash: content_hash.clone(),
            owner_org_id: req.owner_org_id,
            published_at: published_at.clone(),
        };
        let entry_bytes =
            serde_json::to_vec_pretty(&entry).map_err(|e| ArtifactStoreError::Other(e.to_string()))?;
        atomic_write(&self.manifest_path(&req.kind, &req.canonical_id), &entry_bytes)?;

        // Append to events log.
        let log_line = serde_json::to_string(&entry)
            .map_err(|e| ArtifactStoreError::Other(e.to_string()))?;
        let mut log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.events_log_path())
            .map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
        writeln!(log_file, "{log_line}").map_err(|e| ArtifactStoreError::Io(e.to_string()))?;

        Ok(PutResolution {
            artifact_id,
            revision,
            content_hash,
        })
    }

    fn get_blob(&self, content_hash: &str) -> Result<Vec<u8>, ArtifactStoreError> {
        let path = self.blob_path(content_hash);
        fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ArtifactStoreError::Rejected(format!("blob not found: {content_hash}"))
            } else {
                ArtifactStoreError::Io(e.to_string())
            }
        })
    }

    fn run_subscription(
        &self,
        kinds: Vec<String>,
        since_cursor: i64,
        mut on_event: StoreEventCallback,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), ArtifactStoreError> {
        // Replay events.log once, then idle.
        let log_path = self.events_log_path();
        if log_path.exists() {
            let file = fs::File::open(&log_path)
                .map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = match line {
                    Ok(s) if s.trim().is_empty() => continue,
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "local-store events.log read error");
                        continue;
                    }
                };
                let entry: ManifestEntry = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "local-store events.log line is not JSON");
                        continue;
                    }
                };
                if entry.cursor <= since_cursor {
                    continue;
                }
                if !kinds.is_empty() && !kinds.iter().any(|k| k == &entry.kind) {
                    continue;
                }
                let published_at = entry
                    .published_at
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap_or_else(|_| Utc::now());
                let event = ChangeEvent {
                    cursor: entry.cursor,
                    op: "publish".to_owned(),
                    artifact_id: entry.artifact_id,
                    canonical_id: entry.canonical_id,
                    kind: entry.kind,
                    revision: entry.revision,
                    scope: entry.scope,
                    jurisdiction: entry.jurisdiction,
                    content_hash: entry.content_hash,
                    manifest_hash: None,
                    owner_org_id: entry.owner_org_id,
                    published_at,
                };
                on_event(event);
            }
        }

        // Idle until shutdown.
        while !shutdown.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }

    fn read_cursor(&self) -> i64 {
        match fs::read_to_string(self.cursor_path()) {
            Ok(s) => s.trim().parse::<i64>().unwrap_or(0),
            Err(_) => 0,
        }
    }

    fn write_cursor(&self, cursor: i64) -> Result<(), ArtifactStoreError> {
        atomic_write(&self.cursor_path(), cursor.to_string().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_req(kind: &str, canonical_id: &str, body: serde_json::Value) -> PublishArtifactRequest {
        PublishArtifactRequest {
            kind: kind.to_owned(),
            canonical_id: canonical_id.to_owned(),
            body,
            body_schema_rev: 1,
            scope: "shipped".to_owned(),
            trust: "published".to_owned(),
            jurisdiction: vec![],
            owner_org_id: None,
            dependencies: vec![],
            published_by: Uuid::new_v4(),
        }
    }

    #[test]
    fn put_writes_blob_and_manifest() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileArtifactStore::open(dir.path().to_path_buf()).unwrap();

        let req = make_req("definition.v1", "lib::frame", json!({"id": "frame"}));
        let res = store.put(&req).unwrap();
        assert_eq!(res.revision, 1);
        assert!(!res.content_hash.is_empty());

        // Blob file present.
        let blob = store.get_blob(&res.content_hash).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&blob).unwrap();
        assert_eq!(parsed["id"], "frame");
    }

    #[test]
    fn put_dedupes_identical_body() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileArtifactStore::open(dir.path().to_path_buf()).unwrap();

        let req = make_req("definition.v1", "lib::frame", json!({"id": "frame"}));
        let r1 = store.put(&req).unwrap();
        let r2 = store.put(&req).unwrap();
        assert_eq!(r1.revision, r2.revision);
        assert_eq!(r1.artifact_id, r2.artifact_id);
        assert_eq!(r1.content_hash, r2.content_hash);
        // Cursor should NOT advance for a dedup'd republish.
        assert_eq!(store.read_cursor(), 1);
    }

    #[test]
    fn put_bumps_revision_on_changed_body() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileArtifactStore::open(dir.path().to_path_buf()).unwrap();

        let r1 = store
            .put(&make_req("definition.v1", "lib::frame", json!({"v": 1})))
            .unwrap();
        let r2 = store
            .put(&make_req("definition.v1", "lib::frame", json!({"v": 2})))
            .unwrap();
        assert_eq!(r1.revision, 1);
        assert_eq!(r2.revision, 2);
        assert_eq!(r1.artifact_id, r2.artifact_id);
        assert_ne!(r1.content_hash, r2.content_hash);
        assert_eq!(store.read_cursor(), 2);
    }

    #[test]
    fn run_subscription_replays_events_log() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileArtifactStore::open(dir.path().to_path_buf()).unwrap();

        let _ = store
            .put(&make_req("definition.v1", "lib::a", json!({"id": "a"})))
            .unwrap();
        let _ = store
            .put(&make_req("material_def.v1", "lib::m", json!({"id": "m"})))
            .unwrap();

        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<ChangeEvent>::new()));
        let collected_for_cb = collected.clone();
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_for_thread = shutdown.clone();

        let store_handle = std::sync::Arc::new(store);
        let store_for_thread = store_handle.clone();
        let t = std::thread::spawn(move || {
            let cb: StoreEventCallback = Box::new(move |ev| {
                collected_for_cb.lock().unwrap().push(ev);
            });
            store_for_thread
                .run_subscription(vec![], 0, cb, shutdown_for_thread)
                .unwrap();
        });

        // Replay should be near-immediate; give it a moment then signal shutdown.
        std::thread::sleep(std::time::Duration::from_millis(50));
        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        t.join().unwrap();

        let events = collected.lock().unwrap();
        assert_eq!(events.len(), 2, "expected 2 replay events, got {events:#?}");
        assert_eq!(events[0].kind, "definition.v1");
        assert_eq!(events[1].kind, "material_def.v1");
    }
}
