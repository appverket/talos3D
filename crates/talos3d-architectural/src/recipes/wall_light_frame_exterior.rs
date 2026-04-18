//! Registration of the `wall_assembly` element class and the
//! `light_frame_exterior_wall` recipe family (PP71).
//!
//! All architectural nouns live here per ADR-037.  The generic machinery
//! (`ElementClassDescriptor`, `RecipeFamilyDescriptor`, `GenerateFn`, …) lives
//! in `talos3d-core`.

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

/// Build the `wall_assembly` `ElementClassDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
pub fn wall_assembly_class() -> ElementClassDescriptor {
    let mut class_min_obligations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    // At Constructible, five obligations must be resolved.
    class_min_obligations.insert(
        RefinementState::Constructible,
        vec![
            ObligationTemplate {
                id: ObligationId("structure".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("thermal_layer".into()),
                role: SemanticRole("thermal".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("weather_control".into()),
                role: SemanticRole("weather_barrier".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("interior_finish".into()),
                role: SemanticRole("interior_surface".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("exterior_finish".into()),
                role: SemanticRole("exterior_surface".into()),
                required_by_state: RefinementState::Constructible,
            },
            // PP72: bears_on is a class-min obligation satisfied via an existing
            // `bears_on` SemanticRelation into a Constructible+ target.
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
            ClaimPath("height_mm".into()),
            ClaimPath("thickness_mm".into()),
            ClaimPath("length_mm".into()),
            ClaimPath("bears_on".into()),
        ],
    );

    ElementClassDescriptor {
        id: ElementClassId("wall_assembly".into()),
        label: "Wall Assembly".into(),
        description: "A wall assembly element — may serve as exterior envelope, interior \
            partition, or primary-structure element depending on context."
            .into(),
        semantic_roles: vec![
            SemanticRole("exterior_envelope".into()),
            SemanticRole("interior_partition".into()),
            SemanticRole("primary_structure".into()),
        ],
        class_min_obligations,
        class_min_promotion_critical_paths,
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "length_mm":    {"type": "number", "minimum": 0},
                "height_mm":    {"type": "number", "minimum": 0},
                "thickness_mm": {"type": "number", "minimum": 0},
                "bears_on":     {"type": "string", "description": "Element-id or anchor of the bearing surface"}
            }
        }),
    }
}

/// Build the `light_frame_exterior_wall` `RecipeFamilyDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
pub fn light_frame_exterior_wall_recipe() -> RecipeFamilyDescriptor {
    let mut obligation_specializations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    // At Constructible, the recipe adds a lateral-bracing obligation on top of
    // the class minimum.
    obligation_specializations.insert(
        RefinementState::Constructible,
        vec![ObligationTemplate {
            id: ObligationId("lateral_bracing".into()),
            role: SemanticRole("bracing".into()),
            required_by_state: RefinementState::Constructible,
        }],
    );

    let mut promotion_critical_path_specializations: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();

    // The recipe promotes stud_spacing_mm and sheathing_thickness_mm to
    // promotion-critical at Constructible.
    promotion_critical_path_specializations.insert(
        RefinementState::Constructible,
        vec![
            ClaimPath("stud_spacing_mm".into()),
            ClaimPath("sheathing_thickness_mm".into()),
        ],
    );

    RecipeFamilyDescriptor {
        id: RecipeFamilyId("light_frame_exterior_wall".into()),
        target_class: ElementClassId("wall_assembly".into()),
        label: "Light-Frame Exterior Wall".into(),
        description: "Light-frame (wood stud) exterior wall with insulation, sheathing, \
            exterior cladding, and interior finish."
            .into(),
        parameters: vec![
            RecipeParameter {
                name: "length_mm".into(),
                value_schema: serde_json::json!({"type":"number","minimum":0}),
                default: None,
            },
            RecipeParameter {
                name: "height_mm".into(),
                value_schema: serde_json::json!({"type":"number","minimum":0}),
                default: None,
            },
            RecipeParameter {
                name: "thickness_mm".into(),
                value_schema: serde_json::json!({"type":"number","minimum":0}),
                default: None,
            },
            RecipeParameter {
                name: "stud_spacing_mm".into(),
                value_schema: serde_json::json!({"type":"number","minimum":100}),
                default: Some(serde_json::json!(600)),
            },
            RecipeParameter {
                name: "stud_section".into(),
                value_schema: serde_json::json!({"type":"string"}),
                default: Some(serde_json::json!("2x4")),
            },
            RecipeParameter {
                name: "sheathing_thickness_mm".into(),
                value_schema: serde_json::json!({"type":"number","minimum":1}),
                default: Some(serde_json::json!(12)),
            },
            RecipeParameter {
                name: "insulation_type".into(),
                value_schema: serde_json::json!({"type":"string"}),
                default: Some(serde_json::json!("mineral_wool")),
            },
            RecipeParameter {
                name: "exterior_cladding".into(),
                value_schema: serde_json::json!({"type":"string"}),
                default: Some(serde_json::json!("cedar_rainscreen")),
            },
            RecipeParameter {
                name: "interior_finish".into(),
                value_schema: serde_json::json!({"type":"string"}),
                default: Some(serde_json::json!("gypsum_13mm")),
            },
        ],
        supported_refinement_levels: vec![
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
        ],
        obligation_specializations,
        promotion_critical_path_specializations,
        generate: build_generate_fn(),
    }
}

// ---------------------------------------------------------------------------
// Generate function
// ---------------------------------------------------------------------------

/// Merge recipe defaults with caller-supplied overrides into a final parameter
/// map.  Recipe defaults apply for any key the caller omitted.
fn resolve_parameters(input: &GenerateInput) -> HashMap<String, serde_json::Value> {
    let defaults: &[(&str, serde_json::Value)] = &[
        ("stud_spacing_mm", serde_json::json!(600)),
        ("stud_section", serde_json::json!("2x4")),
        ("sheathing_thickness_mm", serde_json::json!(12)),
        ("insulation_type", serde_json::json!("mineral_wool")),
        ("exterior_cladding", serde_json::json!("cedar_rainscreen")),
        ("interior_finish", serde_json::json!("gypsum_13mm")),
    ];
    let mut params = input.parameters.clone();
    for (key, default_value) in defaults {
        params
            .entry((*key).to_string())
            .or_insert_with(|| default_value.clone());
    }
    params
}

/// Spawn a child entity that will satisfy one obligation and return its
/// element-id.  Each child is tagged with `ElementId` and linked to the parent
/// via `refinement_of` / `refined_into`.
fn spawn_child_stub(
    world: &mut World,
    parent_eid: ElementId,
    promoted_from_state: RefinementState,
    target_state: RefinementState,
    _label: impl Into<String>,
) -> u64 {
    let child_eid = world.resource::<ElementIdAllocator>().next_id();
    // Spawn a minimal entity with just an ElementId.
    world.spawn(child_eid);
    // Create the refinement linkage.
    create_refinement_relation_pair(world, parent_eid, child_eid, promoted_from_state, target_state);
    child_eid.0
}

fn build_generate_fn() -> GenerateFn {
    Arc::new(|input: GenerateInput, world: &mut World| -> Result<GenerateOutput, String> {
        let parent_eid = ElementId(input.element_id);
        let target_state = input.target_state;
        let _params = resolve_parameters(&input);
        let promoted_from = RefinementState::Conceptual; // always promoting from some lower state

        let mut output = GenerateOutput::default();

        match target_state {
            RefinementState::Conceptual => {
                // Nothing to generate — obligations are installed by the promote
                // machinery from the merged contract; no children at this level.
            }
            RefinementState::Schematic => {
                // Single box stub representing the wall volume.
                // No obligation-satisfying children; obligations stay Unresolved.
                // (A single entity with parameters attached in the real impl;
                // for PP71 we keep it minimal.)
            }
            RefinementState::Constructible => {
                // Spawn child entities for each obligation satisfier.

                let structure_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state, "wall_studs",
                );
                output.satisfaction_links.push((ObligationId("structure".into()), structure_eid));

                let thermal_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state, "insulation",
                );
                output.satisfaction_links.push((ObligationId("thermal_layer".into()), thermal_eid));

                let weather_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state, "weather_barrier",
                );
                output.satisfaction_links.push((ObligationId("weather_control".into()), weather_eid));

                let interior_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state, "interior_finish",
                );
                output.satisfaction_links.push((ObligationId("interior_finish".into()), interior_eid));

                let exterior_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state, "exterior_cladding",
                );
                output.satisfaction_links.push((ObligationId("exterior_finish".into()), exterior_eid));

                let bracing_eid = spawn_child_stub(
                    world, parent_eid, promoted_from, target_state, "lateral_bracing",
                );
                output.satisfaction_links.push((ObligationId("lateral_bracing".into()), bracing_eid));
            }
            _ => {
                return Err(format!(
                    "light_frame_exterior_wall does not support target state: {}",
                    target_state.as_str()
                ));
            }
        }

        Ok(output)
    })
}

// ---------------------------------------------------------------------------
// Unit tests for the wall recipe module
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

    #[test]
    fn wall_class_has_six_constructible_class_min_obligations() {
        let class = wall_assembly_class();
        let obligations = class
            .class_min_obligations
            .get(&RefinementState::Constructible)
            .expect("obligations at Constructible");
        assert_eq!(obligations.len(), 6, "expected 6 class-min obligations");
        let ids: Vec<&str> = obligations.iter().map(|o| o.id.0.as_str()).collect();
        assert!(ids.contains(&"structure"));
        assert!(ids.contains(&"thermal_layer"));
        assert!(ids.contains(&"weather_control"));
        assert!(ids.contains(&"interior_finish"));
        assert!(ids.contains(&"exterior_finish"));
        assert!(ids.contains(&"bears_on"));
    }

    #[test]
    fn wall_class_has_four_constructible_promotion_critical_paths() {
        let class = wall_assembly_class();
        let paths = class
            .class_min_promotion_critical_paths
            .get(&RefinementState::Constructible)
            .expect("paths at Constructible");
        assert_eq!(paths.len(), 4);
        let strs: Vec<&str> = paths.iter().map(|p| p.0.as_str()).collect();
        assert!(strs.contains(&"height_mm"));
        assert!(strs.contains(&"thickness_mm"));
        assert!(strs.contains(&"length_mm"));
        assert!(strs.contains(&"bears_on"));
    }

    #[test]
    fn recipe_supports_three_refinement_levels() {
        let recipe = light_frame_exterior_wall_recipe();
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
    fn recipe_adds_lateral_bracing_obligation_at_constructible() {
        let recipe = light_frame_exterior_wall_recipe();
        let extra = recipe
            .obligation_specializations
            .get(&RefinementState::Constructible)
            .expect("specializations at Constructible");
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].id.0, "lateral_bracing");
    }

    #[test]
    fn effective_obligations_yields_seven_at_constructible() {
        let class = wall_assembly_class();
        let recipe = light_frame_exterior_wall_recipe();
        let obligations =
            effective_obligations(&class, Some(&recipe), RefinementState::Constructible);
        assert_eq!(obligations.len(), 7, "6 class-min + 1 recipe specialisation");
    }

    #[test]
    fn effective_critical_paths_yields_six_at_constructible() {
        let class = wall_assembly_class();
        let recipe = light_frame_exterior_wall_recipe();
        let paths = effective_promotion_critical_paths(
            &class,
            Some(&recipe),
            RefinementState::Constructible,
        );
        // 4 class-min + 2 recipe specialisations
        assert_eq!(paths.len(), 6);
        let strs: Vec<&str> = paths.iter().map(|p| p.0.as_str()).collect();
        assert!(strs.contains(&"stud_spacing_mm"));
        assert!(strs.contains(&"sheathing_thickness_mm"));
    }

    #[test]
    fn recipe_has_expected_parameters_with_defaults() {
        let recipe = light_frame_exterior_wall_recipe();
        let param = |name: &str| {
            recipe
                .parameters
                .iter()
                .find(|p| p.name == name)
                .expect(name)
        };
        // Required (no default)
        assert!(param("length_mm").default.is_none());
        assert!(param("height_mm").default.is_none());
        // Defaulted
        assert_eq!(param("stud_spacing_mm").default, Some(serde_json::json!(600)));
        assert_eq!(param("stud_section").default, Some(serde_json::json!("2x4")));
        assert_eq!(param("sheathing_thickness_mm").default, Some(serde_json::json!(12)));
        assert_eq!(param("insulation_type").default, Some(serde_json::json!("mineral_wool")));
        assert_eq!(param("exterior_cladding").default, Some(serde_json::json!("cedar_rainscreen")));
        assert_eq!(param("interior_finish").default, Some(serde_json::json!("gypsum_13mm")));
    }

    #[test]
    fn generate_constructible_produces_six_satisfaction_links() {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(CapabilityRegistry::default());
        // Register minimal relation types needed by create_refinement_relation_pair.
        use talos3d_core::plugins::{
            commands::{BeginCommandGroup, EndCommandGroup, ApplyEntityChangesCommand},
            history::{History, PendingCommandQueue},
        };
        use bevy::prelude::Messages;
        world.insert_resource(Messages::<BeginCommandGroup>::default());
        world.insert_resource(Messages::<EndCommandGroup>::default());
        world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(History::default());

        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = light_frame_exterior_wall_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Constructible,
            parameters: HashMap::from([
                ("length_mm".into(), serde_json::json!(4000)),
                ("height_mm".into(), serde_json::json!(2700)),
                ("thickness_mm".into(), serde_json::json!(200)),
            ]),
        };

        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert_eq!(
            output.satisfaction_links.len(),
            6,
            "structure + thermal_layer + weather_control + interior_finish + exterior_finish + lateral_bracing"
        );
        let ids: Vec<&str> = output
            .satisfaction_links
            .iter()
            .map(|(id, _)| id.0.as_str())
            .collect();
        assert!(ids.contains(&"structure"));
        assert!(ids.contains(&"lateral_bracing"));
    }

    #[test]
    fn generate_conceptual_produces_no_links() {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(CapabilityRegistry::default());
        use talos3d_core::plugins::{
            commands::{BeginCommandGroup, EndCommandGroup, ApplyEntityChangesCommand},
            history::{History, PendingCommandQueue},
        };
        use bevy::prelude::Messages;
        world.insert_resource(Messages::<BeginCommandGroup>::default());
        world.insert_resource(Messages::<EndCommandGroup>::default());
        world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(History::default());

        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = light_frame_exterior_wall_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Conceptual,
            parameters: HashMap::new(),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(output.satisfaction_links.is_empty());
    }

    #[test]
    fn register_in_capability_registry() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(wall_assembly_class());
        registry.register_recipe_family(light_frame_exterior_wall_recipe());

        assert_eq!(registry.element_class_descriptors().len(), 1);
        assert_eq!(registry.recipe_family_descriptors(None).len(), 1);

        let filtered = registry
            .recipe_family_descriptors(Some(&ElementClassId("wall_assembly".into())));
        assert_eq!(filtered.len(), 1);

        let none = registry
            .recipe_family_descriptors(Some(&ElementClassId("roof_system".into())));
        assert!(none.is_empty());
    }
}
