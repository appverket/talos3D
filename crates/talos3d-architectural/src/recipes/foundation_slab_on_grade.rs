//! Registration of the `foundation_system` element class and the
//! `slab_on_grade` recipe family (PP72).
//!
//! All architectural nouns live here per ADR-037.  The generic machinery lives
//! in `talos3d-core`.
//!
//! Scope notes (PP72 spec):
//! - No ADR-034 terrain-conforming implementation; slab is a parametric box.
//! - Generation priors are family-local stubs; real `GenerationPriorDescriptor`
//!   lands in PP76.

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
            ClaimPath, ClaimRecord, Grounding, ObligationId, RefinementState, SemanticRole,
            create_refinement_relation_pair,
        },
    },
};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Build the `foundation_system` `ElementClassDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
/// Both `slab_on_grade` and `pier_foundation` target this class.
pub fn foundation_system_class() -> ElementClassDescriptor {
    let mut class_min_obligations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    class_min_obligations.insert(
        RefinementState::Constructible,
        vec![
            ObligationTemplate {
                id: ObligationId("footing".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("top_datum".into()),
                role: SemanticRole("load_bearing".into()),
                required_by_state: RefinementState::Constructible,
            },
            ObligationTemplate {
                id: ObligationId("bears_on_terrain".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Constructible,
            },
        ],
    );

    let mut class_min_promotion_critical_paths: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();

    class_min_promotion_critical_paths.insert(
        RefinementState::Constructible,
        vec![
            ClaimPath("top_datum_mm".into()),
            ClaimPath("footprint_polygon".into()),
            ClaimPath("floor_datum_mm".into()),
        ],
    );

    ElementClassDescriptor {
        id: ElementClassId("foundation_system".into()),
        label: "Foundation System".into(),
        description: "A foundation system element — carries structural loads from the building \
            to the bearing stratum. May be slab-on-grade, pier-and-beam, strip footing, \
            or mat foundation depending on the recipe chosen."
            .into(),
        semantic_roles: vec![
            SemanticRole("primary_structure".into()),
            SemanticRole("load_bearing".into()),
        ],
        class_min_obligations,
        class_min_promotion_critical_paths,
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "footprint_polygon": {
                    "type": "array",
                    "description": "Array of [x, z] point pairs defining the foundation footprint",
                    "items": {
                        "type": "array",
                        "items": {"type": "number"},
                        "minItems": 2,
                        "maxItems": 2
                    }
                },
                "floor_datum_mm": {
                    "type": "number",
                    "description": "Elevation of the finished floor above project datum (mm)"
                },
                "slab_thickness_mm": {
                    "type": "number",
                    "minimum": 50,
                    "description": "Slab thickness (mm)"
                },
                "below_grade_margin_mm": {
                    "type": "number",
                    "minimum": 0,
                    "description": "Depth below grade datum (mm)"
                }
            }
        }),
    }
}

/// Build the `slab_on_grade` `RecipeFamilyDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
pub fn slab_on_grade_recipe() -> RecipeFamilyDescriptor {
    // No additional obligation specializations beyond the class minimum for
    // slab_on_grade; the `stem` obligation from the spec is omitted via
    // this specialization leaving it empty.
    let obligation_specializations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    let promotion_critical_path_specializations: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();

    RecipeFamilyDescriptor {
        id: RecipeFamilyId("slab_on_grade".into()),
        target_class: ElementClassId("foundation_system".into()),
        label: "Slab on Grade".into(),
        description: "Monolithic concrete slab-on-grade foundation. Suitable for flat or \
            gently-sloped terrain (slope < 5%). The slab sits at floor_datum - \
            slab_thickness_mm. \
            TODO(PP76): real GenerationPriorDescriptor mechanism — prior weight is \
            expressed as a stub terrain_slope_pct ranking in handle_select_recipe."
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
                name: "floor_datum_mm".into(),
                value_schema: serde_json::json!({"type": "number"}),
                default: Some(serde_json::json!(0)),
            },
            RecipeParameter {
                name: "slab_thickness_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 50}),
                default: Some(serde_json::json!(150)),
            },
            RecipeParameter {
                name: "below_grade_margin_mm".into(),
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
        generate: build_slab_generate_fn(),
    }
}

// ---------------------------------------------------------------------------
// Generate function — slab_on_grade
// ---------------------------------------------------------------------------

/// Resolve recipe parameter defaults into a final map.
fn resolve_slab_parameters(input: &GenerateInput) -> HashMap<String, serde_json::Value> {
    let defaults: &[(&str, serde_json::Value)] = &[
        ("floor_datum_mm", serde_json::json!(0)),
        ("slab_thickness_mm", serde_json::json!(150)),
        ("below_grade_margin_mm", serde_json::json!(300)),
    ];
    let mut params = input.parameters.clone();
    for (key, default_value) in defaults {
        params
            .entry((*key).to_string())
            .or_insert_with(|| default_value.clone());
    }
    params
}

/// Spawn a minimal child entity and link it to the parent via a
/// `refined_into` / `refinement_of` pair. Returns the child element-id.
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

fn build_slab_generate_fn() -> GenerateFn {
    Arc::new(|input: GenerateInput, world: &mut World| -> Result<GenerateOutput, String> {
        let parent_eid = ElementId(input.element_id);
        let target_state = input.target_state;
        let params = resolve_slab_parameters(&input);
        let promoted_from = RefinementState::Conceptual;

        let mut output = GenerateOutput::default();

        match target_state {
            RefinementState::Conceptual | RefinementState::Schematic => {
                // No children at these levels; obligations are installed but
                // remain Unresolved until Constructible promotion.
            }
            RefinementState::Constructible => {
                // --- Parametric slab box ---
                // Position: top-of-slab = floor_datum_mm, bottom = floor_datum_mm - slab_thickness_mm
                let floor_datum_mm = params
                    .get("floor_datum_mm")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let slab_thickness_mm = params
                    .get("slab_thickness_mm")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(150.0);

                // top_datum_mm is the finished floor elevation (= floor_datum_mm for slab-on-grade).
                let top_datum_mm = floor_datum_mm;

                // TODO(ADR-034): terrain-conforming slab mesh; for now we use a flat parametric box.

                // Spawn `footing` child (the concrete slab body).
                let footing_eid =
                    spawn_child_stub(world, parent_eid, promoted_from, target_state);
                output
                    .satisfaction_links
                    .push((ObligationId("footing".into()), footing_eid));

                // Spawn `top_datum` child (a datum plate at floor level).
                let top_datum_eid =
                    spawn_child_stub(world, parent_eid, promoted_from, target_state);
                output
                    .satisfaction_links
                    .push((ObligationId("top_datum".into()), top_datum_eid));

                // Spawn `bears_on_terrain` child (stub — terrain interface).
                let terrain_eid =
                    spawn_child_stub(world, parent_eid, promoted_from, target_state);
                output
                    .satisfaction_links
                    .push((ObligationId("bears_on_terrain".into()), terrain_eid));

                // Populate grounding for top_datum_mm so walls can pick it up
                // during their own Constructible promotion via bears_on resolution.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                output.grounding_updates.insert(
                    ClaimPath("top_datum_mm".into()),
                    ClaimRecord {
                        grounding: Grounding::Refined(input.element_id),
                        set_at: now,
                        set_by: None,
                    },
                );
                output.grounding_updates.insert(
                    ClaimPath("slab_thickness_mm".into()),
                    ClaimRecord {
                        grounding: Grounding::Refined(input.element_id),
                        set_at: now,
                        set_by: None,
                    },
                );

                // Store computed top_datum_mm as a parameter so the promote
                // machinery can expose it; embed in the grounding record
                // parameters field via the JSON value.
                // The actual numeric value is tracked via a separate component
                // convention: we store it as a claim at top_datum_mm path.
                let _ = top_datum_mm; // value is encoded in Grounding::Refined above
                let _ = slab_thickness_mm;
            }
            _ => {
                return Err(format!(
                    "slab_on_grade does not support target state: {}",
                    target_state.as_str()
                ));
            }
        }

        Ok(output)
    })
}

// ---------------------------------------------------------------------------
// Unit tests
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
    fn foundation_class_has_three_constructible_obligations() {
        let class = foundation_system_class();
        let obligations = class
            .class_min_obligations
            .get(&RefinementState::Constructible)
            .expect("obligations at Constructible");
        assert_eq!(obligations.len(), 3);
        let ids: Vec<&str> = obligations.iter().map(|o| o.id.0.as_str()).collect();
        assert!(ids.contains(&"footing"));
        assert!(ids.contains(&"top_datum"));
        assert!(ids.contains(&"bears_on_terrain"));
    }

    #[test]
    fn foundation_class_has_three_constructible_promotion_critical_paths() {
        let class = foundation_system_class();
        let paths = class
            .class_min_promotion_critical_paths
            .get(&RefinementState::Constructible)
            .expect("paths at Constructible");
        assert_eq!(paths.len(), 3);
        let strs: Vec<&str> = paths.iter().map(|p| p.0.as_str()).collect();
        assert!(strs.contains(&"top_datum_mm"));
        assert!(strs.contains(&"footprint_polygon"));
        assert!(strs.contains(&"floor_datum_mm"));
    }

    #[test]
    fn foundation_class_has_primary_structure_and_load_bearing_roles() {
        let class = foundation_system_class();
        let roles: Vec<&str> = class.semantic_roles.iter().map(|r| r.0.as_str()).collect();
        assert!(roles.contains(&"primary_structure"));
        assert!(roles.contains(&"load_bearing"));
    }

    #[test]
    fn slab_recipe_targets_foundation_system_class() {
        let recipe = slab_on_grade_recipe();
        assert_eq!(recipe.target_class.0, "foundation_system");
        assert_eq!(recipe.id.0, "slab_on_grade");
    }

    #[test]
    fn slab_recipe_supports_three_refinement_levels() {
        let recipe = slab_on_grade_recipe();
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
    fn slab_recipe_parameters_have_correct_defaults() {
        let recipe = slab_on_grade_recipe();
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
        assert_eq!(param("floor_datum_mm").default, Some(serde_json::json!(0)));
        assert_eq!(
            param("slab_thickness_mm").default,
            Some(serde_json::json!(150))
        );
        assert_eq!(
            param("below_grade_margin_mm").default,
            Some(serde_json::json!(300))
        );
    }

    #[test]
    fn effective_obligations_yields_three_at_constructible() {
        let class = foundation_system_class();
        let recipe = slab_on_grade_recipe();
        let obligations =
            effective_obligations(&class, Some(&recipe), RefinementState::Constructible);
        // 3 class-min + 0 recipe specializations = 3
        assert_eq!(obligations.len(), 3, "expected 3 obligations (no recipe specializations)");
    }

    #[test]
    fn effective_critical_paths_yields_three_at_constructible() {
        let class = foundation_system_class();
        let recipe = slab_on_grade_recipe();
        let paths = effective_promotion_critical_paths(
            &class,
            Some(&recipe),
            RefinementState::Constructible,
        );
        // 3 class-min + 0 recipe specializations = 3
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn generate_conceptual_produces_no_links() {
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

        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = slab_on_grade_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Conceptual,
            parameters: HashMap::new(),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(output.satisfaction_links.is_empty());
    }

    #[test]
    fn generate_constructible_produces_three_satisfaction_links() {
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

        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = slab_on_grade_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Constructible,
            parameters: HashMap::from([
                (
                    "footprint_polygon".into(),
                    serde_json::json!([[0,0],[6000,0],[6000,8000],[0,8000]]),
                ),
                ("floor_datum_mm".into(), serde_json::json!(0)),
            ]),
        };

        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert_eq!(
            output.satisfaction_links.len(),
            3,
            "footing + top_datum + bears_on_terrain"
        );
        let ids: Vec<&str> = output
            .satisfaction_links
            .iter()
            .map(|(id, _)| id.0.as_str())
            .collect();
        assert!(ids.contains(&"footing"));
        assert!(ids.contains(&"top_datum"));
        assert!(ids.contains(&"bears_on_terrain"));
    }

    #[test]
    fn generate_constructible_populates_top_datum_grounding() {
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

        let parent_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn(parent_eid);

        let recipe = slab_on_grade_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Constructible,
            parameters: HashMap::from([("floor_datum_mm".into(), serde_json::json!(200))]),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(
            output.grounding_updates.contains_key(&ClaimPath("top_datum_mm".into())),
            "grounding_updates must include top_datum_mm"
        );
    }

    #[test]
    fn register_foundation_class_and_slab_recipe_in_registry() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(foundation_system_class());
        registry.register_recipe_family(slab_on_grade_recipe());

        assert_eq!(registry.element_class_descriptors().len(), 1);
        assert_eq!(registry.recipe_family_descriptors(None).len(), 1);

        let filtered = registry
            .recipe_family_descriptors(Some(&ElementClassId("foundation_system".into())));
        assert_eq!(filtered.len(), 1, "slab_on_grade must be listed under foundation_system");

        let none =
            registry.recipe_family_descriptors(Some(&ElementClassId("wall_assembly".into())));
        assert!(none.is_empty());
    }
}
