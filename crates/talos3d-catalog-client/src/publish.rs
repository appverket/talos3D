//! Domain-aware helpers for constructing [`PublishArtifactRequest`]s.
//!
//! Callers should use these helpers rather than constructing
//! [`PublishArtifactRequest`] by hand. The helpers pin the correct
//! `kind`, `body_schema_rev`, and `trust` values so that callers only
//! need to supply domain-meaningful arguments.

use uuid::Uuid;

use crate::dto::PublishArtifactRequest;

// ---- Error ------------------------------------------------------------------

/// Error returned by publish helpers when preconditions are violated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    /// [`PublishScope::Org`] requires `owner_org_id` to be `Some`.
    #[error("Org-scoped publish requires owner_org_id to be Some")]
    OrgScopeRequiresOwner,
}

// ---- Scope ------------------------------------------------------------------

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
    fn as_wire_str(self) -> &'static str {
        match self {
            PublishScope::Org => "org",
            PublishScope::Shipped => "shipped",
        }
    }
}

// ---- Helpers ----------------------------------------------------------------

/// Build a [`PublishArtifactRequest`] for a `material_def.v1` artifact.
///
/// # Errors
///
/// Returns [`PublishError::OrgScopeRequiresOwner`] when `scope` is
/// [`PublishScope::Org`] and `owner_org_id` is `None`.
pub fn material_def_publish_request(
    canonical_id: impl Into<String>,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owner_org_id: Option<Uuid>,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    build_request(
        "material_def.v1",
        canonical_id.into(),
        body,
        scope,
        jurisdiction,
        owner_org_id,
        published_by,
    )
}

/// Build a [`PublishArtifactRequest`] for a `definition.v1` artifact.
///
/// # Errors
///
/// Returns [`PublishError::OrgScopeRequiresOwner`] when `scope` is
/// [`PublishScope::Org`] and `owner_org_id` is `None`.
pub fn definition_publish_request(
    canonical_id: impl Into<String>,
    body: serde_json::Value,
    scope: PublishScope,
    jurisdiction: Vec<String>,
    owner_org_id: Option<Uuid>,
    published_by: Uuid,
) -> Result<PublishArtifactRequest, PublishError> {
    build_request(
        "definition.v1",
        canonical_id.into(),
        body,
        scope,
        jurisdiction,
        owner_org_id,
        published_by,
    )
}

// ---- Internal ---------------------------------------------------------------

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

    // Org-scoped artifacts must carry an owner; Shipped artifacts must not.
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

// ---- Unit tests -------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dummy_uuid() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    #[test]
    fn definition_publish_request_org_requires_owner() {
        let err = definition_publish_request(
            "com.example/def-1",
            json!({"id": "def-1"}),
            PublishScope::Org,
            vec![],
            None, // missing!
            dummy_uuid(),
        )
        .unwrap_err();

        assert_eq!(err, PublishError::OrgScopeRequiresOwner);
    }

    #[test]
    fn material_def_publish_request_shipped_clears_owner() {
        let org_id = Uuid::new_v4();
        let req = material_def_publish_request(
            "com.example/mat-1",
            json!({"id": "mat-1"}),
            PublishScope::Shipped,
            vec!["SE".into()],
            Some(org_id), // provided but must be cleared for Shipped
            dummy_uuid(),
        )
        .unwrap();

        assert_eq!(req.kind, "material_def.v1");
        assert_eq!(req.scope, "shipped");
        assert_eq!(req.trust, "published");
        assert_eq!(req.body_schema_rev, 1);
        // Shipped scope must not carry owner_org_id to the wire.
        assert_eq!(req.owner_org_id, None);
        assert_eq!(req.jurisdiction, vec!["SE".to_owned()]);
    }

    #[test]
    fn definition_publish_request_org_scope_sets_owner() {
        let org_id = Uuid::new_v4();
        let published_by = dummy_uuid();
        let req = definition_publish_request(
            "com.example/def-2",
            json!({"id": "def-2"}),
            PublishScope::Org,
            vec![],
            Some(org_id),
            published_by,
        )
        .unwrap();

        assert_eq!(req.kind, "definition.v1");
        assert_eq!(req.scope, "org");
        assert_eq!(req.owner_org_id, Some(org_id));
        assert_eq!(req.published_by, published_by);
    }
}
