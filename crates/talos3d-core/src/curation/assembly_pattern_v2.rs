//! `AssemblyPatternV2` — typed body for ADR-042 §9 assembly patterns.
//!
//! This is the recipes-as-data successor to the legacy
//! [`AssemblyPatternDescriptor`](crate::capability_registry::AssemblyPatternDescriptor)
//! shape. The legacy descriptor stays valid; v2 lives alongside as a
//! [`CuratedManifest`](crate::curation::manifests::CuratedManifest) body
//! authored under the [`ASSEMBLY_PATTERN_V2_KIND`] manifest kind.
//!
//! The body carries six v2 elements per ADR-042 §9:
//!
//! - **roles** — declared role tags consumed by domain validators.
//! - **slots** — ordered named members of the pattern with role + multiplicity.
//! - **envelopes** — negative-space declarations (kind/constraint, no geometry).
//! - **relations** — required graph relations between slots.
//! - **variants** — named overrides + recipe / validator / obligation refs.
//! - **obligations** — class-minimum obligation templates per role.
//!
//! Core validates only the generic graph shape (slot id uniqueness, relation
//! source/target reference existing slots, envelope owner_slot references an
//! existing slot, variant overrides reference existing slots / envelopes).
//! Domain validators interpret the role names and concrete envelope semantics.
//!
//! A pattern body can be authored typed (via [`AssemblyPatternV2`] +
//! [`AssemblyPatternV2::to_manifest_body`]) or raw (as `serde_json::Value`).
//! Either way, the generic graph-shape check runs through
//! [`validate_pattern_v2_body`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::identity::AssetKindId;
use super::manifests::{ManifestKindDescriptor, ManifestKindId, RefField};
use crate::plugins::modeling::ghost_geometry::{
    ClearanceConstraint, ClearanceKind, LoadPathRequirement,
};
use crate::plugins::refinement::{ObligationId, RefinementState, SemanticRole};

/// Stable identifier of the v2 assembly-pattern manifest kind.
pub const ASSEMBLY_PATTERN_V2_KIND: &str = "assembly_pattern.v2";

/// Schema version embedded in the body for forward compatibility.
pub const ASSEMBLY_PATTERN_V2_SCHEMA_VERSION: u32 = 1;

/// How many entities may fill a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum SlotMultiplicity {
    /// Exactly one occupant required.
    One,
    /// Zero or one occupant.
    Optional,
    /// Zero or more occupants.
    Many,
    /// One or more occupants.
    OneOrMore,
}

impl SlotMultiplicity {
    pub fn allows_zero(self) -> bool {
        matches!(self, Self::Optional | Self::Many)
    }
}

/// One ordered named member within an assembly pattern.
///
/// `slot_id` must be unique within the pattern. `role` is a domain-defined
/// tag (e.g. `"weather"`, `"bearing"`, `"attic_void"`); core does not
/// interpret the value but enforces uniqueness at the pattern level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternSlot {
    pub slot_id: String,
    pub label: String,
    pub role: SemanticRole,
    pub multiplicity: SlotMultiplicity,
    /// Optional capability hint (e.g. a material family or product
    /// family slug). Domain validators may consume this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_hint: Option<String>,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A negative-space declaration owned by a slot.
///
/// The pattern declares that the slot must, when materialized, emit a
/// [`ClearanceEnvelope`](crate::plugins::modeling::ghost_geometry::ClearanceEnvelope)
/// of the given kind + constraint. Concrete geometry is the recipe's
/// responsibility — the pattern only carries kind / constraint / a stable
/// name so validators can refer to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternEnvelope {
    pub envelope_id: String,
    /// Slot whose materialization must spawn this envelope.
    pub owner_slot_id: String,
    pub kind: ClearanceKind,
    pub constraint: ClearanceConstraint,
    /// Human-readable label, e.g. `"central storage pocket"`.
    pub label: String,
    /// Optional concept asset id this envelope is anchored in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_concept_ref: Option<String>,
}

/// A support-corridor declaration between two slot anchors.
///
/// `start_slot_id` and `end_slot_id` reference [`PatternSlot::slot_id`]
/// values within the same pattern. Concrete face anchors are filled in
/// per occurrence by recipes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternSupportCorridor {
    pub corridor_id: String,
    pub start_slot_id: String,
    pub end_slot_id: String,
    pub required_load_path: LoadPathRequirement,
    pub label: String,
    /// Optional concept asset id this corridor is anchored in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_concept_ref: Option<String>,
}

/// A required graph relation between two slots in the pattern.
///
/// The `relation_type` is the same string surface as
/// [`crate::capability_registry::RelationTypeDescriptor::relation_type`].
/// Core enforces only that `source_slot_id` / `target_slot_id` resolve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternRelation {
    pub relation_type: String,
    pub source_slot_id: String,
    pub target_slot_id: String,
    pub required: bool,
    pub rationale: String,
}

/// A class-minimum obligation template attached to a role.
///
/// Materialized onto the entity at promotion time; identical to the
/// existing [`ObligationTemplate`](crate::capability_registry::ObligationTemplate)
/// shape so consumers can reuse the obligation pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternObligation {
    pub id: ObligationId,
    pub role: SemanticRole,
    pub required_by_state: RefinementState,
}

/// Per-variant override of a slot.
///
/// Variants may opt out of a slot, switch its multiplicity, or supply a
/// variant-specific material hint. The override target must reference a
/// `slot_id` declared on the parent pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternSlotOverride {
    pub slot_id: String,
    /// If `Some`, replaces the parent slot's multiplicity for this
    /// variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplicity: Option<SlotMultiplicity>,
    /// If `Some` (and `enabled` is `true`), replaces the parent slot's
    /// material hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_hint: Option<String>,
    /// `false` = the variant explicitly omits this slot. The slot is
    /// still required to exist on the parent pattern.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// One named variant of the pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PatternVariant {
    pub variant_id: String,
    pub label: String,
    /// Slot overrides keyed by `slot_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slot_overrides: Vec<PatternSlotOverride>,
    /// Envelope ids enabled in this variant. If empty, all parent
    /// envelopes are inherited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub envelope_ids: Vec<String>,
    /// Recipe asset ids that materialize this variant
    /// (e.g. `"recipe.v1/attic_truss_storage_schematic"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipe_refs: Vec<String>,
    /// Validation pack asset ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validator_refs: Vec<String>,
    /// Obligation template ids consumed at promotion (resolved against
    /// the pattern's `obligations` list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligation_refs: Vec<String>,
}

/// The typed body of an [`ASSEMBLY_PATTERN_V2_KIND`] manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssemblyPatternV2 {
    pub schema_version: u32,
    pub pattern_id: String,
    pub label: String,
    pub description: String,
    /// Concept asset id the pattern is anchored to (e.g.
    /// `"vocabulary_concept.v1/wall_assembly"`). Walker emits this as
    /// a required outbound ref.
    pub concept_ref: String,
    /// Free-form orientation hint (`"exterior_to_interior"`,
    /// `"top_to_bottom"`, etc.).
    pub axis: String,
    pub slots: Vec<PatternSlot>,
    pub envelopes: Vec<PatternEnvelope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub support_corridors: Vec<PatternSupportCorridor>,
    pub relations: Vec<PatternRelation>,
    pub obligations: Vec<PatternObligation>,
    pub variants: Vec<PatternVariant>,
    /// Capability-defined tags (climate / jurisdiction hints, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl AssemblyPatternV2 {
    /// Convenience constructor with required fields. All collection
    /// fields default to empty.
    pub fn new(
        pattern_id: impl Into<String>,
        label: impl Into<String>,
        concept_ref: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: ASSEMBLY_PATTERN_V2_SCHEMA_VERSION,
            pattern_id: pattern_id.into(),
            label: label.into(),
            description: String::new(),
            concept_ref: concept_ref.into(),
            axis: String::new(),
            slots: Vec::new(),
            envelopes: Vec::new(),
            support_corridors: Vec::new(),
            relations: Vec::new(),
            obligations: Vec::new(),
            variants: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// Serialize to JSON for the [`CuratedManifest::body`] field.
    pub fn to_manifest_body(&self) -> Value {
        serde_json::to_value(self).expect("AssemblyPatternV2 is JSON-serializable")
    }

    /// Deserialize from a [`CuratedManifest::body`].
    pub fn from_manifest_body(body: &Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(body.clone())
    }
}

// ---------------------------------------------------------------------------
// Manifest kind registration
// ---------------------------------------------------------------------------

/// Build the [`ManifestKindDescriptor`] for the v2 assembly-pattern kind.
///
/// Walker hooks declare:
///
/// - `/concept_ref` → `vocabulary_concept.v1` (required)
/// - `/variants/*/recipe_refs/*` → `recipe.v1`
/// - `/variants/*/validator_refs/*` → `validation_pack.v1`
/// - `/envelopes/*/owner_concept_ref` → `vocabulary_concept.v1`
/// - `/support_corridors/*/owner_concept_ref` → `vocabulary_concept.v1`
pub fn assembly_pattern_v2_manifest_kind() -> ManifestKindDescriptor {
    ManifestKindDescriptor::new(
        ManifestKindId::new(ASSEMBLY_PATTERN_V2_KIND),
        body_schema(),
    )
    .with_walker_hook(
        RefField::new(
            "/concept_ref",
            AssetKindId::new("vocabulary_concept.v1"),
        )
        .required(),
    )
    .with_walker_hook(RefField::new(
        "/variants/*/recipe_refs/*",
        AssetKindId::new("recipe.v1"),
    ))
    .with_walker_hook(RefField::new(
        "/variants/*/validator_refs/*",
        AssetKindId::new("validation_pack.v1"),
    ))
    .with_walker_hook(RefField::new(
        "/envelopes/*/owner_concept_ref",
        AssetKindId::new("vocabulary_concept.v1"),
    ))
    .with_walker_hook(RefField::new(
        "/support_corridors/*/owner_concept_ref",
        AssetKindId::new("vocabulary_concept.v1"),
    ))
    .with_description(
        "Generic assembly-pattern v2 contract — roles, slots, envelopes, \
         relations, variants, obligations. Per ADR-042 §9.",
    )
}

/// JSON Schema sketch for the v2 body. Used today as documentation
/// shipped with the descriptor; runtime body validation is performed by
/// [`validate_pattern_v2_body`] in addition to (eventually) JSON-Schema
/// validation when the kernel grows that pass.
fn body_schema() -> Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "AssemblyPatternV2",
        "type": "object",
        "required": [
            "schema_version",
            "pattern_id",
            "label",
            "concept_ref",
            "slots",
            "envelopes",
            "relations",
            "obligations",
            "variants"
        ],
        "properties": {
            "schema_version": { "type": "integer", "minimum": 1 },
            "pattern_id": { "type": "string", "minLength": 1 },
            "label": { "type": "string" },
            "description": { "type": "string" },
            "concept_ref": { "type": "string", "minLength": 1 },
            "axis": { "type": "string" },
            "slots": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["slot_id", "label", "role", "multiplicity"]
                }
            },
            "envelopes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["envelope_id", "owner_slot_id", "kind", "constraint", "label"]
                }
            },
            "support_corridors": { "type": "array" },
            "relations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["relation_type", "source_slot_id", "target_slot_id", "required", "rationale"]
                }
            },
            "obligations": { "type": "array" },
            "variants": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["variant_id", "label"]
                }
            },
            "tags": { "type": "array", "items": { "type": "string" } }
        }
    })
}

// ---------------------------------------------------------------------------
// Generic graph-shape validator
// ---------------------------------------------------------------------------

/// One structural problem found in a v2 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssemblyPatternV2Error {
    UnsupportedSchemaVersion(u32),
    EmptyPatternId,
    EmptyConceptRef,
    DuplicateSlotId(String),
    DuplicateEnvelopeId(String),
    DuplicateCorridorId(String),
    DuplicateVariantId(String),
    DuplicateObligationId(String),
    EnvelopeOwnerSlotNotFound {
        envelope_id: String,
        slot_id: String,
    },
    CorridorAnchorSlotNotFound {
        corridor_id: String,
        slot_id: String,
    },
    RelationSourceSlotNotFound {
        relation_type: String,
        slot_id: String,
    },
    RelationTargetSlotNotFound {
        relation_type: String,
        slot_id: String,
    },
    VariantSlotOverrideTargetNotFound {
        variant_id: String,
        slot_id: String,
    },
    VariantEnvelopeIdNotFound {
        variant_id: String,
        envelope_id: String,
    },
    VariantObligationRefNotFound {
        variant_id: String,
        obligation_id: String,
    },
    BodyDeserialization(String),
}

impl std::fmt::Display for AssemblyPatternV2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(v) => {
                write!(f, "unsupported schema_version {v} (kernel knows {ASSEMBLY_PATTERN_V2_SCHEMA_VERSION})")
            }
            Self::EmptyPatternId => f.write_str("pattern_id must be non-empty"),
            Self::EmptyConceptRef => f.write_str("concept_ref must be non-empty"),
            Self::DuplicateSlotId(id) => write!(f, "duplicate slot_id `{id}`"),
            Self::DuplicateEnvelopeId(id) => write!(f, "duplicate envelope_id `{id}`"),
            Self::DuplicateCorridorId(id) => write!(f, "duplicate corridor_id `{id}`"),
            Self::DuplicateVariantId(id) => write!(f, "duplicate variant_id `{id}`"),
            Self::DuplicateObligationId(id) => write!(f, "duplicate obligation id `{id}`"),
            Self::EnvelopeOwnerSlotNotFound {
                envelope_id,
                slot_id,
            } => write!(
                f,
                "envelope `{envelope_id}` owner_slot_id `{slot_id}` does not match any slot"
            ),
            Self::CorridorAnchorSlotNotFound {
                corridor_id,
                slot_id,
            } => write!(
                f,
                "support_corridor `{corridor_id}` anchor `{slot_id}` does not match any slot"
            ),
            Self::RelationSourceSlotNotFound {
                relation_type,
                slot_id,
            } => write!(
                f,
                "relation `{relation_type}` source_slot_id `{slot_id}` does not match any slot"
            ),
            Self::RelationTargetSlotNotFound {
                relation_type,
                slot_id,
            } => write!(
                f,
                "relation `{relation_type}` target_slot_id `{slot_id}` does not match any slot"
            ),
            Self::VariantSlotOverrideTargetNotFound {
                variant_id,
                slot_id,
            } => write!(
                f,
                "variant `{variant_id}` slot override targets unknown slot `{slot_id}`"
            ),
            Self::VariantEnvelopeIdNotFound {
                variant_id,
                envelope_id,
            } => write!(
                f,
                "variant `{variant_id}` enables unknown envelope `{envelope_id}`"
            ),
            Self::VariantObligationRefNotFound {
                variant_id,
                obligation_id,
            } => write!(
                f,
                "variant `{variant_id}` references unknown obligation `{obligation_id}`"
            ),
            Self::BodyDeserialization(msg) => {
                write!(f, "body did not deserialize as AssemblyPatternV2: {msg}")
            }
        }
    }
}

impl std::error::Error for AssemblyPatternV2Error {}

/// Generic graph-shape validator. Returns the *full* list of problems
/// rather than short-circuiting on the first — recipe-authoring tools
/// surface the entire batch.
pub fn validate_pattern_v2(pattern: &AssemblyPatternV2) -> Vec<AssemblyPatternV2Error> {
    use std::collections::HashSet;

    let mut errors = Vec::new();

    if pattern.schema_version != ASSEMBLY_PATTERN_V2_SCHEMA_VERSION {
        errors.push(AssemblyPatternV2Error::UnsupportedSchemaVersion(
            pattern.schema_version,
        ));
    }
    if pattern.pattern_id.trim().is_empty() {
        errors.push(AssemblyPatternV2Error::EmptyPatternId);
    }
    if pattern.concept_ref.trim().is_empty() {
        errors.push(AssemblyPatternV2Error::EmptyConceptRef);
    }

    let mut slot_ids: HashSet<&str> = HashSet::new();
    for slot in &pattern.slots {
        if !slot_ids.insert(slot.slot_id.as_str()) {
            errors.push(AssemblyPatternV2Error::DuplicateSlotId(slot.slot_id.clone()));
        }
    }

    let mut envelope_ids: HashSet<&str> = HashSet::new();
    for envelope in &pattern.envelopes {
        if !envelope_ids.insert(envelope.envelope_id.as_str()) {
            errors.push(AssemblyPatternV2Error::DuplicateEnvelopeId(
                envelope.envelope_id.clone(),
            ));
        }
        if !slot_ids.contains(envelope.owner_slot_id.as_str()) {
            errors.push(AssemblyPatternV2Error::EnvelopeOwnerSlotNotFound {
                envelope_id: envelope.envelope_id.clone(),
                slot_id: envelope.owner_slot_id.clone(),
            });
        }
    }

    let mut corridor_ids: HashSet<&str> = HashSet::new();
    for corridor in &pattern.support_corridors {
        if !corridor_ids.insert(corridor.corridor_id.as_str()) {
            errors.push(AssemblyPatternV2Error::DuplicateCorridorId(
                corridor.corridor_id.clone(),
            ));
        }
        if !slot_ids.contains(corridor.start_slot_id.as_str()) {
            errors.push(AssemblyPatternV2Error::CorridorAnchorSlotNotFound {
                corridor_id: corridor.corridor_id.clone(),
                slot_id: corridor.start_slot_id.clone(),
            });
        }
        if !slot_ids.contains(corridor.end_slot_id.as_str()) {
            errors.push(AssemblyPatternV2Error::CorridorAnchorSlotNotFound {
                corridor_id: corridor.corridor_id.clone(),
                slot_id: corridor.end_slot_id.clone(),
            });
        }
    }

    for relation in &pattern.relations {
        if !slot_ids.contains(relation.source_slot_id.as_str()) {
            errors.push(AssemblyPatternV2Error::RelationSourceSlotNotFound {
                relation_type: relation.relation_type.clone(),
                slot_id: relation.source_slot_id.clone(),
            });
        }
        if !slot_ids.contains(relation.target_slot_id.as_str()) {
            errors.push(AssemblyPatternV2Error::RelationTargetSlotNotFound {
                relation_type: relation.relation_type.clone(),
                slot_id: relation.target_slot_id.clone(),
            });
        }
    }

    let mut obligation_ids: HashSet<&str> = HashSet::new();
    for obligation in &pattern.obligations {
        if !obligation_ids.insert(obligation.id.0.as_str()) {
            errors.push(AssemblyPatternV2Error::DuplicateObligationId(
                obligation.id.0.clone(),
            ));
        }
    }

    let mut variant_ids: HashSet<&str> = HashSet::new();
    for variant in &pattern.variants {
        if !variant_ids.insert(variant.variant_id.as_str()) {
            errors.push(AssemblyPatternV2Error::DuplicateVariantId(
                variant.variant_id.clone(),
            ));
        }
        for ovr in &variant.slot_overrides {
            if !slot_ids.contains(ovr.slot_id.as_str()) {
                errors.push(AssemblyPatternV2Error::VariantSlotOverrideTargetNotFound {
                    variant_id: variant.variant_id.clone(),
                    slot_id: ovr.slot_id.clone(),
                });
            }
        }
        for env_id in &variant.envelope_ids {
            if !envelope_ids.contains(env_id.as_str()) {
                errors.push(AssemblyPatternV2Error::VariantEnvelopeIdNotFound {
                    variant_id: variant.variant_id.clone(),
                    envelope_id: env_id.clone(),
                });
            }
        }
        for ob_id in &variant.obligation_refs {
            if !obligation_ids.contains(ob_id.as_str()) {
                errors.push(AssemblyPatternV2Error::VariantObligationRefNotFound {
                    variant_id: variant.variant_id.clone(),
                    obligation_id: ob_id.clone(),
                });
            }
        }
    }

    errors
}

/// Validate a raw [`CuratedManifest::body`] as a v2 assembly pattern.
/// Combines deserialization and the graph-shape pass.
pub fn validate_pattern_v2_body(body: &Value) -> Result<AssemblyPatternV2, Vec<AssemblyPatternV2Error>> {
    let pattern = AssemblyPatternV2::from_manifest_body(body)
        .map_err(|err| vec![AssemblyPatternV2Error::BodyDeserialization(err.to_string())])?;
    let errors = validate_pattern_v2(&pattern);
    if errors.is_empty() {
        Ok(pattern)
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::manifests::{
        CuratedManifest, CuratedManifestRegistry, ManifestKindRegistry,
    };
    use crate::curation::meta::CurationMeta;
    use crate::curation::provenance::{Confidence, Lineage, Provenance};
    use crate::curation::scope_trust::{Scope, Trust};
    use crate::plugins::refinement::AgentId;

    fn slot(id: &str, role: &str, mult: SlotMultiplicity) -> PatternSlot {
        PatternSlot {
            slot_id: id.into(),
            label: id.into(),
            role: SemanticRole(role.into()),
            multiplicity: mult,
            material_hint: None,
            description: None,
        }
    }

    fn relation(rt: &str, src: &str, tgt: &str) -> PatternRelation {
        PatternRelation {
            relation_type: rt.into(),
            source_slot_id: src.into(),
            target_slot_id: tgt.into(),
            required: true,
            rationale: "test".into(),
        }
    }

    fn sample_pattern() -> AssemblyPatternV2 {
        let mut p = AssemblyPatternV2::new(
            "wall.exterior.light_frame.v2",
            "Light-frame exterior wall (v2)",
            "vocabulary_concept.v1/wall_assembly",
        );
        p.axis = "exterior_to_interior".into();
        p.slots = vec![
            slot("cladding", "weather", SlotMultiplicity::One),
            slot("sheathing", "bracing", SlotMultiplicity::One),
            slot("framing", "primary_structure", SlotMultiplicity::One),
            slot("interior_finish", "interior", SlotMultiplicity::Optional),
        ];
        p.envelopes = vec![PatternEnvelope {
            envelope_id: "service_chase".into(),
            owner_slot_id: "framing".into(),
            kind: ClearanceKind::Service,
            constraint: ClearanceConstraint::NoIntersect,
            label: "Service chase between studs".into(),
            owner_concept_ref: Some("vocabulary_concept.v1/service_chase".into()),
        }];
        p.support_corridors = vec![PatternSupportCorridor {
            corridor_id: "vertical_load".into(),
            start_slot_id: "framing".into(),
            end_slot_id: "framing".into(),
            required_load_path: LoadPathRequirement::Continuous,
            label: "Stud-to-plate load path".into(),
            owner_concept_ref: None,
        }];
        p.relations = vec![
            relation("rests_on", "cladding", "sheathing"),
            relation("rests_on", "sheathing", "framing"),
        ];
        p.obligations = vec![PatternObligation {
            id: ObligationId("primary_structure_resolved".into()),
            role: SemanticRole("primary_structure".into()),
            required_by_state: RefinementState::Schematic,
        }];
        p.variants = vec![PatternVariant {
            variant_id: "default".into(),
            label: "Default 2x4 stud wall".into(),
            slot_overrides: Vec::new(),
            envelope_ids: vec!["service_chase".into()],
            recipe_refs: vec!["recipe.v1/wall_light_frame_exterior".into()],
            validator_refs: vec!["validation_pack.v1/wall_light_frame_smoke".into()],
            obligation_refs: vec!["primary_structure_resolved".into()],
        }];
        p.tags = vec!["climate.cold".into()];
        p
    }

    #[test]
    fn kind_constant_matches() {
        let descriptor = assembly_pattern_v2_manifest_kind();
        assert_eq!(descriptor.kind_id.as_str(), ASSEMBLY_PATTERN_V2_KIND);
    }

    #[test]
    fn kind_descriptor_declares_required_walker_hooks() {
        let d = assembly_pattern_v2_manifest_kind();
        let concept_hook = d
            .walker_hooks
            .iter()
            .find(|h| h.path == "/concept_ref")
            .expect("concept_ref hook present");
        assert!(concept_hook.required, "concept_ref must be a required ref");
        assert_eq!(
            concept_hook.target_kind.as_str(),
            "vocabulary_concept.v1"
        );
        let recipe_hook = d
            .walker_hooks
            .iter()
            .find(|h| h.path == "/variants/*/recipe_refs/*")
            .expect("recipe walker hook present");
        assert_eq!(recipe_hook.target_kind.as_str(), "recipe.v1");
    }

    #[test]
    fn pattern_round_trips_through_manifest_body() {
        let pattern = sample_pattern();
        let body = pattern.to_manifest_body();
        let parsed = AssemblyPatternV2::from_manifest_body(&body).unwrap();
        assert_eq!(parsed, pattern);
    }

    #[test]
    fn valid_pattern_passes_validator() {
        let pattern = sample_pattern();
        let errors = validate_pattern_v2(&pattern);
        assert!(errors.is_empty(), "expected no errors, got {errors:?}");
    }

    #[test]
    fn duplicate_slot_id_is_reported() {
        let mut p = sample_pattern();
        p.slots.push(slot("framing", "primary_structure", SlotMultiplicity::One));
        let errors = validate_pattern_v2(&p);
        assert!(matches!(
            errors.first(),
            Some(AssemblyPatternV2Error::DuplicateSlotId(id)) if id == "framing"
        ));
    }

    #[test]
    fn relation_referencing_unknown_slot_is_reported() {
        let mut p = sample_pattern();
        p.relations.push(relation("rests_on", "ghost", "framing"));
        let errors = validate_pattern_v2(&p);
        assert!(errors.iter().any(|e| matches!(
            e,
            AssemblyPatternV2Error::RelationSourceSlotNotFound { slot_id, .. }
                if slot_id == "ghost"
        )));
    }

    #[test]
    fn envelope_with_unknown_owner_slot_is_reported() {
        let mut p = sample_pattern();
        p.envelopes.push(PatternEnvelope {
            envelope_id: "rogue".into(),
            owner_slot_id: "ghost_slot".into(),
            kind: ClearanceKind::Usable,
            constraint: ClearanceConstraint::NoIntersect,
            label: "rogue".into(),
            owner_concept_ref: None,
        });
        let errors = validate_pattern_v2(&p);
        assert!(errors.iter().any(|e| matches!(
            e,
            AssemblyPatternV2Error::EnvelopeOwnerSlotNotFound { envelope_id, .. }
                if envelope_id == "rogue"
        )));
    }

    #[test]
    fn corridor_with_unknown_anchor_is_reported() {
        let mut p = sample_pattern();
        p.support_corridors.push(PatternSupportCorridor {
            corridor_id: "broken".into(),
            start_slot_id: "ghost".into(),
            end_slot_id: "framing".into(),
            required_load_path: LoadPathRequirement::Continuous,
            label: "broken".into(),
            owner_concept_ref: None,
        });
        let errors = validate_pattern_v2(&p);
        assert!(errors.iter().any(|e| matches!(
            e,
            AssemblyPatternV2Error::CorridorAnchorSlotNotFound { corridor_id, slot_id }
                if corridor_id == "broken" && slot_id == "ghost"
        )));
    }

    #[test]
    fn variant_slot_override_to_unknown_slot_is_reported() {
        let mut p = sample_pattern();
        p.variants[0].slot_overrides.push(PatternSlotOverride {
            slot_id: "phantom".into(),
            multiplicity: Some(SlotMultiplicity::Optional),
            material_hint: None,
            enabled: true,
        });
        let errors = validate_pattern_v2(&p);
        assert!(errors.iter().any(|e| matches!(
            e,
            AssemblyPatternV2Error::VariantSlotOverrideTargetNotFound { slot_id, .. }
                if slot_id == "phantom"
        )));
    }

    #[test]
    fn variant_envelope_ref_to_unknown_envelope_is_reported() {
        let mut p = sample_pattern();
        p.variants[0].envelope_ids.push("nope".into());
        let errors = validate_pattern_v2(&p);
        assert!(errors.iter().any(|e| matches!(
            e,
            AssemblyPatternV2Error::VariantEnvelopeIdNotFound { envelope_id, .. }
                if envelope_id == "nope"
        )));
    }

    #[test]
    fn variant_obligation_ref_to_unknown_obligation_is_reported() {
        let mut p = sample_pattern();
        p.variants[0].obligation_refs.push("nope".into());
        let errors = validate_pattern_v2(&p);
        assert!(errors.iter().any(|e| matches!(
            e,
            AssemblyPatternV2Error::VariantObligationRefNotFound { obligation_id, .. }
                if obligation_id == "nope"
        )));
    }

    #[test]
    fn duplicate_envelope_id_is_reported() {
        let mut p = sample_pattern();
        p.envelopes.push(PatternEnvelope {
            envelope_id: "service_chase".into(),
            owner_slot_id: "framing".into(),
            kind: ClearanceKind::Service,
            constraint: ClearanceConstraint::NoIntersect,
            label: "duplicate".into(),
            owner_concept_ref: None,
        });
        let errors = validate_pattern_v2(&p);
        assert!(matches!(
            errors.first(),
            Some(AssemblyPatternV2Error::DuplicateEnvelopeId(id)) if id == "service_chase"
        ));
    }

    #[test]
    fn empty_pattern_id_is_reported() {
        let mut p = sample_pattern();
        p.pattern_id = String::new();
        let errors = validate_pattern_v2(&p);
        assert!(errors
            .iter()
            .any(|e| matches!(e, AssemblyPatternV2Error::EmptyPatternId)));
    }

    #[test]
    fn unsupported_schema_version_is_reported() {
        let mut p = sample_pattern();
        p.schema_version = 999;
        let errors = validate_pattern_v2(&p);
        assert!(errors
            .iter()
            .any(|e| matches!(e, AssemblyPatternV2Error::UnsupportedSchemaVersion(999))));
    }

    #[test]
    fn validate_body_round_trip_succeeds() {
        let body = sample_pattern().to_manifest_body();
        let parsed = validate_pattern_v2_body(&body).expect("valid body");
        assert_eq!(parsed.pattern_id, "wall.exterior.light_frame.v2");
    }

    #[test]
    fn validate_body_with_unparseable_json_fails() {
        let body = serde_json::json!({"not_a_pattern": true});
        let result = validate_pattern_v2_body(&body);
        let errors = result.expect_err("expected deserialization error");
        assert!(matches!(
            errors.first(),
            Some(AssemblyPatternV2Error::BodyDeserialization(_))
        ));
    }

    #[test]
    fn walker_enumerates_recipe_refs_through_curated_manifest_registry() {
        let mut kinds = ManifestKindRegistry::default();
        kinds.register(assembly_pattern_v2_manifest_kind());

        let mut manifests = CuratedManifestRegistry::default();
        let kind_id = ManifestKindId::new(ASSEMBLY_PATTERN_V2_KIND);
        let manifest = CuratedManifest {
            meta: CurationMeta::new(
                CuratedManifest::asset_id_for(&kind_id, "wall.exterior.light_frame.v2"),
                CuratedManifest::asset_kind(),
                Provenance {
                    author: AgentId("test".into()),
                    confidence: Confidence::High,
                    lineage: Lineage::Freeform,
                    rationale: None,
                    jurisdiction: None,
                    catalog_dependencies: Vec::new(),
                    evidence: Vec::new(),
                },
            )
            .with_scope(Scope::Project)
            .with_trust(Trust::Draft),
            manifest_kind: kind_id,
            body: sample_pattern().to_manifest_body(),
        };
        manifests.insert(manifest);

        let refs = manifests.enumerate_outbound_refs(&kinds);
        let recipe_kind = AssetKindId::new("recipe.v1");
        let recipe_refs = refs.get(&recipe_kind).expect("recipe refs walked");
        assert!(recipe_refs.iter().any(|id| id.as_str()
            == "recipe.v1/wall_light_frame_exterior"));

        let concept_kind = AssetKindId::new("vocabulary_concept.v1");
        let concept_refs = refs.get(&concept_kind).expect("concept refs walked");
        assert!(concept_refs
            .iter()
            .any(|id| id.as_str() == "vocabulary_concept.v1/wall_assembly"));

        let validator_kind = AssetKindId::new("validation_pack.v1");
        let validator_refs = refs.get(&validator_kind).expect("validator refs walked");
        assert!(validator_refs
            .iter()
            .any(|id| id.as_str() == "validation_pack.v1/wall_light_frame_smoke"));
    }
}
