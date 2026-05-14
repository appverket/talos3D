//! [`CloudArtifactStore`] — `ArtifactStore` impl backed by the
//! `talos3d-catalog-client` HTTP client and the `ChangePoller` long-poll
//! loop.
//!
//! Lives in `talos3d-core` for now; in Phase 2 of the substrate work
//! this module (and the underlying catalog-client crate) move to
//! `appverket-infra/clients/talos3d-catalog-plugin/` so the core crate
//! stops shipping cloud-specific code.

use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use bevy::prelude::*;
use url::Url;
use uuid::Uuid;

use talos3d_catalog_client::{
    CatalogCache, ChangePoller, RemoteCatalogClient, WorkspaceRemoteCache,
};

use super::artifact_store::{
    ArtifactStore, ArtifactStoreError, PublishArtifactRequest, PutResolution, StoreEventCallback,
};

pub struct CloudArtifactStore {
    description: String,
    /// Shared multi-thread tokio runtime. Both the substrate's publish
    /// worker thread and its subscription thread `block_on` this runtime
    /// concurrently — multi-thread mode tolerates that.
    runtime: Arc<tokio::runtime::Runtime>,
    client: RemoteCatalogClient,
    cache: Arc<dyn CatalogCache>,
    poll_interval: Duration,
    #[allow(dead_code)]
    cache_root: PathBuf,
}

impl CloudArtifactStore {
    pub fn open(base_url: Url, account_id: Option<Uuid>) -> Result<Self, ArtifactStoreError> {
        let cache_root = WorkspaceRemoteCache::discover_default_cache_root()
            .map_err(|e| ArtifactStoreError::Io(e.to_string()))?;
        let cache: Arc<dyn CatalogCache> = Arc::new(
            WorkspaceRemoteCache::open(cache_root.clone())
                .map_err(|e| ArtifactStoreError::Io(e.to_string()))?,
        );
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("talos3d-cloud-store")
                .enable_all()
                .build()
                .map_err(|e| ArtifactStoreError::Other(e.to_string()))?,
        );
        let client = RemoteCatalogClient::new(base_url.clone(), account_id);
        let description = format!("cloud-catalog://{base_url}");
        Ok(Self {
            description,
            runtime,
            client,
            cache,
            poll_interval: Duration::from_secs(5),
            cache_root,
        })
    }
}

impl ArtifactStore for CloudArtifactStore {
    fn description(&self) -> &str {
        &self.description
    }

    fn put(&self, req: &PublishArtifactRequest) -> Result<PutResolution, ArtifactStoreError> {
        self.runtime
            .block_on(self.client.publish_artifact(req))
            .map(PutResolution::from)
            .map_err(|e| ArtifactStoreError::Transport(e.to_string()))
    }

    fn get_blob(&self, content_hash: &str) -> Result<Vec<u8>, ArtifactStoreError> {
        // Cache-then-network — same policy as the previous direct usage.
        if let Some(cached) = self.cache.read_blob(content_hash) {
            return Ok(cached);
        }
        let bytes = self
            .runtime
            .block_on(self.client.get_blob(content_hash))
            .map_err(|e| ArtifactStoreError::Transport(e.to_string()))?;
        let _ = self.cache.write_blob(content_hash, &bytes);
        Ok(bytes)
    }

    fn run_subscription(
        &self,
        kinds: Vec<String>,
        _since_cursor: i64,
        mut on_event: StoreEventCallback,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), ArtifactStoreError> {
        // We delegate to the existing `ChangePoller`, which reads its own
        // cursor from the workspace cache (via `CatalogCache::read_cursor`)
        // and ignores the substrate's `since_cursor` argument. This keeps
        // behaviour identical to the pre-refactor plugin.
        let client = self.client.clone();
        let cache = self.cache.clone();
        let interval = self.poll_interval;
        let kinds_for_poller = kinds.clone();

        self.runtime.block_on(async move {
            let (tx, mut rx) =
                tokio::sync::mpsc::channel::<talos3d_catalog_client::ChangeEvent>(256);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let auto_fetch_kinds = kinds.clone();
            let client_for_fetch = client.clone();
            let cache_for_fetch = cache.clone();

            // Bridge the poller's tokio channel into our blocking callback
            // and surface bodies via the cache (matching prior pre-fetch
            // behaviour for auto-fetch kinds).
            let bridge = tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    // Pre-fetch body opportunistically; we don't expose it
                    // through the callback (the substrate calls
                    // `get_blob` afterwards), but warming the cache means
                    // that fetch is local.
                    if auto_fetch_kinds.iter().any(|k| k == &event.kind) {
                        let hash = event.content_hash.clone();
                        if cache_for_fetch.read_blob(&hash).is_none() {
                            if let Ok(bytes) = client_for_fetch.get_blob(&hash).await {
                                let _ = cache_for_fetch.write_blob(&hash, &bytes);
                            }
                        }
                    }
                    on_event(event);
                }
            });

            // Poll the shutdown flag and forward to the watch channel.
            let shutdown_flag = shutdown.clone();
            let shutdown_pump = tokio::spawn(async move {
                loop {
                    if shutdown_flag.load(Ordering::Relaxed) {
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            });

            let poller = ChangePoller::new(client, cache, kinds_for_poller, interval);
            if let Err(e) = poller.run(tx, shutdown_rx).await {
                warn!(error = %e, "cloud-store change poller exited with error");
            }
            shutdown.store(true, Ordering::Relaxed);
            let _ = bridge.await;
            let _ = shutdown_pump.await;
            Ok::<(), ArtifactStoreError>(())
        })
    }

    fn read_cursor(&self) -> i64 {
        self.cache.read_cursor()
    }

    fn write_cursor(&self, cursor: i64) -> Result<(), ArtifactStoreError> {
        self.cache
            .write_cursor(cursor)
            .map_err(|e| ArtifactStoreError::Io(e.to_string()))
    }
}
