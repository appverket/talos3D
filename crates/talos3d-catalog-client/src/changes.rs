//! Long-poll change poller for the talos-catalog service.
//!
//! [`ChangePoller`] runs a background async loop that fetches new
//! [`ChangeEvent`]s since the last known cursor, delivers them via an
//! [`mpsc`][tokio::sync::mpsc] channel, and sleeps when the feed is caught up.
//!
//! The cursor is persisted to the [`WorkspaceRemoteCache`] after each batch so
//! the poller resumes correctly after process restart or crash.

use std::{sync::Arc, time::Duration};

use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::{
    cache::WorkspaceRemoteCache, client::RemoteCatalogClient, dto::ChangeEvent,
    error::CatalogClientError,
};

/// Long-poll change poller.
///
/// Call [`ChangePoller::run`] to start the loop. It owns the loop lifecycle;
/// drop or send `true` on the shutdown watch to stop it cleanly.
pub struct ChangePoller {
    client: RemoteCatalogClient,
    cache: Arc<WorkspaceRemoteCache>,
    /// Artifact kinds to subscribe to (e.g. `["material_def.v1"]`).
    kinds: Vec<String>,
    /// How long to sleep when the feed is fully caught up.
    idle_interval: Duration,
}

impl ChangePoller {
    /// Create a new poller.
    pub fn new(
        client: RemoteCatalogClient,
        cache: Arc<WorkspaceRemoteCache>,
        kinds: Vec<String>,
        idle_interval: Duration,
    ) -> Self {
        Self {
            client,
            cache,
            kinds,
            idle_interval,
        }
    }

    /// Run the polling loop until `shutdown` becomes `true` or `tx` is closed.
    ///
    /// Events are delivered in cursor order. The cursor is persisted to the
    /// cache after each batch.
    ///
    /// Network errors (HTTP transport, 5xx status) are logged and retried after
    /// `idle_interval * 2`. Other unexpected errors propagate to the caller.
    pub async fn run(
        self,
        tx: mpsc::Sender<ChangeEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), CatalogClientError> {
        let kind_refs: Vec<&str> = self.kinds.iter().map(String::as_str).collect();
        let mut cursor = self.cache.read_cursor();
        info!(cursor, kinds = ?self.kinds, "catalog poller started");

        loop {
            // Check shutdown before each poll.
            if *shutdown.borrow() {
                info!("catalog poller received shutdown signal");
                return Ok(());
            }

            let result = self
                .client
                .list_changes(cursor, &kind_refs, Some(500))
                .await;

            match result {
                Ok(resp) => {
                    let count = resp.changes.len();
                    let next = resp.next_cursor;

                    for event in resp.changes {
                        // Pre-fetch the body for auto-fetch kinds before
                        // forwarding. This is done by the Bevy plugin layer
                        // instead, so here we simply deliver the raw events.
                        match tx.send(event).await {
                            Ok(()) => {}
                            Err(_) => {
                                // Receiver dropped — exit cleanly.
                                info!("catalog poller channel closed, exiting");
                                return Ok(());
                            }
                        }
                    }

                    if next > cursor {
                        // Persist cursor only when it actually advanced.
                        cursor = next;
                        if let Err(e) = self.cache.write_cursor(cursor) {
                            warn!(error = %e, "failed to persist cursor to cache");
                        }
                    }

                    if count > 0 && next > cursor - (count as i64) {
                        // Still catching up — loop immediately.
                        debug!(cursor, count, "catching up with feed");
                        continue;
                    }

                    // Fully caught up — wait before polling again.
                    tokio::select! {
                        _ = tokio::time::sleep(self.idle_interval) => {}
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() {
                                info!("catalog poller received shutdown signal during idle");
                                return Ok(());
                            }
                        }
                    }
                }

                Err(CatalogClientError::Http(ref e)) if is_transient_http(e) => {
                    warn!(error = %e, "transient HTTP error, retrying after backoff");
                    tokio::time::sleep(self.idle_interval * 2).await;
                }

                Err(CatalogClientError::Status { code, ref body }) if code >= 500 => {
                    warn!(code, body, "server error, retrying after backoff");
                    tokio::time::sleep(self.idle_interval * 2).await;
                }

                Err(other) => return Err(other),
            }
        }
    }
}

fn is_transient_http(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}
