//! Wire types and publish-request helpers for the talos-catalog HTTP API.
//!
//! These types form the substrate's stability contract. They live in
//! `talos3d-core` (not in the catalog-client crate) because they're part
//! of the [`super::ArtifactStore`] abstraction — any store implementation
//! that ferries artifacts between processes uses them. The HTTP client in
//! `appverket-infra/services/products/talos3d/talos3d-catalog-client`
//! depends on this module rather than redefining its own.
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
    /// For prod: signed R2 URL.
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
    /// Operator or account id.
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

// ---- Publish helpers --------------------------------------------------------

/// Error returned by publish helpers when preconditions are violated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    #[error("Org-scoped publish requires owner_org_id to be Some")]
    OrgScopeRequiresOwner,
}

/// Distribution scope for a published artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishScope {
    /// Visible only within the owning organisation. `owner_org_id` is
    /// mandatory.
    Org,
    /// Shipped with the product; visible to every account.
    Shipped,
}

impl PublishScope {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            PublishScope::Org => "org",
            PublishScope::Shipped => "shipped",
        }
    }
}

/// Build a [`PublishArtifactRequest`] for a `material_def.v1` artifact.
pub fn material_def_publish_request(
    canonical_id: String,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owner_org_id: Option<Uuid>,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    build_request(
        "material_def.v1",
        canonical_id,
        body,
        scope,
        jurisdiction,
        owner_org_id,
        published_by,
    )
}

/// Build a [`PublishArtifactRequest`] for a `definition.v1` artifact.
pub fn definition_publish_request(
    canonical_id: String,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owner_org_id: Option<Uuid>,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    build_request(
        "definition.v1",
        canonical_id,
        body,
        scope,
        jurisdiction,
        owner_org_id,
        published_by,
    )
}

fn build_request(
    kind: &'static str,
    canonical_id: String,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owner_org_id: Option<Uuid>,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    if scope == PublishScope::Org && owner_org_id.is_none() {
        return Err(PublishError::OrgScopeRequiresOwner);
    }
    let effective_owner = match scope {
        PublishScope::Org => owner_org_id,
        PublishScope::Shipped => None,
    };
    Ok(PublishArtifactRequest {
        kind: kind.to_owned(),
        canonical_id,
        body,
        body_schema_rev: 1,
        scope: scope.as_wire_str().to_owned(),
        trust: "published".to_owned(),
        jurisdiction,
        owner_org_id: effective_owner,
        dependencies: Vec::new(),
        published_by,
    })
}
