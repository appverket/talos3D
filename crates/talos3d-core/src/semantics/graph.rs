//! Canonical authored records of the Design Concept Graph (ADR-064 §1).
//!
//! Everything in this module is *authored* data with evidence and scope. It is
//! the "canonical" half of ADR-064's recomputation test: anything derivable
//! from these records lives in [`SemanticRegistry`](super::registry::SemanticRegistry)
//! as a compiled index and is never authored twice.
//!
//! Provenance types are reused from [`crate::curation`] rather than redefined —
//! a second evidence type would be exactly the parallel-authority mistake
//! ADR-064 exists to prevent.

use serde::{Deserialize, Serialize};

use crate::capability_registry::ElementClassId;
use crate::curation::{EvidenceRef, JurisdictionTag};
use crate::plugins::refinement::RefinementState;

use super::ids::{AnchorKindId, AnchorRoleId, ConceptId, PredicateId, PropositionId};

/// Lifecycle of a concept. Deprecated concepts stay resolvable so old models
/// and old user vocabulary still bind; they are simply no longer preferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConceptStatus {
    #[default]
    Active,
    Deprecated {
        superseded_by: Option<ConceptId>,
    },
}

/// One locale's linguistic surface for a concept.
///
/// This is the layer whose absence forced `architectural_trim` to be minted in
/// Rust by cloning `trim` at load time (ADR-064 Context). Aliases are lexical
/// only: they never create a second concept identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct LexicalEntry {
    /// BCP-47-ish locale tag, e.g. `en-GB`, `sv-SE`.
    pub locale: String,
    pub preferred_label: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub deprecated_terms: Vec<String>,
    /// Terms that look like an alias in this locale but denote a *different*
    /// concept. Recording these is how regional aliases stay distinguishable
    /// from genuine regional variants.
    #[serde(default)]
    pub false_friends: Vec<String>,
    #[serde(default)]
    pub ambiguity_note: Option<String>,
}

impl LexicalEntry {
    /// Case-insensitive match of `term` against the preferred label and every
    /// alias. Deprecated terms match too — resolving an outdated word to the
    /// right concept is the point.
    pub fn matches(&self, term: &str) -> bool {
        let needle = term.trim().to_ascii_lowercase();
        std::iter::once(&self.preferred_label)
            .chain(&self.aliases)
            .chain(&self.deprecated_terms)
            .any(|candidate| candidate.trim().to_ascii_lowercase() == needle)
    }
}

/// A domain concept: what a thing *is*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Concept {
    pub id: ConceptId,
    #[serde(default)]
    pub status: ConceptStatus,
    #[serde(default)]
    pub lexical: Vec<LexicalEntry>,
    #[serde(default)]
    pub broader: Vec<ConceptId>,
    #[serde(default)]
    pub narrower: Vec<ConceptId>,
    /// Concepts this one is explicitly *not*. Drives reidentification repair
    /// hints ("did you mean corner_board?").
    #[serde(default)]
    pub contrasts: Vec<ConceptId>,
    /// Which building system this concept belongs to. Narrows anchor discovery
    /// and routes diagnostics; it never implies admissibility (ADR-064 §5).
    #[serde(default)]
    pub system_membership: Vec<ConceptId>,
    #[serde(default)]
    pub part_of: Vec<ConceptId>,
    /// Representation axis link. Authored, never derived.
    #[serde(default)]
    pub applicable_element_classes: Vec<ElementClassId>,
    #[serde(default)]
    pub regional_scope: Vec<JurisdictionTag>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
}

impl Concept {
    /// Minimal constructor for tests and asset builders.
    pub fn new(id: impl Into<ConceptId>) -> Self {
        Self {
            id: id.into(),
            status: ConceptStatus::Active,
            lexical: Vec::new(),
            broader: Vec::new(),
            narrower: Vec::new(),
            contrasts: Vec::new(),
            system_membership: Vec::new(),
            part_of: Vec::new(),
            applicable_element_classes: Vec::new(),
            regional_scope: Vec::new(),
            evidence: Vec::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, ConceptStatus::Active)
    }

    /// Whether any locale's lexical entry matches `term`.
    pub fn matches_term(&self, term: &str) -> bool {
        self.lexical.iter().any(|entry| entry.matches(term))
    }
}

/// Geometric nature of an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnchorGeometry {
    Point,
    Line,
    Plane,
    Path,
    Surface,
    Region,
}

/// How many instances of an anchor kind one host publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnchorCardinality {
    ExactlyOne,
    /// e.g. a gable roof publishes exactly two rake edges.
    Exactly(u32),
    ZeroOrMore,
}

/// A reusable, host-agnostic kind of attachment site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AnchorKindDescriptor {
    pub id: AnchorKindId,
    pub label: String,
    pub description: String,
    pub geometry: AnchorGeometry,
    pub cardinality: AnchorCardinality,
}

/// A concept's promise to publish an anchor kind.
///
/// Publication is canonical *here*, on the host. Consumers are named by
/// admissibility propositions, and the inverse `consumed_by` index is compiled
/// — never authored twice (ADR-064 §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PublishedAnchorContract {
    /// The concept that offers the anchor, e.g. `arch.concept.roof_system`.
    pub publisher: ConceptId,
    pub anchor_kind: AnchorKindId,
    /// Role discriminators this publisher realises, e.g. `north_west`.
    #[serde(default)]
    pub roles: Vec<AnchorRoleId>,
    /// Opaque reference to the evaluator that resolves the instance geometry.
    /// Core never executes it; the owning domain pack does.
    pub resolver: String,
    #[serde(default)]
    pub extent_rule: Option<String>,
}

/// What an admissibility proposition points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PropositionObject {
    /// The subject resolves against a published anchor of this kind. This is
    /// the whitelist form that makes invalid attachment unreferenceable.
    AnchorKind(AnchorKindId),
    /// The subject relates directly to another concept.
    Concept(ConceptId),
}

/// How many bindings the subject may or must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    ExactlyOne,
    AtMostOne,
    OneOrMore,
    ZeroOrMore,
}

/// A binding domain claim: *this concept relates this way to that anchor*.
///
/// `identity_defining` is the distinction that keeps refinement honest.
/// Refinement defers **missing detail**; it never authorises an **asserted
/// contradiction** (ADR-064 §3, agreement §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AdmissibilityProposition {
    pub id: PropositionId,
    pub subject: ConceptId,
    pub predicate: PredicateId,
    pub object: PropositionObject,
    pub cardinality: Cardinality,
    /// When true, violating this proposition means the caller chose the wrong
    /// concept. Refuses at every refinement state.
    #[serde(default)]
    pub identity_defining: bool,
    /// State at which an *absent* binding stops being deferrable. `None` means
    /// the binding is never required, only constrained when present.
    #[serde(default)]
    pub required_by: Option<RefinementState>,
    #[serde(default)]
    pub regional_scope: Vec<JurisdictionTag>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    /// Why a violation is wrong, in the user's language. Authored once, reused
    /// in every refusal.
    pub refusal_reason: String,
    /// What to do instead.
    pub repair_hint: String,
}

impl AdmissibilityProposition {
    /// Whether this proposition applies under `jurisdiction`.
    ///
    /// An empty `regional_scope` means "general" and applies everywhere. Note
    /// that corpus lint separately flags *binding* propositions authored
    /// without scope, so "general" must be a deliberate authoring choice.
    pub fn applies_in(&self, jurisdiction: Option<&JurisdictionTag>) -> bool {
        if self.regional_scope.is_empty() {
            return true;
        }
        jurisdiction.is_some_and(|tag| self.regional_scope.contains(tag))
    }

    /// Whether an absent binding must already be resolved at `state`.
    pub fn is_required_at(&self, state: RefinementState) -> bool {
        self.required_by.is_some_and(|required| state >= required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposition() -> AdmissibilityProposition {
        AdmissibilityProposition {
            id: PropositionId::new("p"),
            subject: ConceptId::new("c"),
            predicate: PredicateId::new("follows"),
            object: PropositionObject::AnchorKind(AnchorKindId::new("a")),
            cardinality: Cardinality::ExactlyOne,
            identity_defining: true,
            required_by: Some(RefinementState::Schematic),
            regional_scope: Vec::new(),
            evidence: Vec::new(),
            refusal_reason: "r".into(),
            repair_hint: "h".into(),
        }
    }

    #[test]
    fn lexical_entry_matches_label_alias_and_deprecated_term() {
        let entry = LexicalEntry {
            locale: "en-GB".into(),
            preferred_label: "bargeboard".into(),
            aliases: vec!["verge board".into()],
            deprecated_terms: vec!["gable board".into()],
            false_friends: vec!["corner board".into()],
            ambiguity_note: None,
        };
        assert!(entry.matches("Bargeboard"));
        assert!(entry.matches("  verge board "));
        assert!(entry.matches("gable board"));
        // A false friend is recorded, not matched: it denotes another concept.
        assert!(!entry.matches("corner board"));
    }

    #[test]
    fn unscoped_proposition_applies_everywhere() {
        let prop = proposition();
        assert!(prop.applies_in(None));
        assert!(prop.applies_in(Some(&JurisdictionTag::new("SE"))));
    }

    #[test]
    fn scoped_proposition_applies_only_in_scope() {
        let mut prop = proposition();
        prop.regional_scope = vec![JurisdictionTag::new("SE")];
        assert!(prop.applies_in(Some(&JurisdictionTag::new("SE"))));
        assert!(!prop.applies_in(Some(&JurisdictionTag::new("US"))));
        assert!(!prop.applies_in(None));
    }

    #[test]
    fn requirement_fires_at_and_above_the_declared_state() {
        let prop = proposition();
        assert!(!prop.is_required_at(RefinementState::Conceptual));
        assert!(prop.is_required_at(RefinementState::Schematic));
        assert!(prop.is_required_at(RefinementState::FabricationReady));
    }

    #[test]
    fn never_required_proposition_is_constraint_only() {
        let mut prop = proposition();
        prop.required_by = None;
        assert!(!prop.is_required_at(RefinementState::FabricationReady));
    }
}
