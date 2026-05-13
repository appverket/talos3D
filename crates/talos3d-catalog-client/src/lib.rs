//! Async HTTP client for the `talos-catalog` knowledge backend service.
//!
//! # Overview
//!
//! This crate provides:
//!
//! - [`RemoteCatalogClient`] — a thin async HTTP client for the catalog REST API.
//! - [`WorkspaceRemoteCache`] — a content-addressed on-disk cache (blobs,
//!   resolution pointers, and a poll cursor).
//! - [`ChangePoller`] — a long-poll loop that delivers [`ChangeEvent`]s via an
//!   `mpsc` channel.
//!
//! # Wasm compatibility
//!
//! This crate is written for native targets. Two gaps prevent out-of-the-box
//! wasm compilation:
//!
//! 1. `reqwest` is configured with `rustls-tls`, which is native-only. Switch
//!    to `reqwest`'s `"wasm"` feature for browser targets.
//! 2. `etcetera::choose_app_strategy` (used by
//!    [`WorkspaceRemoteCache::discover_default_cache_root`]) has no wasm
//!    counterpart.
//!
//! Both gaps are deferred to PP-KBD-5 (web client). The Bevy plugin in
//! `talos3d-core` gates the entire `remote_catalog` module on
//! `#[cfg(not(target_arch = "wasm32"))]` so wasm builds compile out cleanly.

pub mod cache;
pub mod changes;
pub mod client;
pub mod dto;
pub mod error;
pub mod publish;

pub use cache::{ResolutionPointer, WorkspaceRemoteCache};
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
