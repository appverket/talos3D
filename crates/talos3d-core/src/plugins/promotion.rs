//! Shared `PromotionPlan` boundary for **Make Reusable** flows.
//!
//! Per PP-A2DB-0 (ADR-047, `ASSEMBLY_TO_DEFINITION_BRIDGE_AGREEMENT.md`)
//! every Make Reusable flow lowers its source-specific work into a
//! `PromotionPlan`, and a single shared `PromotionDraftEmitter` consumes
//! plans regardless of source kind (selection / group / SemanticAssembly).
//! This module ships only the data shapes, traits, and the
//! validation/preservation invariants. Concrete source adapters and the
//! actual draft emission land in subsequent slices (PP-A2DB-1 onward and
//! PP-DPROMOTE-3).
//!
//! ElementId preservation is shared infrastructure, not SemanticAssembly-
//! specific: `validate_element_id_preservation` reports the three blocker
//! shapes named in the agreement so that selection, group, and assembly
//! promotion all get the same identity-stability guarantees when they
//! replace the source with the new Occurrence.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::plugins::{
    definition_authoring::DefinitionDraftId,
    identity::ElementId,
    modeling::definition::{ChildSlotDef, ParameterBinding, TransformBinding},
};

// === Source kind ============================================================

/// What kind of source produced this plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromotionSourceKind {
    Selection { element_ids: Vec<ElementId> },
    Group { group_id: ElementId },
    SemanticAssembly { assembly_id: ElementId },
}

impl PromotionSourceKind {
    pub fn source_element_ids(&self) -> Vec<ElementId> {
        match self {
            Self::Selection { element_ids } => element_ids.clone(),
            Self::Group { group_id } => vec![*group_id],
            Self::SemanticAssembly { assembly_id } => vec![*assembly_id],
        }
    }
}

// === Output shape ===========================================================

/// Whether the emitted Definition is a leaf or a compound with explicit
/// child slots.
///
/// `ChildSlotDef` does not derive `Eq`, so this enum cannot either; tests
/// inspect variants via `matches!` rather than equality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromotionOutputShape {
    Leaf,
    Compound { child_slots: Vec<ChildSlotDef> },
}

// === ElementId preservation ================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ElementIdPreservationMode {
    /// Preserve incoming `ElementId`s for child-slot realizations whenever
    /// the conflict policy allows. The default for replace-source flows.
    #[default]
    PreserveWherePossible,
    /// Skip preservation entirely; new `ElementId`s are minted.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ElementIdConflictPolicy {
    /// On any of the three documented conflict shapes, refuse to silently
    /// rewrite ids and surface a blocker. The default and the agreed
    /// behaviour.
    #[default]
    PreserveOrReportBlocker,
    /// Drop preservation for the conflicting items and continue with new
    /// ids. Reserved for explicit opt-in flows.
    DropPreservation,
}

/// The shared replace-source identity plan. Source adapters declare which
/// existing `ElementId`s should map to which child-slot realizations on
/// emission; the emitter enforces the policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElementIdPreservationPlan {
    pub mode: ElementIdPreservationMode,
    /// Pairs of (source `ElementId`, target child-slot id). Slot ids are
    /// stable Definition-local addresses (per `ChildSlotDef.slot_id`).
    pub source_element_to_slot_realization: Vec<(ElementId, String)>,
    pub conflict_policy: ElementIdConflictPolicy,
}

// === Source replacement =====================================================

/// Whether and how the source is replaced after emission.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SourceReplacementPolicy {
    #[default]
    NoReplacement,
    /// Spawn an Occurrence of the new Definition where the source lived.
    /// `preserve_assembly_wrapper` controls whether a SemanticAssembly
    /// source survives as a retargeted project-intent wrapper around the
    /// new Occurrence.
    ReplaceWithOccurrence { preserve_assembly_wrapper: bool },
}

// === Validation requirements ===============================================

/// Pre-emission validation gates the plan must satisfy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromotionValidationRequirements {
    pub require_unique_slot_ids: bool,
    pub require_capability_descriptor: Option<String>,
    pub blocking_findings_must_be_zero: bool,
}

// === Provenance payload =====================================================

/// AuthoringScript / recipe / agent attribution carried through emission
/// so the resulting `DefinitionDraft` records who and what produced it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromotionProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoring_script_payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

// === The plan itself ========================================================

/// A source-agnostic description of one Make Reusable invocation. Source
/// adapters produce these; `PromotionDraftEmitter` consumes them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionPlan {
    pub source_kind: PromotionSourceKind,
    pub draft_id: DefinitionDraftId,
    pub output_shape: PromotionOutputShape,
    #[serde(default)]
    pub parameter_exposure_requests: Vec<ParameterBinding>,
    #[serde(default)]
    pub transform_binding_requests: Vec<TransformBinding>,
    #[serde(default)]
    pub element_id_preservation: ElementIdPreservationPlan,
    #[serde(default)]
    pub source_replacement: SourceReplacementPolicy,
    #[serde(default)]
    pub validation: PromotionValidationRequirements,
    #[serde(default)]
    pub provenance: PromotionProvenance,
}

impl PromotionPlan {
    /// Convenience constructor for a leaf plan with default policies.
    pub fn new_leaf(source_kind: PromotionSourceKind) -> Self {
        Self {
            source_kind,
            draft_id: DefinitionDraftId::new(),
            output_shape: PromotionOutputShape::Leaf,
            parameter_exposure_requests: Vec::new(),
            transform_binding_requests: Vec::new(),
            element_id_preservation: ElementIdPreservationPlan::default(),
            source_replacement: SourceReplacementPolicy::default(),
            validation: PromotionValidationRequirements::default(),
            provenance: PromotionProvenance::default(),
        }
    }

    pub fn declared_slot_ids(&self) -> Vec<&str> {
        match &self.output_shape {
            PromotionOutputShape::Leaf => Vec::new(),
            PromotionOutputShape::Compound { child_slots } => {
                child_slots.iter().map(|s| s.slot_id.as_str()).collect()
            }
        }
    }
}

// === Source adapter trait ==================================================

/// Boundary that lifts a source-specific selection (a click selection, a
/// `Group` element, a `SemanticAssembly`) into a uniform `PromotionPlan`.
pub trait PromotionSourceAdapter {
    type SourceInput;
    type Error: std::error::Error + Send + Sync + 'static;

    fn build_plan(&self, source: Self::SourceInput) -> Result<PromotionPlan, Self::Error>;
}

// === Emission ==============================================================

/// The three blocker shapes the agreement requires the emitter to surface
/// rather than silently rewrite identities for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ElementIdBlocker {
    /// Two source elements would map to the same realization slot.
    DuplicateSourceMapsToSameRealization {
        realization_slot_id: String,
        sources: Vec<ElementId>,
    },
    /// The target id is already occupied by an entity outside the
    /// replacement set.
    TargetIdOccupiedOutsideReplacementSet {
        target: ElementId,
        slot_id: String,
    },
    /// The realization has no stable Definition-local slot address (e.g.
    /// the slot id is empty or the slot is not declared in the plan).
    RealizationLacksStableSlotAddress { source: ElementId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromotionEmissionError {
    ElementIdPreservation(Vec<ElementIdBlocker>),
    MissingCapabilityDescriptor(String),
    DuplicateSlotId(String),
}

impl std::fmt::Display for PromotionEmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ElementIdPreservation(b) => {
                write!(f, "element-id preservation conflict ({} blocker(s))", b.len())
            }
            Self::MissingCapabilityDescriptor(name) => {
                write!(f, "validation: required capability descriptor missing: {name}")
            }
            Self::DuplicateSlotId(id) => {
                write!(f, "validation: duplicate slot id `{id}`")
            }
        }
    }
}

impl std::error::Error for PromotionEmissionError {}

/// Identity map and any blockers produced by the emitter for one plan.
/// Consumers (notably `SemanticGraphMigrationDiff` in PP-A2DB-1) use the
/// identity map to retarget external references after replacement.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromotionEmissionRecord {
    pub draft_id: DefinitionDraftId,
    /// Source element id → realization slot id, for the entries the
    /// emitter actually preserved.
    pub identity_map: Vec<(ElementId, String)>,
    /// Blockers surfaced by the preservation policy. Empty on a clean
    /// emission.
    pub blockers: Vec<ElementIdBlocker>,
}

/// Boundary that consumes a `PromotionPlan` and produces the emitted
/// `DefinitionDraft` plus the identity / blocker record. Implementations
/// land in PP-A2DB-1 (selection adapter wiring) and PP-DPROMOTE-3
/// (compound emission).
pub trait PromotionDraftEmitter {
    fn emit(
        &mut self,
        plan: PromotionPlan,
    ) -> Result<PromotionEmissionRecord, PromotionEmissionError>;
}

// === Pre-emission invariants ==============================================

/// Validate the plan against its declared `validation` gates and slot-id
/// uniqueness. Returns the slot-id duplication or capability-descriptor
/// errors as `PromotionEmissionError` so the same shape is used at every
/// emission site.
pub fn validate_plan(plan: &PromotionPlan) -> Result<(), PromotionEmissionError> {
    if plan.validation.require_unique_slot_ids {
        let mut seen: HashSet<&str> = HashSet::new();
        for slot in plan.declared_slot_ids() {
            if !seen.insert(slot) {
                return Err(PromotionEmissionError::DuplicateSlotId(slot.to_string()));
            }
        }
    }
    if let Some(required) = &plan.validation.require_capability_descriptor {
        if plan
            .provenance
            .source_recipe_id
            .as_deref()
            .filter(|id| id == &required.as_str())
            .is_none()
        {
            return Err(PromotionEmissionError::MissingCapabilityDescriptor(
                required.clone(),
            ));
        }
    }
    Ok(())
}

/// Walk the plan's `ElementIdPreservationPlan` against the declared slot
/// ids and the existing-id occupancy set, and report all blockers per the
/// agreement.
///
/// `existing_ids_outside_replacement_set` should contain every live
/// `ElementId` that is NOT being replaced by this plan; the typical
/// `Selection`/`Group`/`SemanticAssembly` adapter computes it as
/// `world_ids \ source_ids`.
pub fn validate_element_id_preservation(
    plan: &PromotionPlan,
    existing_ids_outside_replacement_set: &HashSet<ElementId>,
) -> Vec<ElementIdBlocker> {
    if plan.element_id_preservation.mode == ElementIdPreservationMode::None {
        return Vec::new();
    }

    let declared_slots: HashSet<&str> = plan.declared_slot_ids().into_iter().collect();
    let mut blockers = Vec::new();
    let mut by_slot: HashMap<&str, Vec<ElementId>> = HashMap::new();

    for (source, slot) in &plan.element_id_preservation.source_element_to_slot_realization {
        // Stable-slot-address blocker: empty slot id or unknown to the plan
        // when the plan is compound; for leaf plans the slot must be empty.
        match &plan.output_shape {
            PromotionOutputShape::Leaf => {
                if !slot.is_empty() {
                    blockers.push(ElementIdBlocker::RealizationLacksStableSlotAddress {
                        source: *source,
                    });
                    continue;
                }
            }
            PromotionOutputShape::Compound { .. } => {
                if slot.is_empty() || !declared_slots.contains(slot.as_str()) {
                    blockers.push(ElementIdBlocker::RealizationLacksStableSlotAddress {
                        source: *source,
                    });
                    continue;
                }
            }
        }

        // Target-id-occupied blocker (the source id is preserved as the
        // realization id; if it's already live elsewhere, that's a clash).
        if existing_ids_outside_replacement_set.contains(source) {
            blockers.push(ElementIdBlocker::TargetIdOccupiedOutsideReplacementSet {
                target: *source,
                slot_id: slot.clone(),
            });
            continue;
        }

        by_slot.entry(slot.as_str()).or_default().push(*source);
    }

    // Duplicate-source-to-same-realization blocker: only meaningful for
    // compound plans (leaf has the empty-slot bucket which holds at most
    // one preserved id by definition; multiples there are degenerate but
    // already filtered as RealizationLacksStableSlotAddress above for
    // non-empty slots).
    for (slot, sources) in by_slot {
        if sources.len() > 1 {
            blockers.push(ElementIdBlocker::DuplicateSourceMapsToSameRealization {
                realization_slot_id: slot.to_string(),
                sources,
            });
        }
    }

    blockers
}

// === Tests =================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::modeling::definition::DefinitionId;

    fn elem(n: u64) -> ElementId {
        ElementId(n)
    }

    fn child_slot(slot_id: &str) -> ChildSlotDef {
        ChildSlotDef {
            slot_id: slot_id.to_string(),
            role: "test_role".to_string(),
            definition_id: DefinitionId("test.def".to_string()),
            parameter_bindings: Vec::new(),
            transform_binding: TransformBinding::default(),
            suppression_expr: None,
        }
    }

    #[test]
    fn leaf_plan_round_trips_through_serde() {
        let plan = PromotionPlan::new_leaf(PromotionSourceKind::Selection {
            element_ids: vec![elem(1), elem(2)],
        });
        let json = serde_json::to_string(&plan).unwrap();
        let back: PromotionPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan.source_kind, back.source_kind);
        assert!(matches!(back.output_shape, PromotionOutputShape::Leaf));
        assert_eq!(back.declared_slot_ids(), Vec::<&str>::new());
    }

    #[test]
    fn compound_plan_exposes_declared_slot_ids() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(7),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("front"), child_slot("back")],
        };
        let ids = plan.declared_slot_ids();
        assert_eq!(ids, vec!["front", "back"]);
    }

    #[test]
    fn validate_plan_flags_duplicate_slot_ids_when_required() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(1),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("dup"), child_slot("dup")],
        };
        plan.validation.require_unique_slot_ids = true;
        let err = validate_plan(&plan).unwrap_err();
        assert_eq!(err, PromotionEmissionError::DuplicateSlotId("dup".into()));
    }

    #[test]
    fn validate_plan_passes_when_unique_slot_check_is_off() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(1),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("dup"), child_slot("dup")],
        };
        // require_unique_slot_ids defaults to false
        assert!(validate_plan(&plan).is_ok());
    }

    #[test]
    fn validate_plan_flags_missing_capability_descriptor() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::SemanticAssembly {
            assembly_id: elem(1),
        });
        plan.validation.require_capability_descriptor = Some("house".into());
        // No matching provenance.source_recipe_id.
        let err = validate_plan(&plan).unwrap_err();
        assert!(matches!(
            err,
            PromotionEmissionError::MissingCapabilityDescriptor(ref s) if s == "house"
        ));
    }

    #[test]
    fn validate_plan_passes_with_matching_capability() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::SemanticAssembly {
            assembly_id: elem(1),
        });
        plan.validation.require_capability_descriptor = Some("house".into());
        plan.provenance.source_recipe_id = Some("house".into());
        assert!(validate_plan(&plan).is_ok());
    }

    #[test]
    fn element_id_preservation_off_returns_no_blockers() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Selection {
            element_ids: vec![elem(1)],
        });
        plan.element_id_preservation.mode = ElementIdPreservationMode::None;
        plan.element_id_preservation.source_element_to_slot_realization =
            vec![(elem(1), "anything".into())];
        let blockers = validate_element_id_preservation(&plan, &HashSet::new());
        assert!(blockers.is_empty());
    }

    #[test]
    fn duplicate_source_to_same_realization_is_flagged() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(99),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("only")],
        };
        plan.element_id_preservation.source_element_to_slot_realization = vec![
            (elem(1), "only".into()),
            (elem(2), "only".into()),
        ];
        let blockers = validate_element_id_preservation(&plan, &HashSet::new());
        assert_eq!(blockers.len(), 1);
        match &blockers[0] {
            ElementIdBlocker::DuplicateSourceMapsToSameRealization {
                realization_slot_id,
                sources,
            } => {
                assert_eq!(realization_slot_id, "only");
                let mut sorted = sources.clone();
                sorted.sort_by_key(|e| e.0);
                assert_eq!(sorted, vec![elem(1), elem(2)]);
            }
            other => panic!("unexpected blocker: {other:?}"),
        }
    }

    #[test]
    fn target_id_occupied_outside_replacement_set_is_flagged() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(1),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("a")],
        };
        plan.element_id_preservation.source_element_to_slot_realization =
            vec![(elem(1), "a".into())];
        let mut existing = HashSet::new();
        existing.insert(elem(1));
        let blockers = validate_element_id_preservation(&plan, &existing);
        assert_eq!(blockers.len(), 1);
        assert!(matches!(
            blockers[0],
            ElementIdBlocker::TargetIdOccupiedOutsideReplacementSet { target, .. }
                if target == elem(1)
        ));
    }

    #[test]
    fn realization_without_stable_slot_address_is_flagged_for_compound() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(99),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("known")],
        };
        plan.element_id_preservation.source_element_to_slot_realization = vec![
            (elem(1), "unknown".into()),
            (elem(2), "".into()),
        ];
        let blockers = validate_element_id_preservation(&plan, &HashSet::new());
        assert_eq!(blockers.len(), 2);
        for b in &blockers {
            assert!(matches!(
                b,
                ElementIdBlocker::RealizationLacksStableSlotAddress { .. }
            ));
        }
    }

    #[test]
    fn leaf_plan_with_non_empty_slot_id_is_flagged() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Selection {
            element_ids: vec![elem(1)],
        });
        plan.element_id_preservation.source_element_to_slot_realization =
            vec![(elem(1), "should-be-empty".into())];
        let blockers = validate_element_id_preservation(&plan, &HashSet::new());
        assert_eq!(blockers.len(), 1);
        assert!(matches!(
            blockers[0],
            ElementIdBlocker::RealizationLacksStableSlotAddress { source } if source == elem(1)
        ));
    }

    #[test]
    fn clean_compound_preservation_yields_no_blockers() {
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Group {
            group_id: elem(99),
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("a"), child_slot("b")],
        };
        plan.element_id_preservation.source_element_to_slot_realization = vec![
            (elem(1), "a".into()),
            (elem(2), "b".into()),
        ];
        let blockers = validate_element_id_preservation(&plan, &HashSet::new());
        assert!(blockers.is_empty());
    }

    #[test]
    fn promotion_plan_error_is_an_error_trait() {
        fn assert_error<E: std::error::Error>(_e: &E) {}
        let err = PromotionEmissionError::DuplicateSlotId("x".into());
        assert_error(&err);
        assert_eq!(format!("{err}"), "validation: duplicate slot id `x`");
    }
}
