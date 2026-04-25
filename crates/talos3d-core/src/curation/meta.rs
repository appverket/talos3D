//! `CurationMeta` — the governance header every curated kind embeds.
//!
//! Per ADR-040 the conceptual center of the substrate is not a
//! `CuratedAsset<T>` wrapper but `CurationMeta` plus the governance
//! surface around it. Domain types embed it as `meta: CurationMeta` and
//! stay recognizably themselves:
//!
//! ```ignore
//! RecipeArtifact { meta: CurationMeta, body: RecipeBody, ... }
//! MaterialSpec   { meta: CurationMeta, body: MaterialSpecBody }
//! ProductEntry   { meta: CurationMeta, body: ProductBody }
//! CodeRulePack   { meta: CurationMeta, body: CodeRuleBody }
//! ```

use serde::{Deserialize, Serialize};

use super::compatibility::CompatibilityRef;
use super::dependencies::DependencyRef;
use super::identity::{AssetId, AssetKindId, AssetRevision};
use super::pack::PackRef;
use super::provenance::Provenance;
use super::scope_trust::{Scope, Trust, ValidationStatus};

/// Governance metadata every curated asset carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CurationMeta {
    pub id: AssetId,
    pub kind: AssetKindId,
    pub revision: AssetRevision,
    pub scope: Scope,
    pub trust: Trust,
    pub validation: ValidationStatus,
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DependencyRef>,
    pub compatibility: CompatibilityRef,
    pub pack_membership: Option<PackRef>,
}

impl CurationMeta {
    /// Construct a fresh `Session`/`Draft`/`Unchecked` metadata header
    /// for a newly-authored asset. Callers fill in the provenance and
    /// evidence fields before save / publish.
    pub fn new(id: AssetId, kind: AssetKindId, provenance: Provenance) -> Self {
        Self {
            id,
            kind,
            revision: AssetRevision::initial(),
            scope: Scope::Session,
            trust: Trust::Draft,
            validation: ValidationStatus::Unchecked,
            provenance,
            dependencies: Vec::new(),
            compatibility: CompatibilityRef::unconstrained(),
            pack_membership: None,
        }
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self
    }

    pub fn with_compatibility(mut self, compatibility: CompatibilityRef) -> Self {
        self.compatibility = compatibility;
        self
    }

    pub fn with_pack(mut self, pack: PackRef) -> Self {
        self.pack_membership = Some(pack);
        self
    }

    pub fn add_dependency(mut self, dep: DependencyRef) -> Self {
        self.dependencies.push(dep);
        self
    }

    /// Shorthand: is this asset shipped + published?
    pub fn is_shipped_published(&self) -> bool {
        self.scope == Scope::Shipped && self.trust == Trust::Published
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::{
        compatibility::VersionReq,
        dependencies::{DependencyRef, DependencyRole},
        identity::{ContentHash, PackId, PackRevision},
        pack::PackRef,
        provenance::{Confidence, JurisdictionTag, Lineage, Provenance},
    };
    use crate::plugins::refinement::{AgentId, RecipeId};

    fn provenance_for_tests() -> Provenance {
        Provenance {
            author: AgentId("claude".into()),
            confidence: Confidence::Medium,
            lineage: Lineage::ViaRecipe(RecipeId("stair_straight_residential".into())),
            rationale: None,
            jurisdiction: Some(JurisdictionTag::new("SE")),
            catalog_dependencies: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn new_meta_is_session_draft_unchecked_with_initial_revision() {
        let meta = CurationMeta::new(
            AssetId::new("recipe.v1/stair_straight_residential"),
            AssetKindId::new("recipe.v1"),
            provenance_for_tests(),
        );
        assert_eq!(meta.scope, Scope::Session);
        assert_eq!(meta.trust, Trust::Draft);
        assert_eq!(meta.validation, ValidationStatus::Unchecked);
        assert_eq!(meta.revision.version, 1);
        assert!(meta.dependencies.is_empty());
        assert!(meta.pack_membership.is_none());
    }

    #[test]
    fn is_shipped_published_requires_both_axes() {
        let mut meta = CurationMeta::new(
            AssetId::new("recipe.v1/x"),
            AssetKindId::new("recipe.v1"),
            provenance_for_tests(),
        );
        assert!(!meta.is_shipped_published());
        meta = meta.with_scope(Scope::Shipped);
        assert!(!meta.is_shipped_published());
        meta = meta.with_trust(Trust::Published);
        assert!(meta.is_shipped_published());
    }

    #[test]
    fn builder_adds_dependencies_and_pack_and_compatibility() {
        let meta = CurationMeta::new(
            AssetId::new("recipe.v1/stair"),
            AssetKindId::new("recipe.v1"),
            provenance_for_tests(),
        )
        .with_scope(Scope::Shipped)
        .with_trust(Trust::Published)
        .with_compatibility(CompatibilityRef::for_core(VersionReq::new("^0.1")))
        .with_pack(PackRef::new(
            PackId::new("talos3d_architecture_se"),
            PackRevision::new("v1"),
        ))
        .add_dependency(DependencyRef::new(
            AssetKindId::new("material_spec.v1"),
            AssetId::new("material_spec.v1/timber_c24"),
            AssetRevision {
                version: 1,
                content_hash: Some(ContentHash::new("blake3:x")),
            },
        ))
        .add_dependency(
            DependencyRef::new(
                AssetKindId::new("source"),
                AssetId::new("boverket.bbr.8"),
                AssetRevision::initial(),
            )
            .with_role(DependencyRole::Citation),
        );

        assert!(meta.is_shipped_published());
        assert_eq!(meta.dependencies.len(), 2);
        assert!(meta.pack_membership.is_some());
        assert!(meta.compatibility.core_api.is_some());
    }

    #[test]
    fn curation_meta_round_trips() {
        let meta = CurationMeta::new(
            AssetId::new("recipe.v1/stair"),
            AssetKindId::new("recipe.v1"),
            provenance_for_tests(),
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft)
        .with_compatibility(CompatibilityRef::for_core(VersionReq::new("^0.1")));
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: CurationMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn empty_dependencies_are_elided_from_json() {
        let meta = CurationMeta::new(
            AssetId::new("recipe.v1/x"),
            AssetKindId::new("recipe.v1"),
            provenance_for_tests(),
        );
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("\"dependencies\""));
    }
}
