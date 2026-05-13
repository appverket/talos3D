//! Async HTTP client for the `talos-catalog` knowledge backend service.
//!
//! # Overview
//!
//! This crate provides:
//!
//! - [`RemoteCatalogClient`] — a thin async HTTP client for the catalog REST API.
//! - [`CatalogCache`] trait — storage interface for cursor, blobs, and
//!   resolution pointers. Two implementations ship:
//!   - [`WorkspaceRemoteCache`] (native only) — filesystem-backed under a
//!     workspace or OS cache directory.
//!   - [`InMemoryCatalogCache`] (every target) — process memory only; loses
//!     state on drop. Useful for tests and as a wasm bootstrap.
//! - [`ChangePoller`] — a long-poll loop that delivers [`ChangeEvent`]s via an
//!   `mpsc` channel, parameterized over `Arc<dyn CatalogCache>`.
//!
//! # Target support
//!
//! Compiles for both native targets and `wasm32-unknown-unknown`. On wasm:
//!
//! - `reqwest` uses the browser's `fetch` API (no TLS feature compiled in).
//! - [`WorkspaceRemoteCache`] is compiled out — use [`InMemoryCatalogCache`].
//!   A future PP can add a `LocalStorageCatalogCache` or
//!   `IndexedDbCatalogCache` impl alongside the in-memory one.
//! - The polling loop uses `tokio::time::sleep` + `tokio::sync` types which
//!   are wasm-compatible; the caller drives it via
//!   `wasm_bindgen_futures::spawn_local` (rather than a `tokio::runtime`).

pub mod cache;
pub mod changes;
pub mod client;
pub mod dto;
pub mod error;
pub mod publish;

pub use cache::{CatalogCache, InMemoryCatalogCache, ResolutionPointer};
#[cfg(not(target_arch = "wasm32"))]
pub use cache::WorkspaceRemoteCache;
pub use changes::ChangePoller;
pub use client::RemoteCatalogClient;
pub use dto::{
    ArtifactResolution, ChangeEvent, ChangesResponse, DependencyRefDto, ManifestResponse,
    PublishArtifactRequest,
};
pub use error::CatalogClientError;
pub use publish::{
    definition_publish_request, material_def_publish_request, PublishError, PublishScope,
};
