//! [`ArtifactStore`] trait — the persistence substrate's public surface.
//!
//! Wire types ([`PublishArtifactRequest`], [`ArtifactResolution`],
//! [`ChangeEvent`]) live in [`super::wire`] and are owned by core. The
//! cloud-side HTTP client in
//! `appverket-infra/services/products/talos3d/talos3d-catalog-client/`
//! depends on this module rather than redefining them, so the wire is
//! a single canonical contract.

use std::sync::{atomic::AtomicBool, Arc};

use bevy::prelude::*;
use uuid::Uuid;

pub use super::wire::{ArtifactResolution, ChangeEvent, PublishArtifactRequest};

/// Minimal put-result surface the substrate cares about. Cloud stores
/// project this from [`ArtifactResolution`]; local stores synthesise it.
#[derive(Debug, Clone)]
pub struct PutResolution {
    pub artifact_id: Uuid,
    pub revision: i32,
    pub content_hash: String,
}

impl From<ArtifactResolution> for PutResolution {
    fn from(r: ArtifactResolution) -> Self {
        Self {
            artifact_id: r.artifact_id,
            revision: r.revision,
            content_hash: r.content_hash,
        }
    }
}

/// Errors a store may surface. Kept small and string-typed; impls add
/// detail via the inner message.
#[derive(Debug, Clone)]
pub enum ArtifactStoreError {
    Io(String),
    Rejected(String),
    Transport(String),
    Other(String),
}

impl std::fmt::Display for ArtifactStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "artifact store I/O error: {m}"),
            Self::Rejected(m) => write!(f, "artifact store rejected the request: {m}"),
            Self::Transport(m) => write!(f, "artifact store transport error: {m}"),
            Self::Other(m) => write!(f, "artifact store: {m}"),
        }
    }
}

impl std::error::Error for ArtifactStoreError {}

/// Compatibility alias — same shape as the catalog wire [`ChangeEvent`],
/// re-exported so callers don't need a direct dependency on the
/// catalog-client crate just to spell the type.
pub type StoreChangeEvent = ChangeEvent;

/// The signature used by `run_subscription` to deliver each event.
pub type StoreEventCallback = Box<dyn FnMut(StoreChangeEvent) + Send + 'static>;

/// Persistence substrate trait. Implementations:
///
/// - [`super::local_file::LocalFileArtifactStore`] writes a CAS-style
///   layout under the OS cache root and provides a one-shot replay
///   subscription (no live updates — there's no other writer in
///   local-only mode).
/// - [`super::cloud::CloudArtifactStore`] wraps the
///   `talos3d-catalog-client` HTTP client and the `ChangePoller`
///   long-poll loop. Will move to `appverket-infra/clients/` in
///   Phase 2 of the substrate work.
///
/// Sync API: implementations that need async I/O internally own their
/// own runtime and present a blocking facade. The substrate runs one
/// dedicated thread for publishes and one for subscription, so blocking
/// calls are safe.
pub trait ArtifactStore: Send + Sync + 'static {
    /// Human-readable identifier for logs.
    fn description(&self) -> &str;

    /// Publish an artifact. Blocking call from the substrate's
    /// publish-worker thread.
    fn put(&self, req: &PublishArtifactRequest) -> Result<PutResolution, ArtifactStoreError>;

    /// Fetch a blob body by content hash. Blocking.
    fn get_blob(&self, content_hash: &str) -> Result<Vec<u8>, ArtifactStoreError>;

    /// Run a long-lived subscription. Blocks the calling thread until
    /// `shutdown` flips to true. For each event the store calls
    /// `on_event`. `since_cursor` is the resume point persisted from a
    /// previous session.
    fn run_subscription(
        &self,
        kinds: Vec<String>,
        since_cursor: i64,
        on_event: StoreEventCallback,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(), ArtifactStoreError>;

    /// Read the persisted cursor. Returns 0 if no cursor has been
    /// recorded yet.
    fn read_cursor(&self) -> i64;

    /// Persist the cursor for the next session.
    fn write_cursor(&self, cursor: i64) -> Result<(), ArtifactStoreError>;
}

/// Resource holding the active store. The substrate plugin reads this at
/// `PostStartup` to bootstrap its worker threads.
///
/// Plugins that want to override the default (e.g. a future
/// `talos3d-catalog-plugin` that installs a `CloudArtifactStore`) must
/// insert this resource *before* the substrate plugin's `PostStartup`
/// runs.
#[derive(Resource, Clone)]
pub struct ActiveArtifactStore(pub Arc<dyn ArtifactStore>);

impl ActiveArtifactStore {
    pub fn new(store: Arc<dyn ArtifactStore>) -> Self {
        Self(store)
    }
}
