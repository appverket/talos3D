//! Persistence substrate for curated artifacts.
//!
//! The [`ArtifactStore`] trait abstracts *where* serialized artifacts live —
//! a local filesystem checkpoint, a remote `talos-catalog` backend, an
//! in-memory mock, or anything else. The substrate plugin in
//! [`crate::plugins::remote_catalog`] is store-agnostic; pick the
//! implementation at app-init time.
//!
//! By default the desktop uses [`LocalFileArtifactStore`] (a CAS-style
//! directory under the OS cache root). A future `talos3d-catalog-plugin`
//! crate (hosted alongside the catalog service in `appverket-infra`)
//! provides the cloud-backed implementation; opting into "backend sync"
//! is the user's choice and is implemented by swapping the
//! [`ActiveArtifactStore`] resource.
//!
//! See `private/proposals/REALTIME_STORAGE_SUBSTRATE_CLAUDE.md` for the
//! design rationale.

#[cfg(not(target_arch = "wasm32"))]
pub mod artifact_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod local_file;
pub mod wire;

#[cfg(not(target_arch = "wasm32"))]
pub use artifact_store::{
    ActiveArtifactStore, ArtifactStore, ArtifactStoreError, PutResolution, StoreChangeEvent,
    StoreEventCallback,
};
#[cfg(not(target_arch = "wasm32"))]
pub use local_file::LocalFileArtifactStore;
pub use wire::{
    definition_publish_request, material_def_publish_request, ArtifactResolution, ChangeEvent,
    ChangesResponse, DependencyRefDto, ManifestResponse, PublishArtifactRequest, PublishError,
    PublishScope,
};
