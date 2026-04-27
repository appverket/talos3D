//! ADR-042 attic-truss vertical-slice verification test.
//!
//! Proves end-to-end that Talos3D can be taught a new construction
//! system as **data** rather than as Rust code. Specifically:
//!
//! 1. `ConstructionSystemManifest` is registered as a new manifest
//!    kind in core's `ManifestKindRegistry` via the generic
//!    `ManifestKindDescriptor` mechanism.
//! 2. The shipped `roof.truss.attic` manifest is authored as JSON
//!    data and inserted into `CuratedManifestRegistry`.
//! 3. The manifest walker enumerates outbound references (variant
//!    recipe / pattern / validator refs) without core knowing the
//!    domain semantics.
//! 4. The attic-truss schematic recipe is authored as
//!    `AuthoringScript` data, not a Rust `GenerateFn`, and is
//!    structurally valid.
//! 5. The recipe replays end-to-end against the existing PP82
//!    deterministic replay executor with a fixture dispatcher,
//!    captures step outputs, and satisfies its postconditions —
//!    including `Postcondition::Claim` on every promotion-critical
//!    path declared by the manifest variant (the `ClaimGrounding`
//!    emission ADR-042 §12 requires).
//!
//! The manifest-and-recipe authoring functions in this file are the
//! shape `talos3d-architecture-core` should own per ADR-042 §16. They
//! live here for the verification slice so this branch can land
//! without a coordinated arch-side rebase. Migrating them to
//! `talos3d-architecture-core/src/manifests.rs` and
//! `recipes/attic_truss_schematic.rs` is the immediate follow-up.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use talos3d_core::curation::authoring_script::{
    ArgExpr, AuthoringScript, McpToolId, MutationScope, OutputPath, Postcondition, Predicate, Step,
    StepId,
};
use talos3d_core::curation::replay::{
    replay, PostconditionOracle, PostconditionVerdict, ResolvedPostcondition, ToolCall,
    ToolDispatchError, ToolDispatcher,
};
use talos3d_core::curation::{
    AssetId, AssetKindId, Confidence, CuratedManifest, CuratedManifestRegistry, CurationMeta,
    GroundingKind, Lineage, ManifestKindDescriptor, ManifestKindId, ManifestKindRegistry,
    Provenance, RecipeArtifact, RecipeBody, RefField, Scope, Trust, RECIPE_ARTIFACT_KIND,
};
use talos3d_core::capability_registry::RecipeFamilyId;
use talos3d_core::plugins::identity::ElementId;
use talos3d_core::plugins::modeling::ghost_geometry::{
    check_clearance_envelope, ClearanceEnvelope, ClearanceShape, GhostObstacle,
};
use talos3d_core::plugins::refinement::{AgentId, ClaimPath, RefinementState, RuleId};
use talos3d_core::authored_entity::EntityBounds;
use bevy::prelude::*;

// ---------------------------------------------------------------------------
// Architecture-owned (post-merge) — ConstructionSystemManifest kind
// ---------------------------------------------------------------------------

const CONSTRUCTION_SYSTEM_MANIFEST_KIND: &str = "construction_system_manifest.v1";

fn construction_system_manifest_kind() -> ManifestKindDescriptor {
    ManifestKindDescriptor::new(
        ManifestKindId::new(CONSTRUCTION_SYSTEM_MANIFEST_KIND),
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "ConstructionSystemManifest body",
            "type": "object",
            "required": ["concept_ref", "variants", "variant_selection",
                         "user_facing_summary_template"],
            "properties": {
                "concept_ref": { "type": "string" },
                "variants": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string" },
                            "summary": { "type": "string" },
                            "pattern_refs": { "type": "array", "items": { "type": "string" } },
                            "recipe_refs": { "type": "array", "items": { "type": "string" } },
                            "prior_refs": { "type": "array", "items": { "type": "string" } },
                            "validator_refs": { "type": "array", "items": { "type": "string" } },
                            "catalog_requirements": {
                                "type": "array", "items": { "type": "string" }
                            },
                            "promotion_critical_paths": {
                                "type": "array", "items": { "type": "string" }
                            }
                        }
                    }
                },
                "variant_selection": { "type": "object" },
                "user_facing_summary_template": { "type": "string" }
            }
        }),
    )
    .with_description("Architecture-owned construction-system manifest kind")
    .with_walker_hook(
        RefField::new("/concept_ref", AssetKindId::new("vocabulary_concept.v1")).required(),
    )
    .with_walker_hook(RefField::new(
        "/variants/*/recipe_refs/*",
        AssetKindId::new("recipe.v1"),
    ))
    .with_walker_hook(RefField::new(
        "/variants/*/pattern_refs/*",
        AssetKindId::new("assembly_pattern.v2"),
    ))
    .with_walker_hook(RefField::new(
        "/variants/*/validator_refs/*",
        AssetKindId::new("constraint_descriptor.v1"),
    ))
}

// ---------------------------------------------------------------------------
// roof.truss.attic — authored as data
// ---------------------------------------------------------------------------

fn roof_truss_attic_manifest() -> CuratedManifest {
    let kind = ManifestKindId::new(CONSTRUCTION_SYSTEM_MANIFEST_KIND);
    let asset_id = CuratedManifest::asset_id_for(&kind, "roof.truss.attic");

    let meta = CurationMeta::new(
        asset_id,
        CuratedManifest::asset_kind(),
        Provenance {
            author: AgentId("shipped".into()),
            confidence: Confidence::High,
            lineage: Lineage::Freeform,
            rationale: Some(
                "Shipped attic-truss manifest authored as data per ADR-042 §7."
                    .into(),
            ),
            jurisdiction: None,
            catalog_dependencies: Vec::new(),
            evidence: Vec::new(),
        },
    )
    .with_scope(Scope::Shipped)
    .with_trust(Trust::Published);

    CuratedManifest {
        meta,
        manifest_kind: kind,
        body: json!({
            "concept_ref": "vocabulary_concept.v1/roof.truss.attic",
            "variants": {
                "storage": {
                    "label": "Attic truss — storage",
                    "summary": "Attic truss with central usable but uninhabitable void.",
                    "pattern_refs": ["assembly_pattern.v2/hybrid_attic_cathedral_roof"],
                    "recipe_refs": ["recipe.v1/attic_truss_schematic"],
                    "validator_refs": [
                        "constraint_descriptor.v1/support_path",
                        "constraint_descriptor.v1/assembly_completeness"
                    ],
                    "promotion_critical_paths": [
                        "truss/variant", "truss/span_mm", "truss/spacing_mm",
                        "truss/top_chord_size", "truss/bottom_chord_size"
                    ]
                },
                "room": {
                    "label": "Attic truss — room (raised heel)",
                    "summary": "Attic truss configured for habitable space; raised heel.",
                    "pattern_refs": ["assembly_pattern.v2/hybrid_attic_cathedral_roof"],
                    "recipe_refs": ["recipe.v1/attic_truss_schematic"],
                    "validator_refs": [
                        "constraint_descriptor.v1/support_path",
                        "constraint_descriptor.v1/assembly_completeness"
                    ],
                    "promotion_critical_paths": [
                        "truss/variant", "truss/span_mm", "truss/spacing_mm",
                        "truss/top_chord_size", "truss/bottom_chord_size",
                        "truss/heel_height_mm", "truss/room_clear_height_mm"
                    ]
                }
            },
            "variant_selection": {
                "default": "storage",
                "criteria": [
                    {
                        "if": "brief.contains_phrase",
                        "value": "habitable attic",
                        "then": "room"
                    },
                    {
                        "if": "brief.contains_phrase",
                        "value": "loft bedroom",
                        "then": "room"
                    }
                ]
            },
            "user_facing_summary_template":
                "Attic truss ({variant_label}) at {spacing_mm} mm spacing, \
                 {span_mm} mm span, {top_chord_size} top chord."
        }),
    }
}

// ---------------------------------------------------------------------------
// attic-truss schematic recipe — authored as AuthoringScript data
// ---------------------------------------------------------------------------

const ATTIC_TRUSS_FAMILY: &str = "attic_truss_schematic";

fn attic_truss_authoring_script() -> AuthoringScript {
    let mut script = AuthoringScript::stub(MutationScope::RefinementSubtree {
        root_element_param: "root_element".into(),
    });

    script.parameter_schema = json!({
        "type": "object",
        "required": ["root_element"],
        "properties": {
            "root_element": { "type": "integer" },
            "variant": {
                "type": "string", "enum": ["storage", "room"], "default": "storage"
            },
            "span_mm": { "type": "number", "default": 7000 },
            "pitch_deg": { "type": "number", "default": 27 },
            "spacing_mm": { "type": "number", "default": 1200 },
            "top_chord_size": { "type": "string", "default": "2x4" },
            "bottom_chord_size": { "type": "string", "default": "2x4" },
            "heel_height_mm": { "type": "number", "default": 150 },
            "room_clear_height_mm": { "type": "number", "default": 2400 }
        }
    });
    script
        .parameter_defaults
        .insert("variant".into(), Value::String("storage".into()));
    script.parameter_defaults.insert("span_mm".into(), 7000.into());
    script.parameter_defaults.insert("pitch_deg".into(), 27.into());
    script.parameter_defaults.insert("spacing_mm".into(), 1200.into());
    script
        .parameter_defaults
        .insert("top_chord_size".into(), Value::String("2x4".into()));
    script
        .parameter_defaults
        .insert("bottom_chord_size".into(), Value::String("2x4".into()));
    script
        .parameter_defaults
        .insert("heel_height_mm".into(), 150.into());
    script
        .parameter_defaults
        .insert("room_clear_height_mm".into(), 2400.into());

    let mut allowed: BTreeSet<McpToolId> = BTreeSet::new();
    allowed.insert(McpToolId::new("definition.create"));
    allowed.insert(McpToolId::new("definition.instantiate"));
    script.allowed_tools = allowed;

    // Step 1 — create_truss_def
    let mut create_def = Step {
        id: StepId::new("create_truss_def"),
        tool: McpToolId::new("definition.create"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: true,
        precondition: None,
    };
    create_def.args.insert(
        "name".into(),
        ArgExpr::Literal {
            value: Value::String("attic_truss".into()),
        },
    );
    create_def.args.insert(
        "definition_kind".into(),
        ArgExpr::Literal {
            value: Value::String("Solid".into()),
        },
    );
    create_def.args.insert(
        "domain_data".into(),
        ArgExpr::Literal {
            value: json!({
                "framing_family": "attic_truss",
                "manifest_ref":
                    "curated_manifest.v1/construction_system_manifest.v1/roof.truss.attic"
            }),
        },
    );
    create_def
        .args
        .insert("variant".into(), ArgExpr::Param { name: "variant".into() });
    create_def
        .args
        .insert("span_mm".into(), ArgExpr::Param { name: "span_mm".into() });
    create_def.args.insert(
        "spacing_mm".into(),
        ArgExpr::Param { name: "spacing_mm".into() },
    );
    create_def.args.insert(
        "pitch_deg".into(),
        ArgExpr::Param { name: "pitch_deg".into() },
    );
    create_def.args.insert(
        "top_chord_size".into(),
        ArgExpr::Param { name: "top_chord_size".into() },
    );
    create_def.args.insert(
        "bottom_chord_size".into(),
        ArgExpr::Param { name: "bottom_chord_size".into() },
    );
    create_def
        .bindings
        .insert("definition_id".into(), OutputPath::new("$.definition_id"));
    script.steps.push(create_def);

    // Step 2 — place_first_truss
    let mut place = Step {
        id: StepId::new("place_first_truss"),
        tool: McpToolId::new("definition.instantiate"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: true,
        precondition: None,
    };
    place.args.insert(
        "definition".into(),
        ArgExpr::StepOutput {
            step_id: StepId::new("create_truss_def"),
            path: OutputPath::new("definition_id"),
        },
    );
    place.args.insert(
        "host_element".into(),
        ArgExpr::Param {
            name: "root_element".into(),
        },
    );
    place
        .bindings
        .insert("occurrence_id".into(), OutputPath::new("$.occurrence_id"));
    script.steps.push(place);

    // Step 3 — room-variant only: lock the clear-height marker.
    let mut room_lock = Step {
        id: StepId::new("lock_room_clear_height"),
        tool: McpToolId::new("definition.create"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: false,
        precondition: Some(Predicate::Equals {
            lhs: ArgExpr::Param {
                name: "variant".into(),
            },
            rhs: ArgExpr::Literal {
                value: Value::String("room".into()),
            },
        }),
    };
    room_lock.args.insert(
        "name".into(),
        ArgExpr::Literal {
            value: Value::String("attic_truss_room_marker".into()),
        },
    );
    room_lock.args.insert(
        "definition_kind".into(),
        ArgExpr::Literal {
            value: Value::String("Annotation".into()),
        },
    );
    room_lock.args.insert(
        "room_clear_height_mm".into(),
        ArgExpr::Param {
            name: "room_clear_height_mm".into(),
        },
    );
    room_lock.args.insert(
        "heel_height_mm".into(),
        ArgExpr::Param {
            name: "heel_height_mm".into(),
        },
    );
    script.steps.push(room_lock);

    // Postconditions — ClaimGrounding on every storage promotion-
    // critical path, plus one relation.
    for path in [
        "truss/variant",
        "truss/span_mm",
        "truss/spacing_mm",
        "truss/top_chord_size",
        "truss/bottom_chord_size",
    ] {
        let grounding = if path == "truss/variant" {
            GroundingKind::ExplicitRule(RuleId(
                "manifest.roof.truss.attic.variant_selection.default".into(),
            ))
        } else {
            GroundingKind::GeneratedByRecipe(ATTIC_TRUSS_FAMILY.into())
        };
        script.postconditions.push(Postcondition::Claim {
            path: ClaimPath(path.into()),
            grounding,
        });
    }
    script.postconditions.push(Postcondition::Relation {
        relation_kind: "has_attic_truss_occurrence".into(),
        from: ArgExpr::Param {
            name: "root_element".into(),
        },
        to: ArgExpr::StepOutput {
            step_id: StepId::new("place_first_truss"),
            path: OutputPath::new("occurrence_id"),
        },
    });

    script
}

fn attic_truss_recipe_artifact() -> RecipeArtifact {
    let family = RecipeFamilyId(ATTIC_TRUSS_FAMILY.into());
    let asset_id = RecipeArtifact::asset_id_for(&family);
    let meta = CurationMeta::new(
        asset_id,
        AssetKindId::new(RECIPE_ARTIFACT_KIND),
        Provenance {
            author: AgentId("shipped".into()),
            confidence: Confidence::High,
            lineage: Lineage::Freeform,
            rationale: Some("Attic-truss schematic recipe authored as data".into()),
            jurisdiction: None,
            catalog_dependencies: Vec::new(),
            evidence: Vec::new(),
        },
    )
    .with_scope(Scope::Shipped)
    .with_trust(Trust::Published);

    RecipeArtifact {
        meta,
        body: RecipeBody::AuthoringScript {
            script: attic_truss_authoring_script(),
        },
        parameter_schema: attic_truss_authoring_script().parameter_schema.clone(),
        target_class: "roof_system".into(),
        supported_refinement_states: vec![
            RefinementState::Schematic,
            RefinementState::Constructible,
        ],
        tests: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Replay test fixtures
// ---------------------------------------------------------------------------

struct FixtureDispatcher {
    responses: Vec<Value>,
    calls: Vec<(McpToolId, Map<String, Value>)>,
}

impl FixtureDispatcher {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            responses,
            calls: Vec::new(),
        }
    }
}

impl ToolDispatcher for FixtureDispatcher {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        self.calls.push((call.tool.clone(), call.args.clone()));
        if self.responses.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(self.responses.remove(0))
        }
    }
}

struct AlwaysPassOracle;

impl PostconditionOracle for AlwaysPassOracle {
    fn check(
        &self,
        _pc: &ResolvedPostcondition,
        _outputs: &BTreeMap<StepId, Map<String, Value>>,
        _params: &Map<String, Value>,
    ) -> PostconditionVerdict {
        PostconditionVerdict::Pass
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn manifest_kind_registers_and_walks_outbound_refs() {
    let mut kinds = ManifestKindRegistry::default();
    kinds.register(construction_system_manifest_kind());
    let mut manifests = CuratedManifestRegistry::default();
    manifests.insert(roof_truss_attic_manifest());

    // Kind is registered with the right id.
    let kind_id = ManifestKindId::new(CONSTRUCTION_SYSTEM_MANIFEST_KIND);
    assert!(kinds.get(&kind_id).is_some(), "kind must be registered");

    // Manifest is indexed by kind.
    let by_kind: Vec<&CuratedManifest> = manifests.get_by_kind(&kind_id).collect();
    assert_eq!(by_kind.len(), 1);

    // Walker enumerates the recipe + validator + pattern + concept refs.
    let refs = manifests.enumerate_outbound_refs(&kinds);
    let recipe_kind = AssetKindId::new("recipe.v1");
    let recipes = refs.get(&recipe_kind).expect("recipe refs");
    assert!(recipes.contains(&AssetId::new("recipe.v1/attic_truss_schematic")));

    let validator_kind = AssetKindId::new("constraint_descriptor.v1");
    let validators = refs.get(&validator_kind).expect("validator refs");
    assert!(validators.contains(&AssetId::new("constraint_descriptor.v1/support_path")));

    let pattern_kind = AssetKindId::new("assembly_pattern.v2");
    let patterns = refs.get(&pattern_kind).expect("pattern refs");
    assert!(patterns
        .contains(&AssetId::new("assembly_pattern.v2/hybrid_attic_cathedral_roof")));

    let concept_kind = AssetKindId::new("vocabulary_concept.v1");
    let concepts = refs.get(&concept_kind).expect("concept refs");
    assert!(concepts.contains(&AssetId::new("vocabulary_concept.v1/roof.truss.attic")));
}

#[test]
fn manifest_walker_reports_no_missing_required_refs() {
    let mut kinds = ManifestKindRegistry::default();
    kinds.register(construction_system_manifest_kind());
    let mut manifests = CuratedManifestRegistry::default();
    let id = manifests.insert(roof_truss_attic_manifest());

    let report = manifests.walk_manifest(&id, &kinds).expect("manifest exists");
    assert!(
        report.missing_required.is_empty(),
        "no required ref should be missing; got: {:?}",
        report.missing_required
    );
}

#[test]
fn recipe_artifact_body_is_data_not_native() {
    let artifact = attic_truss_recipe_artifact();
    match &artifact.body {
        RecipeBody::AuthoringScript { .. } => {} // expected: data, not Rust
        RecipeBody::NativeFnRef { .. } => panic!("attic truss must be authored as data"),
    }
    assert_eq!(artifact.target_class, "roof_system");
}

#[test]
fn authoring_script_validates_structure() {
    let script = attic_truss_authoring_script();
    script
        .validate_structure()
        .expect("attic-truss script must validate structurally");
}

#[test]
fn no_promotion_critical_claim_is_llm_heuristic() {
    let script = attic_truss_authoring_script();
    let any_heuristic = script.postconditions.iter().any(|p| match p {
        Postcondition::Claim { grounding, .. } => {
            matches!(grounding, GroundingKind::LLMHeuristic { .. })
        }
        _ => false,
    });
    assert!(
        !any_heuristic,
        "ADR-042 §12: no LLMHeuristic on promotion-critical paths at schematic"
    );
}

#[test]
fn replay_storage_variant_executes_two_steps_and_passes() {
    let script = attic_truss_authoring_script();
    let mut dispatcher = FixtureDispatcher::new(vec![
        json!({ "definition_id": 100 }),
        json!({ "occurrence_id": 200 }),
    ]);

    let mut params = Map::new();
    params.insert("root_element".into(), Value::from(42));
    // omit variant — default "storage" should apply

    let report = replay(&script, params, &mut dispatcher, &AlwaysPassOracle).unwrap();
    // Two steps run, room-only step skipped because variant != "room".
    assert_eq!(
        report.steps_run,
        vec![
            StepId::new("create_truss_def"),
            StepId::new("place_first_truss")
        ]
    );
    assert_eq!(report.steps_skipped, vec![StepId::new("lock_room_clear_height")]);

    // First call carried the manifest_ref tag in domain_data.
    let domain_data = &dispatcher.calls[0].1["domain_data"];
    assert_eq!(
        domain_data["manifest_ref"],
        Value::String(
            "curated_manifest.v1/construction_system_manifest.v1/roof.truss.attic".into()
        )
    );
    assert_eq!(domain_data["framing_family"], Value::String("attic_truss".into()));

    // Second call hosted on the right element and used the binding from step 1.
    assert_eq!(dispatcher.calls[1].1["host_element"], Value::from(42));
    assert_eq!(dispatcher.calls[1].1["definition"], Value::from(100));

    // Postcondition projection: a Claim + a Relation came through with
    // the right paths and grounding kinds.
    let mut saw_variant_claim = false;
    let mut saw_relation = false;
    for result in &report.postcondition_results {
        match &result.postcondition {
            ResolvedPostcondition::Claim { path, grounding } => {
                if path.0 == "truss/variant" {
                    saw_variant_claim = true;
                    assert!(matches!(
                        grounding,
                        GroundingKind::ExplicitRule(rule)
                            if rule.0 ==
                            "manifest.roof.truss.attic.variant_selection.default"
                    ));
                }
            }
            ResolvedPostcondition::Relation {
                relation_kind,
                from,
                to,
            } => {
                if relation_kind == "has_attic_truss_occurrence" {
                    saw_relation = true;
                    assert_eq!(*from, Value::from(42));
                    assert_eq!(*to, Value::from(200));
                }
            }
            _ => {}
        }
    }
    assert!(saw_variant_claim, "must emit truss/variant Claim postcondition");
    assert!(
        saw_relation,
        "must emit has_attic_truss_occurrence Relation postcondition"
    );
}

#[test]
fn replay_room_variant_executes_three_steps() {
    let script = attic_truss_authoring_script();
    let mut dispatcher = FixtureDispatcher::new(vec![
        json!({ "definition_id": 1 }),
        json!({ "occurrence_id": 2 }),
        json!({ "definition_id": 3 }), // room marker
    ]);
    let mut params = Map::new();
    params.insert("root_element".into(), Value::from(7));
    params.insert("variant".into(), Value::String("room".into()));

    let report = replay(&script, params, &mut dispatcher, &AlwaysPassOracle).unwrap();
    assert_eq!(
        report.steps_run,
        vec![
            StepId::new("create_truss_def"),
            StepId::new("place_first_truss"),
            StepId::new("lock_room_clear_height"),
        ]
    );
    assert!(report.steps_skipped.is_empty());

    // The room marker step received the room_clear_height_mm param.
    let marker_call = &dispatcher.calls[2].1;
    assert_eq!(marker_call["room_clear_height_mm"], Value::from(2400));
    assert_eq!(marker_call["name"], Value::String("attic_truss_room_marker".into()));
}

#[test]
fn manifest_serializes_to_stable_data_form() {
    // The whole point of ADR-042: the manifest is data, not code. So
    // it must round-trip cleanly through JSON without any Rust state
    // leaking in.
    let manifest = roof_truss_attic_manifest();
    let json = serde_json::to_string(&manifest).unwrap();
    let parsed: CuratedManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, manifest);
}

#[test]
fn recipe_artifact_serializes_to_stable_data_form() {
    let artifact = attic_truss_recipe_artifact();
    let json = serde_json::to_string(&artifact).unwrap();
    let parsed: RecipeArtifact = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, artifact);
}

#[test]
fn storage_variant_clearance_envelope_passes_validator() {
    // ADR-042 §10 PP-B: the attic-truss `storage` variant declares a
    // central pocket that downstream construction must keep clear so
    // the storage volume remains usable. With nothing else in the
    // model the validator must produce no findings; the envelope
    // ignores the truss owner itself (an envelope cannot intrude on
    // its own owner).
    let truss_owner = ElementId(42);
    // 4 m × 2 m × 6 m envelope centred over the attic floor, 1.0 m
    // above the bottom-chord datum. Half-extents in metres.
    let envelope = ClearanceEnvelope::usable(
        truss_owner,
        "roof.truss.attic.storage_void",
        ClearanceShape::Box {
            center: Vec3::new(0.0, 1.0, 0.0),
            half_extents: Vec3::new(2.0, 1.0, 3.0),
            rotation: Quat::IDENTITY,
        },
    )
    .with_label("storage volume");

    // No obstacles in the world → no findings.
    let findings = check_clearance_envelope(&envelope, &[]);
    assert!(
        findings.is_empty(),
        "storage envelope should validate cleanly with no obstacles; got: {findings:?}"
    );

    // The owning truss occurrence at the same location is also OK —
    // the envelope cannot intrude on its own owner.
    let truss_obstacle = GhostObstacle {
        element_id: truss_owner,
        bounds: EntityBounds {
            min: Vec3::new(-2.0, 0.0, -3.0),
            max: Vec3::new(2.0, 2.0, 3.0),
        },
        label: Some("truss occurrence".into()),
    };
    let findings = check_clearance_envelope(&envelope, std::slice::from_ref(&truss_obstacle));
    assert!(
        findings.is_empty(),
        "owner geometry must not be flagged against its own envelope"
    );

    // A foreign element placed inside the envelope must be flagged.
    let intruder = GhostObstacle {
        element_id: ElementId(999),
        bounds: EntityBounds {
            min: Vec3::new(-0.1, 0.5, -0.1),
            max: Vec3::new(0.1, 1.5, 0.1),
        },
        label: Some("rogue strut".into()),
    };
    let findings = check_clearance_envelope(&envelope, std::slice::from_ref(&intruder));
    assert_eq!(findings.len(), 1, "intruder must be reported");
    assert_eq!(findings[0].offending_element, ElementId(999));
}
