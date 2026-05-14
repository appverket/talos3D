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
    /// `"local"` | `"account_private"` | `"project"` | `"workspace"` |
    /// `"org"` | `"talos_global"` | `"shipped"`
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
    /// Account owner for account-private artifacts.
    #[serde(default)]
    pub owner_account_id: Option<Uuid>,
    /// Project owner for project-scoped artifacts.
    #[serde(default)]
    pub owner_project_id: Option<Uuid>,
    /// Workspace owner for workspace-scoped artifacts.
    #[serde(default)]
    pub owner_workspace_id: Option<Uuid>,
    /// Organization owner for org-scoped artifacts.
    #[serde(default)]
    pub owner_org_id: Option<Uuid>,
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
    /// `"local"` | `"account_private"` | `"project"` | `"workspace"` |
    /// `"org"` | `"talos_global"` | `"shipped"`
    pub scope: String,
    /// `"draft"` | `"published"`
    pub trust: String,
    /// ISO country codes. Empty vec = universal.
    #[serde(default)]
    pub jurisdiction: Vec<String>,
    /// Required when `scope == "account_private"`.
    #[serde(default)]
    pub owner_account_id: Option<Uuid>,
    /// Required when `scope == "project"`.
    #[serde(default)]
    pub owner_project_id: Option<Uuid>,
    /// Required when `scope == "workspace"`.
    #[serde(default)]
    pub owner_workspace_id: Option<Uuid>,
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
    /// `"local"` | `"account_private"` | `"project"` | `"workspace"` |
    /// `"org"` | `"talos_global"` | `"shipped"`
    pub scope: String,
    pub jurisdiction: Vec<String>,
    pub content_hash: String,
    pub manifest_hash: Option<String>,
    #[serde(default)]
    pub owner_account_id: Option<Uuid>,
    #[serde(default)]
    pub owner_project_id: Option<Uuid>,
    #[serde(default)]
    pub owner_workspace_id: Option<Uuid>,
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
    #[error("Account-private publish requires owner_account_id to be Some")]
    AccountPrivateScopeRequiresOwner,
    #[error("Project-scoped publish requires owner_project_id to be Some")]
    ProjectScopeRequiresOwner,
    #[error("Workspace-scoped publish requires owner_workspace_id to be Some")]
    WorkspaceScopeRequiresOwner,
    #[error("Org-scoped publish requires owner_org_id to be Some")]
    OrgScopeRequiresOwner,
}

/// Distribution scope for a published artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishScope {
    /// Local to one runtime; never leaves the local store unless explicitly
    /// republished under a backend-visible scope.
    Local,
    /// Visible only to one authenticated account. If no explicit account owner
    /// is supplied by a helper, `published_by` is used as the owner account.
    AccountPrivate,
    /// Visible to members who can access the owning project.
    Project,
    /// Visible to members who can access the owning workspace.
    Workspace,
    /// Visible only within the owning organisation. `owner_org_id` is
    /// mandatory.
    Org,
    /// Curated Talos3D-global artifact, visible to every account.
    TalosGlobal,
    /// Shipped with the product; visible to every account.
    Shipped,
}

impl PublishScope {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            PublishScope::Local => "local",
            PublishScope::AccountPrivate => "account_private",
            PublishScope::Project => "project",
            PublishScope::Workspace => "workspace",
            PublishScope::Org => "org",
            PublishScope::TalosGlobal => "talos_global",
            PublishScope::Shipped => "shipped",
        }
    }
}

/// Owner ids attached to a publish request.
///
/// Stores and backends must treat these as authorization inputs, not as display
/// metadata. Each scoped publish requires exactly the owner id relevant to its
/// scope; global scopes clear all owner ids.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublishOwners {
    pub account_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
}

impl PublishOwners {
    pub fn account(account_id: Uuid) -> Self {
        Self {
            account_id: Some(account_id),
            ..Default::default()
        }
    }

    pub fn project(project_id: Uuid) -> Self {
        Self {
            project_id: Some(project_id),
            ..Default::default()
        }
    }

    pub fn workspace(workspace_id: Uuid) -> Self {
        Self {
            workspace_id: Some(workspace_id),
            ..Default::default()
        }
    }

    pub fn org(org_id: Uuid) -> Self {
        Self {
            org_id: Some(org_id),
            ..Default::default()
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
    let owners = helper_owners(scope, owner_org_id, published_by);
    build_request(
        "material_def.v1",
        canonical_id,
        body,
        scope,
        jurisdiction,
        owners,
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
    let owners = helper_owners(scope, owner_org_id, published_by);
    build_request(
        "definition.v1",
        canonical_id,
        body,
        scope,
        jurisdiction,
        owners,
        published_by,
    )
}

/// Build a [`PublishArtifactRequest`] for an arbitrary artifact kind.
pub fn artifact_publish_request(
    kind: &'static str,
    canonical_id: String,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owners: PublishOwners,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    build_request(
        kind,
        canonical_id,
        body,
        scope,
        jurisdiction,
        owners,
        published_by,
    )
}

fn build_request(
    kind: &'static str,
    canonical_id: String,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owners: PublishOwners,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    let effective_owners = match scope {
        PublishScope::Local | PublishScope::TalosGlobal | PublishScope::Shipped => {
            PublishOwners::default()
        }
        PublishScope::AccountPrivate => PublishOwners {
            account_id: Some(
                owners
                    .account_id
                    .ok_or(PublishError::AccountPrivateScopeRequiresOwner)?,
            ),
            ..Default::default()
        },
        PublishScope::Project => PublishOwners {
            project_id: Some(
                owners
                    .project_id
                    .ok_or(PublishError::ProjectScopeRequiresOwner)?,
            ),
            ..Default::default()
        },
        PublishScope::Workspace => PublishOwners {
            workspace_id: Some(
                owners
                    .workspace_id
                    .ok_or(PublishError::WorkspaceScopeRequiresOwner)?,
            ),
            ..Default::default()
        },
        PublishScope::Org => PublishOwners {
            org_id: Some(owners.org_id.ok_or(PublishError::OrgScopeRequiresOwner)?),
            ..Default::default()
        },
    };
    Ok(PublishArtifactRequest {
        kind: kind.to_owned(),
        canonical_id,
        body,
        body_schema_rev: 1,
        scope: scope.as_wire_str().to_owned(),
        trust: "published".to_owned(),
        jurisdiction,
        owner_account_id: effective_owners.account_id,
        owner_project_id: effective_owners.project_id,
        owner_workspace_id: effective_owners.workspace_id,
        owner_org_id: effective_owners.org_id,
        dependencies: Vec::new(),
        published_by,
    })
}

fn helper_owners(
    scope: PublishScope,
    owner_org_id: Option<Uuid>,
    published_by: Uuid,
) -> PublishOwners {
    match scope {
        PublishScope::AccountPrivate => PublishOwners::account(published_by),
        PublishScope::Org => PublishOwners {
            org_id: owner_org_id,
            ..Default::default()
        },
        _ => PublishOwners::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn publish_scope_wire_strings_cover_adr_scope_lattice() {
        assert_eq!(PublishScope::Local.as_wire_str(), "local");
        assert_eq!(
            PublishScope::AccountPrivate.as_wire_str(),
            "account_private"
        );
        assert_eq!(PublishScope::Project.as_wire_str(), "project");
        assert_eq!(PublishScope::Workspace.as_wire_str(), "workspace");
        assert_eq!(PublishScope::Org.as_wire_str(), "org");
        assert_eq!(PublishScope::TalosGlobal.as_wire_str(), "talos_global");
        assert_eq!(PublishScope::Shipped.as_wire_str(), "shipped");
    }

    #[test]
    fn account_private_publish_requires_and_sets_account_owner() {
        let account_id = Uuid::new_v4();
        let req = artifact_publish_request(
            "recipe.v1",
            "private.recipe".to_owned(),
            json!({"id": "private.recipe"}),
            PublishScope::AccountPrivate,
            Vec::new(),
            PublishOwners::account(account_id),
            Uuid::new_v4(),
        )
        .unwrap();

        assert_eq!(req.scope, "account_private");
        assert_eq!(req.owner_account_id, Some(account_id));
        assert_eq!(req.owner_project_id, None);
        assert_eq!(req.owner_workspace_id, None);
        assert_eq!(req.owner_org_id, None);
    }

    #[test]
    fn account_private_helper_defaults_owner_to_publisher() {
        let published_by = Uuid::new_v4();
        let req = material_def_publish_request(
            "private.material".to_owned(),
            json!({"id": "private.material"}),
            PublishScope::AccountPrivate,
            Vec::new(),
            None,
            published_by,
        )
        .unwrap();

        assert_eq!(req.owner_account_id, Some(published_by));
    }

    #[test]
    fn scoped_publish_rejects_missing_required_owner() {
        let published_by = Uuid::new_v4();
        let body = json!({"id": "x"});

        let missing_account = artifact_publish_request(
            "definition.v1",
            "account.x".to_owned(),
            body.clone(),
            PublishScope::AccountPrivate,
            Vec::new(),
            PublishOwners::default(),
            published_by,
        );
        assert_eq!(
            missing_account.unwrap_err(),
            PublishError::AccountPrivateScopeRequiresOwner
        );

        let missing_project = artifact_publish_request(
            "definition.v1",
            "project.x".to_owned(),
            body.clone(),
            PublishScope::Project,
            Vec::new(),
            PublishOwners::default(),
            published_by,
        );
        assert_eq!(
            missing_project.unwrap_err(),
            PublishError::ProjectScopeRequiresOwner
        );

        let missing_workspace = artifact_publish_request(
            "definition.v1",
            "workspace.x".to_owned(),
            body.clone(),
            PublishScope::Workspace,
            Vec::new(),
            PublishOwners::default(),
            published_by,
        );
        assert_eq!(
            missing_workspace.unwrap_err(),
            PublishError::WorkspaceScopeRequiresOwner
        );

        let missing_org = artifact_publish_request(
            "definition.v1",
            "org.x".to_owned(),
            body,
            PublishScope::Org,
            Vec::new(),
            PublishOwners::default(),
            published_by,
        );
        assert_eq!(
            missing_org.unwrap_err(),
            PublishError::OrgScopeRequiresOwner
        );
    }

    #[test]
    fn global_scopes_clear_owner_metadata() {
        let owners = PublishOwners {
            account_id: Some(Uuid::new_v4()),
            project_id: Some(Uuid::new_v4()),
            workspace_id: Some(Uuid::new_v4()),
            org_id: Some(Uuid::new_v4()),
        };
        let req = artifact_publish_request(
            "source_passage.v1",
            "global.passage".to_owned(),
            json!({"id": "global.passage"}),
            PublishScope::TalosGlobal,
            Vec::new(),
            owners,
            Uuid::new_v4(),
        )
        .unwrap();

        assert_eq!(req.scope, "talos_global");
        assert_eq!(req.owner_account_id, None);
        assert_eq!(req.owner_project_id, None);
        assert_eq!(req.owner_workspace_id, None);
        assert_eq!(req.owner_org_id, None);
    }
}
