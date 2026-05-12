//! Wire types for the talos-catalog HTTP API.
//!
//! These types mirror the server-side DTOs in the `talos-catalog` crate
//! (appverket-infra repo). They are intentionally duplicated rather than
//! shared via a common crate — the two repos have different dependency graphs
//! and the wire shape (JSON over HTTP) is the stability contract.
//!
//! Field names use snake_case. Enum string values use lower_snake_case
//! to match the catalog Postgres column values.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---- Shared sub-types -------------------------------------------------------

/// Cross-kind dependency reference. Mirrors `artifact_dependency` schema row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyRefDto {
    /// UUID of the artifact this record depends on.
    pub depends_on_artifact_id: Uuid,
    /// Dependency role: `"execution"` | `"validation"` | `"citation_completeness"`.
    pub role: String,
}

// ---- Resolution response ----------------------------------------------------

/// Full artifact record returned by `GET /v1/artifacts` and `POST /v1/artifacts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactResolution {
    pub artifact_id: Uuid,
    pub kind: String,
    pub canonical_id: String,
    pub revision: i32,
    /// `"session"` | `"project"` | `"org"` | `"shipped"`
    pub scope: String,
    /// `"draft"` | `"published"`
    pub trust: String,
    /// Lowercase hex sha256 of the canonical body bytes.
    pub content_hash: String,
    pub body: serde_json::Value,
    pub body_schema_rev: i32,
    pub jurisdiction: Vec<String>,
    /// Hex hash of the pack-release manifest that includes this artifact, if any.
    pub pack_release_manifest_hash: Option<String>,
    /// UUID of the artifact row this revision supersedes.
    pub supersedes: Option<Uuid>,
    /// Blob URL. For dev backend: relative `/v1/blobs/{hash}`.
    /// For prod (PP-KBD-3): signed R2 URL.
    pub blob_url: Option<String>,
}

// ---- Publish request --------------------------------------------------------

/// Request body for `POST /v1/artifacts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishArtifactRequest {
    pub kind: String,
    pub canonical_id: String,
    pub body: serde_json::Value,
    pub body_schema_rev: i32,
    /// `"org"` | `"shipped"`
    pub scope: String,
    /// `"draft"` | `"published"`
    pub trust: String,
    /// ISO country codes. Empty vec = universal.
    #[serde(default)]
    pub jurisdiction: Vec<String>,
    /// Required when `scope == "org"`.
    pub owner_org_id: Option<Uuid>,
    #[serde(default)]
    pub dependencies: Vec<DependencyRefDto>,
    /// Operator or account id. Trusted by the caller in PP-KBD-1; PP-KBD-6
    /// layers in Stytch session middleware.
    pub published_by: Uuid,
}

// ---- Manifest response ------------------------------------------------------

/// Response for `GET /v1/manifests/{hash}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestResponse {
    pub manifest_hash: String,
    pub manifest: serde_json::Value,
    /// Ed25519 signature bytes (hex). In dev backend this is an empty string.
    pub signature: String,
    /// ID of the signing key used. In dev backend this is a fixed UUID.
    pub signing_key_id: Uuid,
}

// ---- Change event -----------------------------------------------------------

/// A single entry in the change feed (both long-poll and SSE).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeEvent {
    /// Monotonically increasing cursor. Clients use this as `since` on
    /// reconnect.
    pub cursor: i64,
    /// `"publish"` | `"supersede"` | `"rollback"`
    pub op: String,
    pub artifact_id: Uuid,
    pub canonical_id: String,
    pub kind: String,
    pub revision: i32,
    /// `"session"` | `"project"` | `"org"` | `"shipped"`
    pub scope: String,
    pub jurisdiction: Vec<String>,
    pub content_hash: String,
    pub manifest_hash: Option<String>,
    pub owner_org_id: Option<Uuid>,
    pub published_at: DateTime<Utc>,
}

// ---- Long-poll response -----------------------------------------------------

/// Response for `GET /v1/changes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangesResponse {
    pub changes: Vec<ChangeEvent>,
    /// Use as `since` on the next poll call.
    pub next_cursor: i64,
}
