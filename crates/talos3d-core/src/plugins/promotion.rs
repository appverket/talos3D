//! Shared `PromotionPlan` boundary for **Make Reusable** flows.
//!
//! Per PP-A2DB-0 (ADR-047, `ASSEMBLY_TO_DEFINITION_BRIDGE_AGREEMENT.md`)
//! every Make Reusable flow lowers its source-specific work into a
//! `PromotionPlan`, and a single shared `PromotionDraftEmitter` consumes
//! plans regardless of source kind (selection / group / SemanticAssembly).
//!
//! This module owns:
//!
//! - the data shapes for plans, preservation, validation, and emission
//!   records;
//! - the `PromotionSourceAdapter` and `PromotionDraftEmitter` traits;
//! - the shared validation entry points (`validate_plan` and
//!   `validate_element_id_preservation`);
//! - a concrete `SelectionPromotionSource` that lifts a `Vec<ElementId>`
//!   into a leaf `PromotionPlan` (the simplest source adapter; group and
//!   SemanticAssembly adapters land in PP-DPROMOTE-3 and PP-A2DB-1
//!   respectively);
//! - a concrete `DefaultPromotionDraftEmitter` that runs the validation
//!   gates, enforces ElementId preservation, and inserts a
//!   `DefinitionDraft` into a `DefinitionDraftRegistry` via an injected
//!   body builder.
//!
//! ElementId preservation is shared infrastructure, not SemanticAssembly-
//! specific: `validate_element_id_preservation` reports the three blocker
//! shapes named in the agreement so that selection, group, and assembly
//! promotion all get the same identity-stability guarantees when they
//! replace the source with the new Occurrence.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::plugins::{
    definition_authoring::{DefinitionDraft, DefinitionDraftId, DefinitionDraftRegistry},
    identity::ElementId,
    modeling::definition::{ChildSlotDef, Definition, ParameterBinding, TransformBinding},
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

// === Selection source adapter ==============================================

/// Errors produced by `SelectionPromotionSource::build_plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionAdapterError {
    /// The selection contained no element ids.
    EmptySelection,
    /// The configured `preservation_target` is not part of the supplied
    /// selection.
    PreservationTargetNotInSelection { target: ElementId },
    /// `preservation_target` was set on a source adapter whose
    /// `replace_source` flag is `false`. Preservation is only meaningful
    /// when the source is being replaced.
    PreservationRequestedWithoutReplaceSource,
}

impl std::fmt::Display for SelectionAdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySelection => write!(f, "selection adapter: empty selection"),
            Self::PreservationTargetNotInSelection { target } => write!(
                f,
                "selection adapter: preservation_target {target:?} is not in the selection"
            ),
            Self::PreservationRequestedWithoutReplaceSource => write!(
                f,
                "selection adapter: preservation_target requires replace_source = true"
            ),
        }
    }
}

impl std::error::Error for SelectionAdapterError {}

/// The pure input to `SelectionPromotionSource::build_plan`. Carries only
/// the source element ids; the rest of the plan shape is configured on the
/// adapter itself.
#[derive(Debug, Clone)]
pub struct SelectionPromotionInput {
    pub source_ids: Vec<ElementId>,
}

/// Lifts a flat selection into a leaf `PromotionPlan`.
///
/// PP-A2DB-0 only ships the *boundary*; selection promotion produces a
/// leaf plan because compound emission and slot decomposition land with
/// PP-DPROMOTE-3. When `replace_source` is `true`, the produced plan
/// carries a `ReplaceWithOccurrence` policy, and — if `preservation_target`
/// is set — an `ElementIdPreservationPlan` requesting that the target's
/// id be reused as the new Occurrence's id.
#[derive(Debug, Clone, Default)]
pub struct SelectionPromotionSource {
    /// Friendly name carried in the produced `Definition` body via the
    /// body builder.
    pub name: String,
    /// Whether the source elements should be replaced with an Occurrence
    /// of the new Definition. `false` produces a `NoReplacement` plan.
    pub replace_source: bool,
    /// If `replace_source` is `true`, this id is the source element whose
    /// `ElementId` is preserved as the realization id for the leaf
    /// Occurrence. Must be present in `SelectionPromotionInput.source_ids`.
    pub preservation_target: Option<ElementId>,
    /// AuthoringScript / agent attribution carried into provenance.
    pub provenance: PromotionProvenance,
}

impl PromotionSourceAdapter for SelectionPromotionSource {
    type SourceInput = SelectionPromotionInput;
    type Error = SelectionAdapterError;

    fn build_plan(&self, source: Self::SourceInput) -> Result<PromotionPlan, Self::Error> {
        if source.source_ids.is_empty() {
            return Err(SelectionAdapterError::EmptySelection);
        }
        if !self.replace_source && self.preservation_target.is_some() {
            return Err(SelectionAdapterError::PreservationRequestedWithoutReplaceSource);
        }
        if let Some(target) = self.preservation_target {
            if !source.source_ids.contains(&target) {
                return Err(SelectionAdapterError::PreservationTargetNotInSelection { target });
            }
        }

        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Selection {
            element_ids: source.source_ids.clone(),
        });
        plan.provenance = self.provenance.clone();

        if self.replace_source {
            plan.source_replacement = SourceReplacementPolicy::ReplaceWithOccurrence {
                preserve_assembly_wrapper: false,
            };
            if let Some(target) = self.preservation_target {
                plan.element_id_preservation = ElementIdPreservationPlan {
                    mode: ElementIdPreservationMode::PreserveWherePossible,
                    // Leaf realization uses the empty slot address per the
                    // agreement; `validate_element_id_preservation` flags
                    // non-empty slot addresses on leaf plans.
                    source_element_to_slot_realization: vec![(target, String::new())],
                    conflict_policy: ElementIdConflictPolicy::PreserveOrReportBlocker,
                };
            }
        }

        Ok(plan)
    }
}

// === Default emitter =======================================================

/// A reusable `PromotionDraftEmitter` that consumes any `PromotionPlan`,
/// runs the shared validation gates, enforces ElementId preservation, and
/// inserts a `DefinitionDraft` into a `DefinitionDraftRegistry`. The
/// concrete `Definition` body is supplied by an injected body builder so
/// that this emitter remains source-agnostic — selection promotion can
/// build a leaf body, group/SemanticAssembly promotion can build a
/// compound body, and the emitter does not need to know which.
///
/// The caller computes the set of `ElementId`s that exist outside the
/// replacement set (typically `world_ids \ source_ids`) and passes it in;
/// keeping that out of the emitter keeps `promotion.rs` ECS-free.
pub struct DefaultPromotionDraftEmitter<'a, F>
where
    F: FnMut(&PromotionPlan) -> Result<Definition, PromotionEmissionError>,
{
    drafts: &'a mut DefinitionDraftRegistry,
    existing_ids_outside_replacement_set: &'a HashSet<ElementId>,
    body_builder: F,
}

impl<'a, F> DefaultPromotionDraftEmitter<'a, F>
where
    F: FnMut(&PromotionPlan) -> Result<Definition, PromotionEmissionError>,
{
    pub fn new(
        drafts: &'a mut DefinitionDraftRegistry,
        existing_ids_outside_replacement_set: &'a HashSet<ElementId>,
        body_builder: F,
    ) -> Self {
        Self {
            drafts,
            existing_ids_outside_replacement_set,
            body_builder,
        }
    }
}

impl<'a, F> PromotionDraftEmitter for DefaultPromotionDraftEmitter<'a, F>
where
    F: FnMut(&PromotionPlan) -> Result<Definition, PromotionEmissionError>,
{
    fn emit(
        &mut self,
        plan: PromotionPlan,
    ) -> Result<PromotionEmissionRecord, PromotionEmissionError> {
        // Run the shared pre-emission gates; this catches duplicate slot
        // ids and missing-capability-descriptor cases at the boundary.
        validate_plan(&plan)?;

        // Enforce ElementId preservation. Per the agreement we surface
        // blockers rather than silently rewriting ids; the registry stays
        // untouched on conflict.
        let blockers =
            validate_element_id_preservation(&plan, self.existing_ids_outside_replacement_set);
        if !blockers.is_empty() {
            return Err(PromotionEmissionError::ElementIdPreservation(blockers));
        }

        // Build the body via the injected builder. The builder may itself
        // surface a `PromotionEmissionError` (e.g. a domain-specific
        // capability descriptor mismatch).
        let body = (self.body_builder)(&plan)?;

        // Identity map mirrors the (preserved) preservation plan. Because
        // we already rejected blockers above, every entry here is a clean
        // preservation that the downstream `SemanticGraphMigrationDiff`
        // consumer can rely on.
        let identity_map = match plan.element_id_preservation.mode {
            ElementIdPreservationMode::None => Vec::new(),
            ElementIdPreservationMode::PreserveWherePossible => plan
                .element_id_preservation
                .source_element_to_slot_realization
                .clone(),
        };

        let draft = DefinitionDraft {
            draft_id: plan.draft_id.clone(),
            source_definition_id: None,
            source_library_id: None,
            working_copy: body,
            dirty: true,
        };
        let inserted_id = self.drafts.insert(draft);

        Ok(PromotionEmissionRecord {
            draft_id: inserted_id,
            identity_map,
            blockers: Vec::new(),
        })
    }
}

// === SemanticAssembly source adapter =======================================

/// Classification of one assembly member, supplied by the snapshot
/// builder. The adapter uses this to enforce the PP-A2DB-1 *flatness*
/// requirement (nested SemanticAssembly members are unsupported in this
/// PP) and to decide whether ElementId preservation is appropriate per
/// member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssemblyMemberKind {
    /// A first-class authored entity (wall, slab, leaf geometry, etc.).
    AuthoredEntity,
    /// An Occurrence of an existing reusable Definition.
    Occurrence,
    /// Another `SemanticAssembly`. Unsupported in PP-A2DB-1; the adapter
    /// surfaces this as a preview blocker rather than silently flattening.
    NestedAssembly,
}

/// One member of a flat-assembly snapshot. Mirrors `AssemblyMemberRef`
/// but ECS-free so promotion.rs stays decoupled from the Bevy world; the
/// caller (typically `model_api.rs` in slice B) gathers the snapshot
/// from world state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssemblyMemberSnapshot {
    pub element_id: ElementId,
    pub role: String,
    pub kind: AssemblyMemberKind,
    /// Definition id when `kind == Occurrence`. The promoted compound
    /// `Definition` reuses this id as the child slot's `definition_id`.
    /// `None` for authored leaves; the body builder creates a leaf
    /// Definition for those members in slice B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_definition_id: Option<crate::plugins::modeling::definition::DefinitionId>,
}

/// Optional capability projection carried into the promoted Definition.
/// Per the agreement, none of these fields are required to ratify a
/// SemanticAssembly promotion — when absent, the adapter records the
/// gap as a `capability_projection_outdated` warning rather than
/// rejecting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssemblyCapabilityProjection {
    pub assembly_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_vocabulary_version: Option<String>,
}

/// A live external reference into the assembly graph. Lets the source
/// adapter compute the `SemanticGraphMigrationDiff` (which assemblies
/// will be retargeted, which relations need to follow the new
/// Occurrence, which memberships go orphaned).
///
/// The shape is intentionally narrow — only the fields needed by
/// PP-A2DB-1's minimum migration diff. Slice B will broaden this when
/// world-side retargeting lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAssemblyMembership {
    /// The assembly that references one or more of this assembly's
    /// members.
    pub assembly_id: ElementId,
    /// Members of *that* assembly (by `element_id`) that are targets of
    /// the source assembly's promotion. The source adapter will
    /// re-target these to the promoted Occurrence on commit.
    pub member_targets: Vec<ElementId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRelation {
    pub relation_id: ElementId,
    pub source: ElementId,
    pub target: ElementId,
    pub relation_type: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalGraph {
    /// Assemblies (other than the source) that reference any source
    /// member.
    #[serde(default)]
    pub memberships: Vec<ExternalAssemblyMembership>,
    /// Relations (other than internal source-to-source) whose endpoints
    /// touch a source member.
    #[serde(default)]
    pub relations: Vec<ExternalRelation>,
}

/// One internal `SemanticRelation` snapshot — both endpoints either
/// reference the source assembly itself (`self`) or one of its source
/// members. Slice A's adapter classifies these into
/// `SemanticRelationTemplate` candidates; slice B will source them
/// from world state in `promotion_world.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalRelationSnapshot {
    pub relation_id: ElementId,
    pub source: ElementId,
    pub target: ElementId,
    pub relation_type: String,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// Pure input to `SemanticAssemblyPromotionSource::build_plan`. The
/// adapter is ECS-free; the caller builds this struct from world state.
#[derive(Debug, Clone)]
pub struct SemanticAssemblyPromotionInput {
    pub assembly_id: ElementId,
    pub members: Vec<AssemblyMemberSnapshot>,
    pub capability: AssemblyCapabilityProjection,
    pub external_graph: ExternalGraph,
    /// Relations whose ECS source/target both touch the source
    /// assembly (the assembly itself or a source member). Candidates
    /// for `SemanticRelationTemplate` on the promoted Definition.
    pub internal_relations: Vec<InternalRelationSnapshot>,
    /// Original assembly-level parameters carried into the promoted
    /// Definition's provenance (PP-A2DB-1 trivially passes them
    /// through; rich parameter inference lands later).
    pub source_parameters: serde_json::Value,
    /// Original assembly-level metadata (label, etc.) carried into
    /// provenance and migration warnings.
    pub source_label: String,
}

/// Errors produced by `SemanticAssemblyPromotionSource::build_plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticAssemblyAdapterError {
    /// The assembly had no members.
    EmptyAssembly { assembly_id: ElementId },
    /// One or more members were nested SemanticAssemblies. PP-A2DB-1
    /// rejects this shape — the agreement defers nested-assembly
    /// cascade to PP-A2DB-4.
    UnsupportedNestedAssemblyMembers { offending_members: Vec<ElementId> },
    /// A member had `kind == Occurrence` but no `occurrence_definition_id`.
    OccurrenceMemberMissingDefinitionId { member: ElementId },
}

impl std::fmt::Display for SemanticAssemblyAdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyAssembly { assembly_id } => {
                write!(f, "semantic-assembly adapter: assembly {assembly_id:?} has no members")
            }
            Self::UnsupportedNestedAssemblyMembers { offending_members } => write!(
                f,
                "semantic-assembly adapter: PP-A2DB-1 rejects nested SemanticAssembly members ({} found)",
                offending_members.len()
            ),
            Self::OccurrenceMemberMissingDefinitionId { member } => write!(
                f,
                "semantic-assembly adapter: occurrence member {member:?} is missing occurrence_definition_id"
            ),
        }
    }
}

impl std::error::Error for SemanticAssemblyAdapterError {}

/// PP-A2DB-1 minimum surface for the migration diff that promotion
/// preview shows the user before commit. PP-A2DB-2 slice A extends it
/// with `candidate_relation_templates` and `preserved_relations` so
/// internal source-internal relations become reusable templates while
/// boundary-spanning relations are surfaced for audit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticGraphMigrationDiff {
    /// Other assemblies that hold any of the source members; their
    /// `member_targets` will be retargeted to the promoted Occurrence
    /// on commit. The surviving source assembly itself appears in this
    /// list as a degenerate self-retarget.
    #[serde(default)]
    pub retargeted_assemblies: Vec<RetargetedAssembly>,
    /// External relations whose endpoints touch a source member; on
    /// commit they will be re-pointed to the promoted Occurrence.
    #[serde(default)]
    pub retargeted_relations: Vec<RetargetedRelation>,
    /// Memberships that cannot be retargeted because the matching
    /// member is dropped (e.g. a duplicate-role indexed slot would
    /// require choosing a representative; the chosen one wins, the
    /// rest go orphaned). PP-A2DB-1 surfaces these as warnings rather
    /// than blocking emission.
    #[serde(default)]
    pub orphaned_memberships: Vec<OrphanedMembership>,
    /// Internal relations (both endpoints inside the source) lifted
    /// into reusable `SemanticRelationTemplate` candidates. Accepted
    /// templates land on the promoted Definition; rejection (or
    /// "preserve as instance" instead) is a UX decision — slice A
    /// records every internal relation as a candidate. PP-A2DB-2.
    #[serde(default)]
    pub candidate_relation_templates: Vec<SemanticRelationTemplate>,
    /// Relations that touched a source member but had at least one
    /// boundary-spanning endpoint, so they cannot become
    /// `SemanticRelationTemplate` candidates without an
    /// `ExternalContextRequirement` declaration. Slice A records them
    /// verbatim; slice B will classify each as `HostContract` /
    /// `RequiredContext` / `AdvisoryContext` / `DropWithAudit` per
    /// the relation descriptor. PP-A2DB-2.
    #[serde(default)]
    pub preserved_relations: Vec<PreservedRelation>,
    /// Free-form preview warnings (capability projection outdated,
    /// duplicate role indexed, etc.).
    #[serde(default)]
    pub warnings: Vec<MigrationWarning>,
}

/// Where one endpoint of a `SemanticRelationTemplate` resolves inside
/// the promoted Definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RelationEndpoint {
    /// The promoted Definition itself (typically the surviving
    /// SemanticAssembly's wrapper Occurrence).
    SelfRoot,
    /// One of the Definition's child slots, addressed by `slot_id`.
    Slot(String),
}

/// A relation whose endpoints both resolve into the promoted
/// Definition (self or a child slot). Stored on the diff under
/// `candidate_relation_templates`. PP-A2DB-2 slice A; slice C will
/// store accepted templates on the Definition itself and materialize
/// them as authored `SemanticRelation`s on Occurrence creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticRelationTemplate {
    pub subject: RelationEndpoint,
    pub relation_type: String,
    pub object: RelationEndpoint,
    /// Original relation parameters carried verbatim. Slice C will
    /// normalize concrete-entity references inside this payload into
    /// slot/parameter references; for slice A we preserve the
    /// original JSON so the audit trail is complete.
    #[serde(default)]
    pub parameters: serde_json::Value,
    /// The original relation's `ElementId` so downstream consumers
    /// can correlate template <-> source.
    pub source_relation_id: ElementId,
}

/// A relation that the adapter could NOT lift into a template
/// because at least one endpoint references something outside the
/// promoted Definition (a sibling assembly, an unrelated authored
/// entity, etc.). Slice A surfaces them for preview audit; slice B
/// will replace this with descriptor-backed
/// `ExternalRelationClassification`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedRelation {
    pub relation_id: ElementId,
    pub source: ElementId,
    pub target: ElementId,
    pub relation_type: String,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetargetedAssembly {
    pub assembly_id: ElementId,
    /// Source members that were referenced from this assembly.
    pub from_members: Vec<ElementId>,
    /// Per-source-member: the slot id under the new Occurrence that
    /// will receive the retargeted reference. Slice B will look this
    /// up via the emitter's `identity_map`; in slice A we record the
    /// adapter's intended slot mapping.
    pub to_slot_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetargetedRelation {
    pub relation_id: ElementId,
    pub original_source: ElementId,
    pub original_target: ElementId,
    pub relation_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanedMembership {
    pub assembly_id: ElementId,
    pub dropped_member: ElementId,
    pub reason: OrphanReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrphanReason {
    /// The member was a nested SemanticAssembly and was rejected. In
    /// PP-A2DB-1 the whole promotion fails on this case, so the diff
    /// is informational only; PP-A2DB-4 will treat it differently.
    NestedAssemblyRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationWarning {
    DuplicateRoleIndexed { role: String, count: usize },
    CapabilityProjectionOutdated { detail: String },
}

/// The flat-assembly source adapter. Produces a compound
/// `PromotionPlan` whose child slots match the assembly's members
/// one-to-one (with indexed slot ids when roles repeat), plus the
/// migration diff Preview displays before the user commits.
///
/// The adapter is ECS-free: the caller builds a
/// `SemanticAssemblyPromotionInput` from world state and the adapter
/// transforms it. World mutation lives in slice B.
#[derive(Debug, Clone)]
pub struct SemanticAssemblyPromotionSource {
    /// Friendly name carried into the promoted `Definition` (the body
    /// builder consumes this; the adapter just records it on
    /// provenance).
    pub name: String,
    /// Whether the source assembly survives as a retargeted wrapper
    /// after commit. Mirrors
    /// `SourceReplacementPolicy::ReplaceWithOccurrence{
    /// preserve_assembly_wrapper: true }` when set.
    pub replace_source: bool,
    /// AuthoringScript / agent attribution carried into provenance.
    pub provenance: PromotionProvenance,
}

/// Full output of `SemanticAssemblyPromotionSource::build_plan`. The
/// `PromotionSourceAdapter::build_plan` trait method only returns the
/// plan, but the adapter also produces a migration diff that the MCP
/// preview surfaces. Callers that need both go through
/// `build_plan_and_diff` directly.
#[derive(Debug, Clone)]
pub struct SemanticAssemblyPromotionOutput {
    pub plan: PromotionPlan,
    pub migration_diff: SemanticGraphMigrationDiff,
}

impl SemanticAssemblyPromotionSource {
    /// Adapter-native entry point. Returns the plan and the migration
    /// diff in one pass; the trait method below delegates to this and
    /// drops the diff.
    pub fn build_plan_and_diff(
        &self,
        input: SemanticAssemblyPromotionInput,
    ) -> Result<SemanticAssemblyPromotionOutput, SemanticAssemblyAdapterError> {
        // Reject empty / nested-assembly inputs first; both are hard
        // blocks per the agreement. Empty assemblies have nothing to
        // promote; nested members defer to PP-A2DB-4.
        if input.members.is_empty() {
            return Err(SemanticAssemblyAdapterError::EmptyAssembly {
                assembly_id: input.assembly_id,
            });
        }
        let nested: Vec<ElementId> = input
            .members
            .iter()
            .filter(|m| m.kind == AssemblyMemberKind::NestedAssembly)
            .map(|m| m.element_id)
            .collect();
        if !nested.is_empty() {
            return Err(SemanticAssemblyAdapterError::UnsupportedNestedAssemblyMembers {
                offending_members: nested,
            });
        }
        for member in &input.members {
            if member.kind == AssemblyMemberKind::Occurrence
                && member.occurrence_definition_id.is_none()
            {
                return Err(
                    SemanticAssemblyAdapterError::OccurrenceMemberMissingDefinitionId {
                        member: member.element_id,
                    },
                );
            }
        }

        // === Member -> slot mapping ====================================
        //
        // Indexed slot ids when a role repeats: `wall`, `wall_2`,
        // `wall_3`, ... PP-97 (collection slots) will replace this with
        // a richer scheme; until then, indexed slots keep the slot id
        // stable and unique within the Definition.
        let mut role_counts: HashMap<&str, usize> = HashMap::new();
        let mut child_slots: Vec<ChildSlotDef> = Vec::with_capacity(input.members.len());
        let mut member_slot_ids: Vec<(ElementId, String)> = Vec::with_capacity(input.members.len());
        let mut warnings: Vec<MigrationWarning> = Vec::new();
        let mut role_role_counts: HashMap<String, usize> = HashMap::new();

        for member in &input.members {
            *role_role_counts.entry(member.role.clone()).or_insert(0) += 1;
        }
        for (role, count) in &role_role_counts {
            if *count > 1 {
                warnings.push(MigrationWarning::DuplicateRoleIndexed {
                    role: role.clone(),
                    count: *count,
                });
            }
        }

        for member in &input.members {
            let count = role_counts.entry(member.role.as_str()).or_insert(0);
            *count += 1;
            let slot_id = if role_role_counts.get(&member.role).copied().unwrap_or(1) <= 1 {
                member.role.clone()
            } else {
                format!("{}_{}", member.role, *count)
            };
            // Authored-leaf members borrow a placeholder DefinitionId
            // ("draft.leaf:<element_id>"); slice B will replace this
            // with the leaf Definition that the body builder produces.
            // Occurrence members reuse their existing definition_id
            // directly per the agreement.
            let definition_id = match member.kind {
                AssemblyMemberKind::Occurrence => member
                    .occurrence_definition_id
                    .clone()
                    .expect("validated above"),
                AssemblyMemberKind::AuthoredEntity => {
                    crate::plugins::modeling::definition::DefinitionId(format!(
                        "draft.leaf:{}",
                        member.element_id.0
                    ))
                }
                AssemblyMemberKind::NestedAssembly => unreachable!("rejected above"),
            };
            child_slots.push(ChildSlotDef {
                slot_id: slot_id.clone(),
                role: member.role.clone(),
                definition_id,
                parameter_bindings: Vec::new(),
                transform_binding: TransformBinding::default(),
                suppression_expr: None,
            });
            member_slot_ids.push((member.element_id, slot_id));
        }

        // === Capability projection =====================================
        if input.capability.descriptor_id.is_none()
            || input.capability.descriptor_version.is_none()
        {
            warnings.push(MigrationWarning::CapabilityProjectionOutdated {
                detail: format!(
                    "assembly_type='{}': descriptor_id={:?}, descriptor_version={:?}",
                    input.capability.assembly_type,
                    input.capability.descriptor_id,
                    input.capability.descriptor_version,
                ),
            });
        }

        // === Plan ======================================================
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::SemanticAssembly {
            assembly_id: input.assembly_id,
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: child_slots.clone(),
        };
        plan.validation.require_unique_slot_ids = true;
        plan.provenance = PromotionProvenance {
            agent: self
                .provenance
                .agent
                .clone()
                .or_else(|| Some("semantic_assembly".into())),
            source_recipe_id: self.provenance.source_recipe_id.clone(),
            authoring_script_payload: Some(serde_json::json!({
                "kind": "semantic_assembly",
                "name": self.name,
                "label": input.source_label,
                "assembly_type": input.capability.assembly_type,
                "descriptor_id": input.capability.descriptor_id,
                "descriptor_version": input.capability.descriptor_version,
                "role_vocabulary_version": input.capability.role_vocabulary_version,
                "members": input
                    .members
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "element_id": m.element_id,
                            "role": m.role,
                            "kind": m.kind,
                        })
                    })
                    .collect::<Vec<_>>(),
                "source_parameters": input.source_parameters,
            })),
        };

        if self.replace_source {
            plan.source_replacement = SourceReplacementPolicy::ReplaceWithOccurrence {
                preserve_assembly_wrapper: true,
            };
            plan.element_id_preservation = ElementIdPreservationPlan {
                mode: ElementIdPreservationMode::PreserveWherePossible,
                source_element_to_slot_realization: member_slot_ids.clone(),
                conflict_policy: ElementIdConflictPolicy::PreserveOrReportBlocker,
            };
        }

        // === Migration diff ============================================
        // Self-retarget: the surviving source assembly's `members` field
        // collapses into a single { target: promoted_occurrence_id,
        // role: "realization" } reference on commit. Slice A records the
        // intended retargeting; slice B applies it.
        let mut retargeted_assemblies: Vec<RetargetedAssembly> = Vec::new();
        let source_member_ids: Vec<ElementId> = input.members.iter().map(|m| m.element_id).collect();
        if self.replace_source {
            retargeted_assemblies.push(RetargetedAssembly {
                assembly_id: input.assembly_id,
                from_members: source_member_ids.clone(),
                // Surviving source assembly collapses to one realization
                // entry; the slot id is the empty leaf address (the new
                // Occurrence is single-rooted from the surviving
                // assembly's point of view).
                to_slot_ids: vec![String::new()],
            });
        }

        // External assemblies that hold any source member are recorded
        // for retargeting. Each external `member_targets` entry maps to
        // the slot id chosen above (or to the new Occurrence's leaf id
        // for matching that didn't go through a slot).
        let slot_lookup: HashMap<ElementId, String> =
            member_slot_ids.iter().cloned().collect();
        for ext in &input.external_graph.memberships {
            if ext.assembly_id == input.assembly_id {
                // Same as the self-retarget above; skip duplicate.
                continue;
            }
            let mut from_members: Vec<ElementId> = Vec::new();
            let mut to_slot_ids: Vec<String> = Vec::new();
            for target in &ext.member_targets {
                if let Some(slot) = slot_lookup.get(target) {
                    from_members.push(*target);
                    to_slot_ids.push(slot.clone());
                }
            }
            if !from_members.is_empty() {
                retargeted_assemblies.push(RetargetedAssembly {
                    assembly_id: ext.assembly_id,
                    from_members,
                    to_slot_ids,
                });
            }
        }

        let source_member_set: HashSet<ElementId> = source_member_ids.iter().copied().collect();
        let retargeted_relations: Vec<RetargetedRelation> = input
            .external_graph
            .relations
            .iter()
            .filter(|r| {
                source_member_set.contains(&r.source) || source_member_set.contains(&r.target)
            })
            .map(|r| RetargetedRelation {
                relation_id: r.relation_id,
                original_source: r.source,
                original_target: r.target,
                relation_type: r.relation_type.clone(),
            })
            .collect();

        // === Internal relation classification (PP-A2DB-2 slice A) =====
        //
        // For each `InternalRelationSnapshot` the caller provided, walk
        // both endpoints. If both resolve cleanly to `self` (= the
        // source assembly id) or one of the source members (which we
        // can map to a slot id via `slot_lookup`), the relation
        // becomes a `SemanticRelationTemplate` candidate. Otherwise
        // the relation has at least one boundary-spanning endpoint;
        // it goes into `preserved_relations` for slice B's
        // descriptor-backed classification.
        //
        // The slot_lookup was built earlier and maps each source
        // ElementId to its child-slot id; slice A reuses it without
        // recomputation.
        let mut candidate_relation_templates: Vec<SemanticRelationTemplate> = Vec::new();
        let mut preserved_relations: Vec<PreservedRelation> = Vec::new();
        for relation in &input.internal_relations {
            let subject = classify_relation_endpoint(
                relation.source,
                input.assembly_id,
                &slot_lookup,
            );
            let object = classify_relation_endpoint(
                relation.target,
                input.assembly_id,
                &slot_lookup,
            );
            match (subject, object) {
                (Some(subject), Some(object)) => {
                    candidate_relation_templates.push(SemanticRelationTemplate {
                        subject,
                        relation_type: relation.relation_type.clone(),
                        object,
                        parameters: relation.parameters.clone(),
                        source_relation_id: relation.relation_id,
                    });
                }
                _ => {
                    preserved_relations.push(PreservedRelation {
                        relation_id: relation.relation_id,
                        source: relation.source,
                        target: relation.target,
                        relation_type: relation.relation_type.clone(),
                        parameters: relation.parameters.clone(),
                    });
                }
            }
        }

        let migration_diff = SemanticGraphMigrationDiff {
            retargeted_assemblies,
            retargeted_relations,
            orphaned_memberships: Vec::new(),
            candidate_relation_templates,
            preserved_relations,
            warnings,
        };

        Ok(SemanticAssemblyPromotionOutput {
            plan,
            migration_diff,
        })
    }
}

impl PromotionSourceAdapter for SemanticAssemblyPromotionSource {
    type SourceInput = SemanticAssemblyPromotionInput;
    type Error = SemanticAssemblyAdapterError;

    fn build_plan(&self, source: Self::SourceInput) -> Result<PromotionPlan, Self::Error> {
        Ok(self.build_plan_and_diff(source)?.plan)
    }
}

/// Map an internal-relation endpoint to a `RelationEndpoint` if
/// possible. Endpoints pointing at the source assembly itself become
/// `SelfRoot`; endpoints pointing at one of the source members
/// become `Slot(slot_id)` via the adapter's slot lookup. Anything
/// else (i.e. a boundary-spanning endpoint that the caller mistakenly
/// included in `internal_relations`) returns `None` and lands in
/// `preserved_relations` for audit.
fn classify_relation_endpoint(
    endpoint: ElementId,
    assembly_id: ElementId,
    slot_lookup: &HashMap<ElementId, String>,
) -> Option<RelationEndpoint> {
    if endpoint == assembly_id {
        return Some(RelationEndpoint::SelfRoot);
    }
    slot_lookup
        .get(&endpoint)
        .map(|slot_id| RelationEndpoint::Slot(slot_id.clone()))
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

    // === SelectionPromotionSource ==========================================

    #[test]
    fn selection_adapter_rejects_empty_selection() {
        let adapter = SelectionPromotionSource::default();
        let err = adapter
            .build_plan(SelectionPromotionInput {
                source_ids: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(err, SelectionAdapterError::EmptySelection);
    }

    #[test]
    fn selection_adapter_rejects_preservation_without_replace_source() {
        let adapter = SelectionPromotionSource {
            name: "Custom Group".into(),
            replace_source: false,
            preservation_target: Some(elem(7)),
            provenance: PromotionProvenance::default(),
        };
        let err = adapter
            .build_plan(SelectionPromotionInput {
                source_ids: vec![elem(7)],
            })
            .unwrap_err();
        assert_eq!(
            err,
            SelectionAdapterError::PreservationRequestedWithoutReplaceSource
        );
    }

    #[test]
    fn selection_adapter_rejects_preservation_target_outside_selection() {
        let adapter = SelectionPromotionSource {
            name: "Custom".into(),
            replace_source: true,
            preservation_target: Some(elem(99)),
            provenance: PromotionProvenance::default(),
        };
        let err = adapter
            .build_plan(SelectionPromotionInput {
                source_ids: vec![elem(1), elem(2)],
            })
            .unwrap_err();
        assert_eq!(
            err,
            SelectionAdapterError::PreservationTargetNotInSelection { target: elem(99) }
        );
    }

    #[test]
    fn selection_adapter_emits_leaf_plan_with_no_replacement_by_default() {
        let adapter = SelectionPromotionSource {
            name: "leaf".into(),
            replace_source: false,
            preservation_target: None,
            provenance: PromotionProvenance::default(),
        };
        let plan = adapter
            .build_plan(SelectionPromotionInput {
                source_ids: vec![elem(1), elem(2)],
            })
            .unwrap();
        assert!(matches!(plan.output_shape, PromotionOutputShape::Leaf));
        assert_eq!(
            plan.source_kind,
            PromotionSourceKind::Selection {
                element_ids: vec![elem(1), elem(2)],
            }
        );
        assert_eq!(plan.source_replacement, SourceReplacementPolicy::NoReplacement);
        assert!(plan
            .element_id_preservation
            .source_element_to_slot_realization
            .is_empty());
    }

    #[test]
    fn selection_adapter_with_replace_source_requests_preservation() {
        let adapter = SelectionPromotionSource {
            name: "preserved".into(),
            replace_source: true,
            preservation_target: Some(elem(2)),
            provenance: PromotionProvenance {
                agent: Some("test".into()),
                ..Default::default()
            },
        };
        let plan = adapter
            .build_plan(SelectionPromotionInput {
                source_ids: vec![elem(1), elem(2), elem(3)],
            })
            .unwrap();
        assert_eq!(
            plan.source_replacement,
            SourceReplacementPolicy::ReplaceWithOccurrence {
                preserve_assembly_wrapper: false,
            }
        );
        assert_eq!(
            plan.element_id_preservation.mode,
            ElementIdPreservationMode::PreserveWherePossible
        );
        assert_eq!(
            plan.element_id_preservation
                .source_element_to_slot_realization,
            vec![(elem(2), String::new())],
        );
        assert_eq!(plan.provenance.agent.as_deref(), Some("test"));
    }

    // === DefaultPromotionDraftEmitter ======================================

    fn leaf_body_builder()
    -> impl FnMut(&PromotionPlan) -> Result<Definition, PromotionEmissionError> {
        |plan| {
            // Carry the selection size into the name so tests can verify
            // the body builder actually saw the plan.
            let count = match &plan.source_kind {
                PromotionSourceKind::Selection { element_ids } => element_ids.len(),
                PromotionSourceKind::Group { .. } => 1,
                PromotionSourceKind::SemanticAssembly { .. } => 1,
            };
            Ok(crate::plugins::definition_authoring::blank_definition(format!(
                "promoted-{count}"
            )))
        }
    }

    #[test]
    fn default_emitter_inserts_draft_on_clean_emission() {
        let mut drafts = DefinitionDraftRegistry::default();
        let existing = HashSet::<ElementId>::new();
        let plan = SelectionPromotionSource {
            name: "clean".into(),
            replace_source: false,
            preservation_target: None,
            provenance: PromotionProvenance::default(),
        }
        .build_plan(SelectionPromotionInput {
            source_ids: vec![elem(10), elem(11)],
        })
        .unwrap();
        let plan_draft_id = plan.draft_id.clone();

        let mut emitter =
            DefaultPromotionDraftEmitter::new(&mut drafts, &existing, leaf_body_builder());
        let record = emitter.emit(plan).unwrap();

        assert_eq!(record.draft_id, plan_draft_id);
        assert!(record.blockers.is_empty());
        assert!(record.identity_map.is_empty());
        assert!(drafts.get(&plan_draft_id).is_some());
        assert_eq!(
            drafts.get(&plan_draft_id).unwrap().working_copy.name,
            "promoted-2"
        );
    }

    #[test]
    fn default_emitter_writes_identity_map_on_replace_source_promotion() {
        let mut drafts = DefinitionDraftRegistry::default();
        let existing = HashSet::<ElementId>::new();
        let plan = SelectionPromotionSource {
            name: "preserved".into(),
            replace_source: true,
            preservation_target: Some(elem(2)),
            provenance: PromotionProvenance::default(),
        }
        .build_plan(SelectionPromotionInput {
            source_ids: vec![elem(1), elem(2), elem(3)],
        })
        .unwrap();

        let mut emitter =
            DefaultPromotionDraftEmitter::new(&mut drafts, &existing, leaf_body_builder());
        let record = emitter.emit(plan).unwrap();

        assert!(record.blockers.is_empty());
        assert_eq!(record.identity_map, vec![(elem(2), String::new())]);
        assert_eq!(drafts.list().len(), 1);
    }

    #[test]
    fn default_emitter_returns_blockers_and_does_not_insert_on_conflict() {
        let mut drafts = DefinitionDraftRegistry::default();
        // The preservation target id is already live somewhere else in
        // the world — that's a `TargetIdOccupiedOutsideReplacementSet`
        // blocker, not a silent rewrite.
        let mut existing = HashSet::<ElementId>::new();
        existing.insert(elem(2));

        let plan = SelectionPromotionSource {
            name: "conflict".into(),
            replace_source: true,
            preservation_target: Some(elem(2)),
            provenance: PromotionProvenance::default(),
        }
        .build_plan(SelectionPromotionInput {
            source_ids: vec![elem(2)],
        })
        .unwrap();

        let mut emitter =
            DefaultPromotionDraftEmitter::new(&mut drafts, &existing, leaf_body_builder());
        let err = emitter.emit(plan).unwrap_err();
        match err {
            PromotionEmissionError::ElementIdPreservation(blockers) => {
                assert_eq!(blockers.len(), 1);
                assert!(matches!(
                    blockers[0],
                    ElementIdBlocker::TargetIdOccupiedOutsideReplacementSet { target, .. }
                        if target == elem(2)
                ));
            }
            other => panic!("expected preservation blocker, got {other:?}"),
        }
        // Registry untouched.
        assert_eq!(drafts.list().len(), 0);
    }

    #[test]
    fn default_emitter_propagates_validate_plan_errors() {
        let mut drafts = DefinitionDraftRegistry::default();
        let existing = HashSet::<ElementId>::new();
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Selection {
            element_ids: vec![elem(1)],
        });
        plan.output_shape = PromotionOutputShape::Compound {
            child_slots: vec![child_slot("dup"), child_slot("dup")],
        };
        plan.validation.require_unique_slot_ids = true;

        let mut emitter =
            DefaultPromotionDraftEmitter::new(&mut drafts, &existing, leaf_body_builder());
        let err = emitter.emit(plan).unwrap_err();
        assert_eq!(err, PromotionEmissionError::DuplicateSlotId("dup".into()));
        assert_eq!(drafts.list().len(), 0);
    }

    #[test]
    fn default_emitter_propagates_body_builder_errors() {
        let mut drafts = DefinitionDraftRegistry::default();
        let existing = HashSet::<ElementId>::new();
        let plan = SelectionPromotionSource {
            name: "rejected".into(),
            replace_source: false,
            preservation_target: None,
            provenance: PromotionProvenance::default(),
        }
        .build_plan(SelectionPromotionInput {
            source_ids: vec![elem(1)],
        })
        .unwrap();

        let mut emitter = DefaultPromotionDraftEmitter::new(&mut drafts, &existing, |_| {
            Err(PromotionEmissionError::MissingCapabilityDescriptor(
                "house".into(),
            ))
        });
        let err = emitter.emit(plan).unwrap_err();
        assert!(matches!(
            err,
            PromotionEmissionError::MissingCapabilityDescriptor(ref s) if s == "house"
        ));
        assert_eq!(drafts.list().len(), 0);
    }

    #[test]
    fn default_emitter_does_not_emit_identity_map_when_preservation_is_off() {
        // Even with a preservation map populated by hand, mode == None
        // should yield an empty identity_map. Defensive against future
        // adapter drift.
        let mut drafts = DefinitionDraftRegistry::default();
        let existing = HashSet::<ElementId>::new();
        let mut plan = PromotionPlan::new_leaf(PromotionSourceKind::Selection {
            element_ids: vec![elem(1)],
        });
        plan.element_id_preservation.mode = ElementIdPreservationMode::None;
        plan.element_id_preservation.source_element_to_slot_realization =
            vec![(elem(1), String::new())];

        let mut emitter =
            DefaultPromotionDraftEmitter::new(&mut drafts, &existing, leaf_body_builder());
        let record = emitter.emit(plan).unwrap();
        assert!(record.identity_map.is_empty());
        assert_eq!(drafts.list().len(), 1);
    }

    // === SemanticAssemblyPromotionSource ===================================

    fn member(
        id: u64,
        role: &str,
        kind: AssemblyMemberKind,
        occ_def: Option<&str>,
    ) -> AssemblyMemberSnapshot {
        AssemblyMemberSnapshot {
            element_id: elem(id),
            role: role.to_string(),
            kind,
            occurrence_definition_id: occ_def.map(|s| DefinitionId(s.to_string())),
        }
    }

    fn assembly_input_for(members: Vec<AssemblyMemberSnapshot>) -> SemanticAssemblyPromotionInput {
        SemanticAssemblyPromotionInput {
            assembly_id: elem(100),
            members,
            capability: AssemblyCapabilityProjection {
                assembly_type: "test_assembly".into(),
                descriptor_id: Some("descriptor.test".into()),
                descriptor_version: Some("1.0".into()),
                role_vocabulary_version: Some("v1".into()),
            },
            external_graph: ExternalGraph::default(),
            internal_relations: Vec::new(),
            source_parameters: serde_json::Value::Null,
            source_label: "Test Assembly".into(),
        }
    }

    #[test]
    fn assembly_adapter_rejects_empty_assembly() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "n".into(),
            replace_source: false,
            provenance: PromotionProvenance::default(),
        };
        let err = adapter
            .build_plan_and_diff(assembly_input_for(Vec::new()))
            .unwrap_err();
        assert_eq!(
            err,
            SemanticAssemblyAdapterError::EmptyAssembly { assembly_id: elem(100) }
        );
    }

    #[test]
    fn assembly_adapter_rejects_nested_assembly_members() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "n".into(),
            replace_source: false,
            provenance: PromotionProvenance::default(),
        };
        let input = assembly_input_for(vec![
            member(1, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "subroom", AssemblyMemberKind::NestedAssembly, None),
        ]);
        let err = adapter.build_plan_and_diff(input).unwrap_err();
        match err {
            SemanticAssemblyAdapterError::UnsupportedNestedAssemblyMembers {
                offending_members,
            } => assert_eq!(offending_members, vec![elem(2)]),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn assembly_adapter_rejects_occurrence_member_without_definition_id() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "n".into(),
            replace_source: false,
            provenance: PromotionProvenance::default(),
        };
        let input = assembly_input_for(vec![member(
            7,
            "anchor",
            AssemblyMemberKind::Occurrence,
            None,
        )]);
        let err = adapter.build_plan_and_diff(input).unwrap_err();
        assert_eq!(
            err,
            SemanticAssemblyAdapterError::OccurrenceMemberMissingDefinitionId { member: elem(7) }
        );
    }

    #[test]
    fn assembly_adapter_emits_compound_plan_with_one_slot_per_unique_role() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Door+Frame".into(),
            replace_source: false,
            provenance: PromotionProvenance::default(),
        };
        let input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "leaf", AssemblyMemberKind::AuthoredEntity, None),
            member(
                3,
                "lock",
                AssemblyMemberKind::Occurrence,
                Some("hardware.lock"),
            ),
        ]);
        let out = adapter.build_plan_and_diff(input).unwrap();
        let slots = match &out.plan.output_shape {
            PromotionOutputShape::Compound { child_slots } => child_slots,
            other => panic!("expected compound plan, got {other:?}"),
        };
        let slot_ids: Vec<&str> = slots.iter().map(|s| s.slot_id.as_str()).collect();
        assert_eq!(slot_ids, vec!["frame", "leaf", "lock"]);
        // Occurrence member reuses its existing definition id; authored
        // leaves get the placeholder draft.leaf:* id pending slice B.
        assert_eq!(slots[2].definition_id, DefinitionId("hardware.lock".into()));
        assert!(slots[0].definition_id.0.starts_with("draft.leaf:"));
        // require_unique_slot_ids is enforced by validate_plan downstream.
        assert!(out.plan.validation.require_unique_slot_ids);
        // Provenance carries the AuthoringScript payload.
        let payload = out.plan.provenance.authoring_script_payload.as_ref().unwrap();
        assert_eq!(payload["kind"], "semantic_assembly");
        assert_eq!(payload["assembly_type"], "test_assembly");
    }

    #[test]
    fn assembly_adapter_indexes_duplicate_role_slots() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Wall Trio".into(),
            replace_source: false,
            provenance: PromotionProvenance::default(),
        };
        let input = assembly_input_for(vec![
            member(1, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(3, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(4, "roof", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        let out = adapter.build_plan_and_diff(input).unwrap();
        let slots = match &out.plan.output_shape {
            PromotionOutputShape::Compound { child_slots } => child_slots,
            other => panic!("expected compound plan, got {other:?}"),
        };
        let slot_ids: Vec<&str> = slots.iter().map(|s| s.slot_id.as_str()).collect();
        assert_eq!(slot_ids, vec!["wall_1", "wall_2", "wall_3", "roof"]);
        let warning_count = out
            .migration_diff
            .warnings
            .iter()
            .filter(|w| matches!(w, MigrationWarning::DuplicateRoleIndexed { .. }))
            .count();
        assert_eq!(warning_count, 1);
    }

    #[test]
    fn assembly_adapter_with_replace_source_requests_full_preservation() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Replaced".into(),
            replace_source: true,
            provenance: PromotionProvenance::default(),
        };
        let input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "leaf", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        let out = adapter.build_plan_and_diff(input).unwrap();
        assert_eq!(
            out.plan.source_replacement,
            SourceReplacementPolicy::ReplaceWithOccurrence {
                preserve_assembly_wrapper: true,
            }
        );
        assert_eq!(
            out.plan.element_id_preservation.mode,
            ElementIdPreservationMode::PreserveWherePossible
        );
        assert_eq!(
            out.plan
                .element_id_preservation
                .source_element_to_slot_realization,
            vec![(elem(1), "frame".into()), (elem(2), "leaf".into())],
        );
        // Self-retarget appears in the diff.
        assert!(out
            .migration_diff
            .retargeted_assemblies
            .iter()
            .any(|r| r.assembly_id == elem(100)));
    }

    #[test]
    fn assembly_adapter_records_external_membership_retargets() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "House".into(),
            replace_source: true,
            provenance: PromotionProvenance::default(),
        };
        let mut input = assembly_input_for(vec![
            member(1, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "wall", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        // External assembly references the first wall.
        input.external_graph.memberships.push(ExternalAssemblyMembership {
            assembly_id: elem(200),
            member_targets: vec![elem(1)],
        });
        // External relation touches the second wall.
        input.external_graph.relations.push(ExternalRelation {
            relation_id: elem(300),
            source: elem(2),
            target: elem(999), // unrelated outsider
            relation_type: "supports".into(),
        });
        let out = adapter.build_plan_and_diff(input).unwrap();
        // Self-retarget + external retarget.
        assert_eq!(out.migration_diff.retargeted_assemblies.len(), 2);
        let external = out
            .migration_diff
            .retargeted_assemblies
            .iter()
            .find(|r| r.assembly_id == elem(200))
            .unwrap();
        assert_eq!(external.from_members, vec![elem(1)]);
        assert_eq!(external.to_slot_ids, vec!["wall_1".to_string()]);
        assert_eq!(out.migration_diff.retargeted_relations.len(), 1);
        assert_eq!(out.migration_diff.retargeted_relations[0].relation_id, elem(300));
    }

    #[test]
    fn assembly_adapter_warns_on_outdated_capability_projection() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Skinny".into(),
            replace_source: false,
            provenance: PromotionProvenance::default(),
        };
        let mut input = assembly_input_for(vec![member(
            1,
            "wall",
            AssemblyMemberKind::AuthoredEntity,
            None,
        )]);
        input.capability.descriptor_id = None;
        let out = adapter.build_plan_and_diff(input).unwrap();
        assert!(out
            .migration_diff
            .warnings
            .iter()
            .any(|w| matches!(w, MigrationWarning::CapabilityProjectionOutdated { .. })));
    }

    #[test]
    fn assembly_adapter_plan_runs_through_default_emitter_with_compound_body() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Window".into(),
            replace_source: false,
            provenance: PromotionProvenance {
                agent: Some("test-suite".into()),
                ..Default::default()
            },
        };
        let input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "glazing", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        let plan = adapter.build_plan(input).unwrap();
        let plan_draft_id = plan.draft_id.clone();

        let mut drafts = DefinitionDraftRegistry::default();
        let existing = HashSet::<ElementId>::new();
        let mut emitter =
            DefaultPromotionDraftEmitter::new(&mut drafts, &existing, |plan: &PromotionPlan| {
                let mut def = crate::plugins::definition_authoring::blank_definition("Window");
                if let PromotionOutputShape::Compound { child_slots } = &plan.output_shape {
                    let compound = crate::plugins::modeling::definition::CompoundDefinition {
                        child_slots: child_slots.clone(),
                        ..Default::default()
                    };
                    def.compound = Some(compound);
                }
                Ok(def)
            });
        let record = emitter.emit(plan).unwrap();
        assert_eq!(record.draft_id, plan_draft_id);
        assert!(record.blockers.is_empty());

        let stored = drafts.get(&plan_draft_id).unwrap();
        let compound = stored
            .working_copy
            .compound
            .as_ref()
            .expect("compound body");
        assert_eq!(compound.child_slots.len(), 2);
        assert_eq!(compound.child_slots[0].slot_id, "frame");
        assert_eq!(compound.child_slots[1].slot_id, "glazing");
    }

    // === PP-A2DB-2 slice A: relation templates =============================

    fn internal_relation(
        relation_id: u64,
        source: ElementId,
        target: ElementId,
        relation_type: &str,
    ) -> InternalRelationSnapshot {
        InternalRelationSnapshot {
            relation_id: elem(relation_id),
            source,
            target,
            relation_type: relation_type.to_string(),
            parameters: serde_json::Value::Null,
        }
    }

    #[test]
    fn assembly_adapter_lifts_member_to_member_relation_into_template() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Door".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let mut input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "leaf", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        input.internal_relations.push(internal_relation(
            30,
            elem(1),
            elem(2),
            "hinges_on",
        ));
        let out = adapter.build_plan_and_diff(input).unwrap();

        assert_eq!(out.migration_diff.candidate_relation_templates.len(), 1);
        let template = &out.migration_diff.candidate_relation_templates[0];
        assert_eq!(template.subject, RelationEndpoint::Slot("frame".into()));
        assert_eq!(template.object, RelationEndpoint::Slot("leaf".into()));
        assert_eq!(template.relation_type, "hinges_on");
        assert_eq!(template.source_relation_id, elem(30));
        assert!(out.migration_diff.preserved_relations.is_empty());
    }

    #[test]
    fn assembly_adapter_lifts_self_to_member_relation_into_template() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Window".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let mut input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        // The source assembly itself is the relation's source endpoint
        // (as if the assembly "contains" the frame).
        input.internal_relations.push(internal_relation(
            30,
            input.assembly_id,
            elem(1),
            "contains",
        ));
        let out = adapter.build_plan_and_diff(input).unwrap();

        assert_eq!(out.migration_diff.candidate_relation_templates.len(), 1);
        let template = &out.migration_diff.candidate_relation_templates[0];
        assert_eq!(template.subject, RelationEndpoint::SelfRoot);
        assert_eq!(template.object, RelationEndpoint::Slot("frame".into()));
    }

    #[test]
    fn assembly_adapter_preserves_relation_when_endpoint_is_outside_source() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Door".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let mut input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        // The relation has one endpoint inside the source (frame=elem(1))
        // and one endpoint that is neither the assembly itself nor a
        // source member (elem(99)). It cannot be lifted into a template
        // and lands in `preserved_relations`.
        input.internal_relations.push(internal_relation(
            31,
            elem(1),
            elem(99),
            "anchored_to",
        ));
        let out = adapter.build_plan_and_diff(input).unwrap();

        assert!(out.migration_diff.candidate_relation_templates.is_empty());
        assert_eq!(out.migration_diff.preserved_relations.len(), 1);
        let preserved = &out.migration_diff.preserved_relations[0];
        assert_eq!(preserved.relation_id, elem(31));
        assert_eq!(preserved.source, elem(1));
        assert_eq!(preserved.target, elem(99));
        assert_eq!(preserved.relation_type, "anchored_to");
    }

    #[test]
    fn assembly_adapter_uses_indexed_slot_id_for_template_endpoint() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Walls".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let mut input = assembly_input_for(vec![
            member(1, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "wall", AssemblyMemberKind::AuthoredEntity, None),
            member(3, "wall", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        // Relation between wall #1 and wall #3 — the indexed slot id
        // (`wall_1` / `wall_3`) must be the template's endpoint
        // address, NOT the role.
        input.internal_relations.push(internal_relation(
            40,
            elem(1),
            elem(3),
            "adjacent_to",
        ));
        let out = adapter.build_plan_and_diff(input).unwrap();

        let template = &out.migration_diff.candidate_relation_templates[0];
        assert_eq!(template.subject, RelationEndpoint::Slot("wall_1".into()));
        assert_eq!(template.object, RelationEndpoint::Slot("wall_3".into()));
    }

    #[test]
    fn assembly_adapter_carries_relation_parameters_verbatim() {
        let adapter = SemanticAssemblyPromotionSource {
            name: "Door".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let mut input = assembly_input_for(vec![
            member(1, "frame", AssemblyMemberKind::AuthoredEntity, None),
            member(2, "leaf", AssemblyMemberKind::AuthoredEntity, None),
        ]);
        input.internal_relations.push(InternalRelationSnapshot {
            relation_id: elem(30),
            source: elem(1),
            target: elem(2),
            relation_type: "hinges_on".into(),
            parameters: serde_json::json!({ "hinge_count": 3, "axis": "y" }),
        });
        let out = adapter.build_plan_and_diff(input).unwrap();

        let template = &out.migration_diff.candidate_relation_templates[0];
        assert_eq!(template.parameters["hinge_count"], serde_json::json!(3));
        assert_eq!(template.parameters["axis"], serde_json::json!("y"));
    }

    #[test]
    fn migration_diff_relation_fields_round_trip_through_serde() {
        let diff = SemanticGraphMigrationDiff {
            retargeted_assemblies: Vec::new(),
            retargeted_relations: Vec::new(),
            orphaned_memberships: Vec::new(),
            candidate_relation_templates: vec![SemanticRelationTemplate {
                subject: RelationEndpoint::SelfRoot,
                relation_type: "contains".into(),
                object: RelationEndpoint::Slot("frame".into()),
                parameters: serde_json::json!({ "k": "v" }),
                source_relation_id: elem(30),
            }],
            preserved_relations: vec![PreservedRelation {
                relation_id: elem(31),
                source: elem(1),
                target: elem(99),
                relation_type: "anchored_to".into(),
                parameters: serde_json::Value::Null,
            }],
            warnings: Vec::new(),
        };
        let json = serde_json::to_string(&diff).unwrap();
        let back: SemanticGraphMigrationDiff = serde_json::from_str(&json).unwrap();
        assert_eq!(back.candidate_relation_templates.len(), 1);
        assert_eq!(back.preserved_relations.len(), 1);
        assert_eq!(
            back.candidate_relation_templates[0].subject,
            RelationEndpoint::SelfRoot
        );
        assert_eq!(
            back.candidate_relation_templates[0].object,
            RelationEndpoint::Slot("frame".into())
        );
    }
}
