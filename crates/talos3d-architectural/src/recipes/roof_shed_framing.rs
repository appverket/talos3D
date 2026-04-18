//! Registration of the `roof_system` element class and the
//! `shed_roof_framing` recipe family (PP73).
//!
//! All architectural nouns live here per ADR-037.  The generic machinery
//! (`ElementClassDescriptor`, `RecipeFamilyDescriptor`, `GenerateFn`, …) lives
//! in `talos3d-core`.
//!
//! Scope notes (PP73 spec):
//! - No actual rafter geometry — each obligation-satisfying child is a stub entity.
//! - The `bears_on` obligation is resolved automatically by `apply_promote_refinement`
//!   when a `bears_on` SemanticRelation from this roof into a Constructible+ wall
//!   (with a `top_datum_mm` ClaimGrounding entry) exists. If no such relation
//!   exists, the obligation stays Unresolved and the validator emits a finding.
//! - Generation priors for `style_intent: modern_pavilion` are a stub comment;
//!   the real `GenerationPriorDescriptor` mechanism lands in PP76.

use std::{collections::HashMap, sync::Arc};

use bevy::prelude::*;
use talos3d_core::{
    capability_registry::{
        ElementClassDescriptor, ElementClassId, GenerateFn, GenerateInput, GenerateOutput,
        ObligationTemplate, RecipeFamilyDescriptor, RecipeFamilyId, RecipeParameter,
    },
    plugins::{
        identity::{ElementId, ElementIdAllocator},
        refinement::{
            ClaimPath, ObligationId, RefinementState, SemanticRole,
            create_refinement_relation_pair,
        },
    },
};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Build the `roof_system` `ElementClassDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
/// Both `shed_roof_framing` and any future hip/gable recipes target this class.
pub fn roof_system_class() -> ElementClassDescriptor {
    let mut class_min_obligations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    // At Constructible, five obligations must be resolved.
    class_min_obligations.insert(
        RefinementState::Constructible,
        vec![
            ObligationTemplate {
                id: ObligationId("primary_framing".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("sheathing".into()),
                role: SemanticRole("exterior_envelope".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("underlayment".into()),
                role: SemanticRole("exterior_envelope".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("finish".into()),
                role: SemanticRole("exterior_envelope".into()),
                required_by_state: RefinementState::Constructible,
            },
            // PP73: bears_on is a class-min obligation resolved via an existing
            // `bears_on` SemanticRelation into a Constructible+ wall/foundation
            // with a `top_datum_mm` ClaimGrounding entry.
            ObligationTemplate {
                id: ObligationId("bears_on".into()),
                role: SemanticRole("load_path".into()),
                required_by_state: RefinementState::Constructible,
            },
        ],
    );

    let mut class_min_promotion_critical_paths: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();

    class_min_promotion_critical_paths.insert(
        RefinementState::Constructible,
        vec![
            ClaimPath("pitch_deg".into()),
            ClaimPath("high_plate_datum_mm".into()),
            ClaimPath("low_plate_datum_mm".into()),
            ClaimPath("footprint_polygon".into()),
        ],
    );

    ElementClassDescriptor {
        id: ElementClassId("roof_system".into()),
        label: "Roof System".into(),
        description: "A roof system element — carries precipitation loads and forms the \
            exterior weather envelope above the top story. May serve as primary structure \
            and exterior envelope depending on the recipe chosen."
            .into(),
        semantic_roles: vec![
            SemanticRole("primary_structure".into()),
            SemanticRole("exterior_envelope".into()),
        ],
        class_min_obligations,
        class_min_promotion_critical_paths,
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "footprint_polygon": {
                    "type": "array",
                    "description": "Array of [x, z] point pairs defining the roof footprint",
                    "items": {
                        "type": "array",
                        "items": {"type": "number"},
                        "minItems": 2,
                        "maxItems": 2
                    }
                },
                "pitch_deg": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": 89,
                    "description": "Roof pitch in degrees above horizontal"
                },
                "high_plate_datum_mm": {
                    "type": "number",
                    "description": "Elevation of the high-side top plate above project datum (mm)"
                },
                "low_plate_datum_mm": {
                    "type": "number",
                    "description": "Elevation of the low-side top plate above project datum (mm)"
                },
                "bears_on": {
                    "type": "string",
                    "description": "Element-id or anchor of the primary bearing wall/s"
                }
            }
        }),
    }
}

/// Build the `shed_roof_framing` `RecipeFamilyDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
///
/// TODO(PP76): When `style_intent: modern_pavilion` is present in the selection
/// context, this recipe should be ranked first via a `GenerationPriorDescriptor`.
pub fn shed_roof_framing_recipe() -> RecipeFamilyDescriptor {
    // The shed recipe adds no extra obligations beyond the five class-min
    // obligations. Rafter-level geometry specialization comes from the
    // Constructible critical-path additions below.
    let obligation_specializations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    let mut promotion_critical_path_specializations: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();

    // The recipe promotes rafter_spacing_mm and rafter_section to
    // promotion-critical at Constructible, on top of the four class-min paths.
    promotion_critical_path_specializations.insert(
        RefinementState::Constructible,
        vec![
            ClaimPath("rafter_spacing_mm".into()),
            ClaimPath("rafter_section".into()),
        ],
    );

    RecipeFamilyDescriptor {
        id: RecipeFamilyId("shed_roof_framing".into()),
        target_class: ElementClassId("roof_system".into()),
        label: "Shed Roof Framing".into(),
        description: "Single-slope (shed) roof with wood rafter framing, structural sheathing, \
            underlayment membrane, and finish roofing. Rafters run perpendicular to the \
            ridge/high-edge direction. Suited for modern pavilion and lean-to structures."
            .into(),
        parameters: vec![
            RecipeParameter {
                name: "footprint_polygon".into(),
                value_schema: serde_json::json!({
                    "type": "array",
                    "items": {"type": "array", "items": {"type": "number"}}
                }),
                default: None,
            },
            RecipeParameter {
                name: "pitch_deg".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 0, "maximum": 89}),
                default: Some(serde_json::json!(15)),
            },
            RecipeParameter {
                name: "high_edge_direction".into(),
                value_schema: serde_json::json!({
                    "type": "string",
                    "enum": ["N", "E", "S", "W"],
                    "description": "Cardinal direction of the high (ridge) edge"
                }),
                default: Some(serde_json::json!("W")),
            },
            RecipeParameter {
                name: "rafter_section".into(),
                value_schema: serde_json::json!({"type": "string"}),
                default: Some(serde_json::json!("2x10")),
            },
            RecipeParameter {
                name: "rafter_spacing_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 100}),
                default: Some(serde_json::json!(600)),
            },
            RecipeParameter {
                name: "sheathing_thickness_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 1}),
                default: Some(serde_json::json!(18)),
            },
            RecipeParameter {
                name: "overhang_eave_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 0}),
                default: Some(serde_json::json!(600)),
            },
            RecipeParameter {
                name: "overhang_rake_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 0}),
                default: Some(serde_json::json!(300)),
            },
        ],
        supported_refinement_levels: vec![
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
        ],
        obligation_specializations,
        promotion_critical_path_specializations,
        generate: build_shed_generate_fn(),
    }
}

// ---------------------------------------------------------------------------
// Generate function — shed_roof_framing
// ---------------------------------------------------------------------------

/// Merge recipe defaults with caller-supplied overrides.
fn resolve_shed_parameters(input: &GenerateInput) -> HashMap<String, serde_json::Value> {
    let defaults: &[(&str, serde_json::Value)] = &[
        ("pitch_deg", serde_json::json!(15)),
        ("high_edge_direction", serde_json::json!("W")),
        ("rafter_section", serde_json::json!("2x10")),
        ("rafter_spacing_mm", serde_json::json!(600)),
        ("sheathing_thickness_mm", serde_json::json!(18)),
        ("overhang_eave_mm", serde_json::json!(600)),
        ("overhang_rake_mm", serde_json::json!(300)),
    ];
    let mut params = input.parameters.clone();
    for (key, default_value) in defaults {
        params
            .entry((*key).to_string())
            .or_insert_with(|| default_value.clone());
    }
    params
}

/// Spawn a minimal child entity linked to the parent via a refinement pair.
fn spawn_child_stub(
    world: &mut World,
    parent_eid: ElementId,
    promoted_from_state: RefinementState,
    target_state: RefinementState,
) -> u64 {
    let child_eid = world.resource::<ElementIdAllocator>().next_id();
    world.spawn(child_eid);
    create_refinement_relation_pair(world, parent_eid, child_eid, promoted_from_state, target_state);
    child_eid.0
}

fn build_shed_generate_fn() -> GenerateFn {
    Arc::new(|input: GenerateInput, world: &mut World| -> Result<GenerateOutput, String> {
        let parent_eid = ElementId(input.element_id);
        let target_state = input.target_state;
        let _params = resolve_shed_parameters(&input);
        let promoted_from = RefinementState::Conceptual;

        let mut output = GenerateOutput::default();

        match target_state {
            RefinementState::Conceptual | RefinementState::Schematic => {
                // No children at these levels — obligations are installed by the
                // promote machinery from the merged contract but remain Unresolved
                // until Constructible promotion.
            }
            RefinementState::Constructible => {
                // Spawn four child stubs, one per class-min obligation (excluding
                // bears_on, which is resolved by the generic cross-entity mechanism
                // in apply_promote_refinement when a suitable SemanticRelation exists).

                let framing_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state,
                );
                output.satisfaction_links.push((ObligationId("primary_framing".into()), framing_eid));

                let sheathing_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state,
                );
                output.satisfaction_links.push((ObligationId("sheathing".into()), sheathing_eid));

                let underlayment_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state,
                );
                output.satisfaction_links.push((ObligationId("underlayment".into()), underlayment_eid));

                let finish_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state,
                );
                output.satisfaction_links.push((ObligationId("finish".into()), finish_eid));

                // `bears_on` is NOT spawned as a child stub — it is resolved by
                // the generic `apply_promote_refinement` logic when a `bears_on`
                // SemanticRelation into a Constructible+ wall exists. If no such
                // relation is present the obligation remains Unresolved.
            }
            _ => {
                return Err(format!(
                    "shed_roof_framing does not support target state: {}",
                    target_state.as_str()
                ));
            }
        }

        Ok(output)
    })
}

// ---------------------------------------------------------------------------
// Unit tests for the roof recipe module (PP73)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use talos3d_core::{
        capability_registry::{
            effective_obligations, effective_promotion_critical_paths, CapabilityRegistry,
        },
        plugins::refinement::RefinementState,
    };

    // -----------------------------------------------------------------------
    // Element class tests
    // -----------------------------------------------------------------------

    #[test]
    fn roof_class_has_five_constructible_class_min_obligations() {
        let class = roof_system_class();
        let obligations = class
            .class_min_obligations
            .get(&RefinementState::Constructible)
            .expect("obligations at Constructible");
        assert_eq!(obligations.len(), 5, "expected 5 class-min obligations");
        let ids: Vec<&str> = obligations.iter().map(|o| o.id.0.as_str()).collect();
        assert!(ids.contains(&"primary_framing"));
        assert!(ids.contains(&"sheathing"));
        assert!(ids.contains(&"underlayment"));
        assert!(ids.contains(&"finish"));
        assert!(ids.contains(&"bears_on"));
    }

    #[test]
    fn roof_class_has_four_constructible_promotion_critical_paths() {
        let class = roof_system_class();
        let paths = class
            .class_min_promotion_critical_paths
            .get(&RefinementState::Constructible)
            .expect("paths at Constructible");
        assert_eq!(paths.len(), 4);
        let strs: Vec<&str> = paths.iter().map(|p| p.0.as_str()).collect();
        assert!(strs.contains(&"pitch_deg"));
        assert!(strs.contains(&"high_plate_datum_mm"));
        assert!(strs.contains(&"low_plate_datum_mm"));
        assert!(strs.contains(&"footprint_polygon"));
    }

    #[test]
    fn roof_class_has_primary_structure_and_exterior_envelope_roles() {
        let class = roof_system_class();
        let roles: Vec<&str> = class.semantic_roles.iter().map(|r| r.0.as_str()).collect();
        assert!(roles.contains(&"primary_structure"));
        assert!(roles.contains(&"exterior_envelope"));
    }

    // -----------------------------------------------------------------------
    // Recipe descriptor tests
    // -----------------------------------------------------------------------

    #[test]
    fn shed_recipe_targets_roof_system_class() {
        let recipe = shed_roof_framing_recipe();
        assert_eq!(recipe.target_class.0, "roof_system");
        assert_eq!(recipe.id.0, "shed_roof_framing");
    }

    #[test]
    fn shed_recipe_supports_three_refinement_levels() {
        let recipe = shed_roof_framing_recipe();
        assert!(recipe
            .supported_refinement_levels
            .contains(&RefinementState::Conceptual));
        assert!(recipe
            .supported_refinement_levels
            .contains(&RefinementState::Schematic));
        assert!(recipe
            .supported_refinement_levels
            .contains(&RefinementState::Constructible));
        assert!(!recipe
            .supported_refinement_levels
            .contains(&RefinementState::Detailed));
    }

    #[test]
    fn shed_recipe_has_expected_parameters_with_defaults() {
        let recipe = shed_roof_framing_recipe();
        let param = |name: &str| {
            recipe
                .parameters
                .iter()
                .find(|p| p.name == name)
                .expect(name)
        };
        // Required (no default)
        assert!(param("footprint_polygon").default.is_none());
        // Defaulted
        assert_eq!(param("pitch_deg").default, Some(serde_json::json!(15)));
        assert_eq!(param("high_edge_direction").default, Some(serde_json::json!("W")));
        assert_eq!(param("rafter_section").default, Some(serde_json::json!("2x10")));
        assert_eq!(param("rafter_spacing_mm").default, Some(serde_json::json!(600)));
        assert_eq!(param("sheathing_thickness_mm").default, Some(serde_json::json!(18)));
        assert_eq!(param("overhang_eave_mm").default, Some(serde_json::json!(600)));
        assert_eq!(param("overhang_rake_mm").default, Some(serde_json::json!(300)));
    }

    #[test]
    fn shed_recipe_adds_rafter_paths_to_constructible_critical_paths() {
        let recipe = shed_roof_framing_recipe();
        let extra = recipe
            .promotion_critical_path_specializations
            .get(&RefinementState::Constructible)
            .expect("specializations at Constructible");
        assert_eq!(extra.len(), 2);
        let strs: Vec<&str> = extra.iter().map(|p| p.0.as_str()).collect();
        assert!(strs.contains(&"rafter_spacing_mm"));
        assert!(strs.contains(&"rafter_section"));
    }

    #[test]
    fn shed_recipe_adds_no_extra_obligations() {
        let recipe = shed_roof_framing_recipe();
        let specializations = recipe
            .obligation_specializations
            .get(&RefinementState::Constructible);
        // Either no entry at all, or an empty vec — either way zero extra obligations.
        let extra_count = specializations.map_or(0, |v| v.len());
        assert_eq!(extra_count, 0, "shed_roof_framing adds no obligation specializations");
    }

    // -----------------------------------------------------------------------
    // Effective merged contract tests
    // -----------------------------------------------------------------------

    #[test]
    fn effective_obligations_yields_five_at_constructible() {
        let class = roof_system_class();
        let recipe = shed_roof_framing_recipe();
        let obligations =
            effective_obligations(&class, Some(&recipe), RefinementState::Constructible);
        // 5 class-min + 0 recipe specializations = 5
        assert_eq!(
            obligations.len(),
            5,
            "5 class-min + 0 recipe specializations; got: {:?}",
            obligations.iter().map(|o| &o.id.0).collect::<Vec<_>>()
        );
    }

    #[test]
    fn effective_critical_paths_yields_six_at_constructible() {
        let class = roof_system_class();
        let recipe = shed_roof_framing_recipe();
        let paths = effective_promotion_critical_paths(
            &class,
            Some(&recipe),
            RefinementState::Constructible,
        );
        // 4 class-min + 2 recipe specializations = 6
        assert_eq!(paths.len(), 6);
        let strs: Vec<&str> = paths.iter().map(|p| p.0.as_str()).collect();
        assert!(strs.contains(&"pitch_deg"));
        assert!(strs.contains(&"footprint_polygon"));
        assert!(strs.contains(&"rafter_spacing_mm"));
        assert!(strs.contains(&"rafter_section"));
    }

    // -----------------------------------------------------------------------
    // Generate function tests
    // -----------------------------------------------------------------------

    fn make_test_world() -> World {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(CapabilityRegistry::default());
        use talos3d_core::plugins::{
            commands::{ApplyEntityChangesCommand, BeginCommandGroup, EndCommandGroup},
            history::{History, PendingCommandQueue},
        };
        use bevy::prelude::Messages;
        world.insert_resource(Messages::<BeginCommandGroup>::default());
        world.insert_resource(Messages::<EndCommandGroup>::default());
        world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(History::default());
        world
    }

    #[test]
    fn generate_conceptual_produces_no_links() {
        let mut world = make_test_world();
        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = shed_roof_framing_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Conceptual,
            parameters: HashMap::new(),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(output.satisfaction_links.is_empty());
    }

    #[test]
    fn generate_schematic_produces_no_links() {
        let mut world = make_test_world();
        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = shed_roof_framing_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Schematic,
            parameters: HashMap::new(),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(output.satisfaction_links.is_empty());
    }

    #[test]
    fn generate_constructible_produces_four_satisfaction_links() {
        let mut world = make_test_world();
        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = shed_roof_framing_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Constructible,
            parameters: HashMap::from([
                (
                    "footprint_polygon".into(),
                    serde_json::json!([[0,0],[6000,0],[6000,8000],[0,8000]]),
                ),
                ("pitch_deg".into(), serde_json::json!(15)),
            ]),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert_eq!(
            output.satisfaction_links.len(),
            4,
            "primary_framing + sheathing + underlayment + finish (bears_on resolved via relation); got: {:?}",
            output.satisfaction_links.iter().map(|(id, _)| &id.0).collect::<Vec<_>>()
        );
        let ids: Vec<&str> = output
            .satisfaction_links
            .iter()
            .map(|(id, _)| id.0.as_str())
            .collect();
        assert!(ids.contains(&"primary_framing"));
        assert!(ids.contains(&"sheathing"));
        assert!(ids.contains(&"underlayment"));
        assert!(ids.contains(&"finish"));
        // bears_on must NOT be pre-satisfied by generate — that is the cross-entity
        // mechanism's responsibility.
        assert!(!ids.contains(&"bears_on"));
    }

    // -----------------------------------------------------------------------
    // Registry round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn register_roof_class_and_shed_recipe_in_registry() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(roof_system_class());
        registry.register_recipe_family(shed_roof_framing_recipe());

        assert_eq!(registry.element_class_descriptors().len(), 1);
        assert_eq!(registry.recipe_family_descriptors(None).len(), 1);

        let filtered =
            registry.recipe_family_descriptors(Some(&ElementClassId("roof_system".into())));
        assert_eq!(filtered.len(), 1, "shed_roof_framing must be listed under roof_system");

        let none =
            registry.recipe_family_descriptors(Some(&ElementClassId("wall_assembly".into())));
        assert!(none.is_empty(), "shed_roof_framing must not appear under wall_assembly");
    }
}
