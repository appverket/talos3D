//! Shared DTOs for dynamically acquired knowledge assets.
//!
//! These types deliberately keep semantic scope (`Scope::Project`,
//! `Scope::Org`, etc.) separate from byte residency (`project_file`,
//! `.talos3d/knowledge`, cache). PP-DKC work uses them to make learned drafts
//! inspectable, persistable, and executable without introducing
//! `Scope::Workspace`.

use serde::{Deserialize, Serialize};

use crate::curation::{
    AssetId, AssetKindId, CurationMeta, EvidenceRef, JurisdictionTag, Provenance, Scope, Trust,
    ValidationStatus,
};
use crate::plugins::refinement::{AgentId, AuthoringMode};

pub const RECIPE_DRAFT_KIND: &str = "recipe_draft.v1";
pub const ASSEMBLY_PATTERN_DRAFT_KIND: &str = "assembly_pattern_draft.v1";
pub const PARAMETRIC_DRAFT_KIND: &str = "parametric_draft.v1";

/// Where the bytes for a knowledge asset currently live.
///
/// This is not a publication tier. A workspace-resident asset still carries
/// `Scope::Project` or `Scope::Org` in `CurationMeta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KnowledgeResidency {
    SessionCache,
    ProjectFile,
    WorkspaceKnowledge { relative_path: String },
}

impl Default for KnowledgeResidency {
    fn default() -> Self {
        Self::ProjectFile
    }
}

/// Evidence to be supplied for a claim before publication or gate promotion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct EvidenceSlot {
    pub claim_path: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<EvidenceRef>,
}

/// Evidence-backed runtime claim for executable learned assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RuntimeCapabilityClaim {
    pub claim_id: String,
    pub capability: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<String>,
}

impl RuntimeCapabilityClaim {
    pub fn verified_geometry_emission(
        evidence_refs: Vec<EvidenceRef>,
        last_verified: i64,
        verification_method: impl Into<String>,
    ) -> Self {
        Self {
            claim_id: "geometry_emission".into(),
            capability: "materializes_geometry".into(),
            evidence_refs,
            last_verified: Some(last_verified),
            verification_method: Some(verification_method.into()),
        }
    }

    pub fn is_evidence_backed(&self) -> bool {
        self.last_verified.is_some() && !self.evidence_refs.is_empty()
    }
}

pub fn draft_meta(
    asset_id: impl Into<String>,
    kind: &'static str,
    scope: Scope,
    jurisdiction: Option<&str>,
    gap_id: Option<&str>,
    rationale: Option<String>,
) -> CurationMeta {
    let mut provenance = Provenance::freeform(AgentId("agent".into()));
    provenance.lineage = AuthoringMode::Freeform;
    provenance.rationale = rationale
        .or_else(|| gap_id.map(|id| format!("Dynamically acquired to close corpus gap {id}.")));
    provenance.jurisdiction = jurisdiction.map(JurisdictionTag::new);

    CurationMeta::new(AssetId::new(asset_id), AssetKindId::new(kind), provenance)
        .with_scope(scope)
        .with_trust(Trust::Draft)
}

pub fn default_recipe_draft_meta() -> CurationMeta {
    draft_meta("", RECIPE_DRAFT_KIND, Scope::Session, None, None, None)
}

pub fn default_assembly_pattern_draft_meta() -> CurationMeta {
    draft_meta(
        "",
        ASSEMBLY_PATTERN_DRAFT_KIND,
        Scope::Session,
        None,
        None,
        None,
    )
}

pub fn default_parametric_draft_meta() -> CurationMeta {
    draft_meta("", PARAMETRIC_DRAFT_KIND, Scope::Session, None, None, None)
}

pub fn parse_scope(value: Option<&str>) -> Result<Scope, String> {
    match value.unwrap_or("project") {
        "session" => Ok(Scope::Session),
        "project" => Ok(Scope::Project),
        "org" => Ok(Scope::Org),
        "shipped" => Ok(Scope::Shipped),
        other => Err(format!(
            "unknown curation scope '{other}'; expected session, project, org, or shipped"
        )),
    }
}

pub fn scope_as_str(scope: Scope) -> &'static str {
    match scope {
        Scope::Session => "session",
        Scope::Project => "project",
        Scope::Org => "org",
        Scope::Shipped => "shipped",
    }
}

pub fn trust_as_str(trust: Trust) -> &'static str {
    match trust {
        Trust::Draft => "draft",
        Trust::Published => "published",
    }
}

pub fn validation_as_str(validation: &ValidationStatus) -> &'static str {
    match validation {
        ValidationStatus::Unchecked => "unchecked",
        ValidationStatus::Passing => "passing",
        ValidationStatus::Failing { .. } => "failing",
    }
}
