//! Provenance and evidence types for curated assets.
//!
//! Reuses the shipped [`crate::plugins::refinement`] vocabulary for claim
//! grounding wherever it is already fit for purpose:
//!
//! - [`AuthoringMode`](crate::plugins::refinement::AuthoringMode) is the
//!   `lineage` field on [`Provenance`]. It already carries
//!   `ViaRecipe` / `Freeform` / `Imported` / `Refined` — the four lineage
//!   kinds ADR-040 requires.
//! - [`Grounding`](crate::plugins::refinement::Grounding) is the
//!   `grounding_kind` field on [`EvidenceRef`]. Variants match the
//!   ADR-039 `Grounding` spec one-for-one, including the
//!   `GeneratedByRecipe` addition.
//! - [`ClaimPath`](crate::plugins::refinement::ClaimPath) addresses
//!   individual claims within an asset.
//!
//! What this module adds on top is the *reference* shape:
//! [`EvidenceRef`] resolves against a `SourceRegistry` entry by
//! `source_id + revision` (the registry itself lands in PP80), and
//! [`Provenance`] bundles lineage, confidence, jurisdiction, catalog
//! dependencies, and a list of evidence references into a single field
//! for `CurationMeta`.

use serde::{Deserialize, Serialize};

use crate::plugins::refinement::{AgentId, AuthoringMode, ClaimPath, Grounding};

use super::identity::{SourceId, SourceRevision};

// Re-export the shipped refinement-layer types that curation reuses, so
// consumers can pull everything from a single namespace.
pub use crate::plugins::refinement::{
    AgentId as AgentIdReExport, AuthoringMode as Lineage, CatalogRowId,
    ClaimPath as ClaimPathReExport, ClaimRecord, Grounding as GroundingKind, HeuristicTag,
    PassageRef, RecipeId, RuleId, SourceRef,
};

/// Confidence an author or reviewer has in a curated asset's contents.
///
/// `Certified` carries a specific operational meaning: combined with an
/// explicit reviewer signature it can override the default "no superseded
/// evidence" publication floor for one specific publication attempt.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    #[default]
    Low,
    Medium,
    High,
    Certified,
}

/// Jurisdiction tag (ISO-3166 alpha-2 recommended for country level; may
/// be extended with a subdivision suffix like `"SE-AB"` for Stockholm
/// county when needed).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct JurisdictionTag(pub String);

impl JurisdictionTag {
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JurisdictionTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reference to a row or a whole catalog provider. Used on
/// `Provenance.catalog_dependencies` so publication policies can surface
/// "this asset depends on dimensional lumber catalog X" cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CatalogRef {
    /// Catalog provider identifier (string to avoid a crate dependency on
    /// the catalog provider surface; resolved at lookup time).
    pub provider_id: String,
    /// Optional specific row — `None` when the dependency is the whole
    /// provider (e.g. "this recipe assumes *some* timber catalog is
    /// registered").
    pub row_id: Option<CatalogRowId>,
}

/// Opaque reference to a specific passage / excerpt inside a source. The
/// interpretation of the string is up to the source-registry entry's
/// metadata — for a PDF it might be `"p42:§8.22"`; for a web resource a
/// fragment identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ExcerptRef(pub String);

impl ExcerptRef {
    pub fn new(r: impl Into<String>) -> Self {
        Self(r.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A single evidence reference. Resolves against a `SourceRegistry` entry
/// by `source_id + revision`; an optional `claim_path` scopes the
/// evidence to a specific claim on the owning asset, and an optional
/// `excerpt_ref` pins to a specific passage inside the source.
///
/// `grounding_kind` is the shipped [`Grounding`] enum from
/// `refinement` (re-exported as [`GroundingKind`] for ADR-040 vocabulary
/// consistency).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct EvidenceRef {
    pub source_id: SourceId,
    pub revision: SourceRevision,
    pub claim_path: Option<ClaimPath>,
    pub excerpt_ref: Option<ExcerptRef>,
    pub grounding_kind: GroundingKind,
}

/// Provenance bundle for a curated asset. Lives on
/// `CurationMeta.provenance`.
///
/// Unifies the shipped `AuthoringProvenance { mode, rationale }` (→
/// `lineage` + `rationale` fields here) with evidence, confidence,
/// jurisdiction, and catalog dependencies. ADR-040 §Supersession Map
/// records the field-level mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Provenance {
    /// Agent that authored or last modified the asset.
    pub author: AgentId,
    pub confidence: Confidence,
    /// Lineage of the asset itself: which recipe / import / refinement
    /// produced it. Uses the shipped `AuthoringMode` from the refinement
    /// layer.
    pub lineage: Lineage,
    /// Free-form author note.
    pub rationale: Option<String>,
    /// Jurisdiction the asset applies to, when applicable.
    pub jurisdiction: Option<JurisdictionTag>,
    /// External catalog providers or rows this asset depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_dependencies: Vec<CatalogRef>,
    /// Evidence refs grounding the asset's claims in sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

impl Provenance {
    /// Minimal constructor for a freshly-authored session-scope asset.
    pub fn freeform(author: AgentId) -> Self {
        Self {
            author,
            confidence: Confidence::Low,
            lineage: AuthoringMode::Freeform,
            rationale: None,
            jurisdiction: None,
            catalog_dependencies: Vec::new(),
            evidence: Vec::new(),
        }
    }

    /// Returns `true` when every evidence ref resolves (in the sense of
    /// carrying a non-empty `source_id` + `revision`). Real resolution
    /// against a `SourceRegistry` happens in PP80; this check catches the
    /// degenerate case of uninitialized refs.
    pub fn evidence_is_populated(&self) -> bool {
        self.evidence
            .iter()
            .all(|e| !e.source_id.as_str().is_empty() && !e.revision.as_str().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
        assert!(Confidence::High < Confidence::Certified);
    }

    #[test]
    fn jurisdiction_tag_display_matches_inner() {
        let se = JurisdictionTag::new("SE");
        assert_eq!(se.to_string(), "SE");
    }

    #[test]
    fn catalog_ref_serializes_with_optional_row() {
        let whole = CatalogRef {
            provider_id: "dimensional_lumber_metric".into(),
            row_id: None,
        };
        let row = CatalogRef {
            provider_id: "dimensional_lumber_metric".into(),
            row_id: Some(CatalogRowId("2x6_spf_8ft".into())),
        };

        for c in [whole, row] {
            let json = serde_json::to_string(&c).unwrap();
            let parsed: CatalogRef = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, c);
        }
    }

    #[test]
    fn evidence_ref_round_trips() {
        let ev = EvidenceRef {
            source_id: SourceId::new("boverket.bbr.8"),
            revision: SourceRevision::new("2011:6"),
            claim_path: Some(ClaimPath("stair/riser_height_mm".into())),
            excerpt_ref: Some(ExcerptRef::new("§8:22")),
            grounding_kind: Grounding::ExplicitRule(RuleId("bbr_stair_riser_max".into())),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let parsed: EvidenceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ev);
    }

    #[test]
    fn provenance_freeform_has_empty_evidence() {
        let p = Provenance::freeform(AgentId("claude".into()));
        assert!(p.evidence.is_empty());
        assert!(p.catalog_dependencies.is_empty());
        assert!(matches!(p.lineage, AuthoringMode::Freeform));
        assert_eq!(p.confidence, Confidence::Low);
    }

    #[test]
    fn provenance_evidence_is_populated_detects_blanks() {
        let mut p = Provenance::freeform(AgentId("claude".into()));
        p.evidence.push(EvidenceRef {
            source_id: SourceId::new(""),
            revision: SourceRevision::new(""),
            claim_path: None,
            excerpt_ref: None,
            grounding_kind: Grounding::LLMHeuristic {
                rationale: "placeholder".into(),
                heuristic_tag: HeuristicTag("todo".into()),
            },
        });
        assert!(!p.evidence_is_populated());

        p.evidence[0].source_id = SourceId::new("boverket.bbr.8");
        p.evidence[0].revision = SourceRevision::new("2011:6");
        assert!(p.evidence_is_populated());
    }

    #[test]
    fn provenance_round_trips_through_json() {
        let p = Provenance {
            author: AgentId("codex".into()),
            confidence: Confidence::High,
            lineage: AuthoringMode::ViaRecipe(RecipeId("stair_straight_residential".into())),
            rationale: Some("default Swedish residential stair".into()),
            jurisdiction: Some(JurisdictionTag::new("SE")),
            catalog_dependencies: vec![CatalogRef {
                provider_id: "dimensional_lumber_metric".into(),
                row_id: None,
            }],
            evidence: vec![EvidenceRef {
                source_id: SourceId::new("boverket.bbr.8"),
                revision: SourceRevision::new("2011:6"),
                claim_path: Some(ClaimPath("stair/riser_height_mm".into())),
                excerpt_ref: Some(ExcerptRef::new("§8:22")),
                grounding_kind: Grounding::ExplicitRule(RuleId("bbr_stair_riser_max".into())),
            }],
        };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: Provenance = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, p);
    }
}
