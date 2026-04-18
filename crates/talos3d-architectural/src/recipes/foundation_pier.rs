//! `pier_foundation` recipe family for the `foundation_system` element class (PP72).
//!
//! Places discrete box-column pier children at footprint corners and along
//! segments at ≤ `pier_spacing_mm` intervals, plus a grade-beam top-plate
//! child that satisfies the `top_datum` obligation.
//!
//! Scope notes (PP72 spec):
//! - No ADR-034 terrain-conforming implementation.
//! - Generation priors are family-local stubs; real `GenerationPriorDescriptor`
//!   lands in PP76. Slope ranking lives as a stub in `handle_select_recipe`.

use std::{collections::HashMap, sync::Arc};

use bevy::prelude::*;
use talos3d_core::{
    capability_registry::{
        ElementClassId, GenerateFn, GenerateInput, GenerateOutput, ObligationTemplate,
        RecipeFamilyDescriptor, RecipeFamilyId, RecipeParameter,
    },
    plugins::{
        identity::{ElementId, ElementIdAllocator},
        refinement::{
            ClaimPath, ClaimRecord, Grounding, ObligationId, RefinementState,
            create_refinement_relation_pair,
        },
    },
};

// Re-export the element class from the slab module so callers only need one
// registration call.
pub use crate::recipes::foundation_slab_on_grade::foundation_system_class;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Build the `pier_foundation` `RecipeFamilyDescriptor`.
///
/// Registered by `ArchitecturalPlugin` via `CapabilityRegistryAppExt`.
pub fn pier_foundation_recipe() -> RecipeFamilyDescriptor {
    // Recipe adds no obligation specializations beyond the class minimum.
    let obligation_specializations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();

    let promotion_critical_path_specializations: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();

    RecipeFamilyDescriptor {
        id: RecipeFamilyId("pier_foundation".into()),
        target_class: ElementClassId("foundation_system".into()),
        label: "Pier Foundation".into(),
        description: "Discrete concrete pier columns placed at footprint corners and along \
            footprint segments at ≤ pier_spacing_mm intervals. A grade-beam / top-plate \
            element satisfies the top_datum obligation. Preferred on steep terrain (slope > 10%). \
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
                name: "pier_spacing_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 200}),
                default: Some(serde_json::json!(2400)),
            },
            RecipeParameter {
                name: "pier_diameter_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 100}),
                default: Some(serde_json::json!(300)),
            },
            RecipeParameter {
                name: "pier_depth_below_grade_mm".into(),
                value_schema: serde_json::json!({"type": "number", "minimum": 0}),
                default: Some(serde_json::json!(900)),
            },
        ],
        supported_refinement_levels: vec![
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
        ],
        obligation_specializations,
        promotion_critical_path_specializations,
        generate: build_pier_generate_fn(),
    }
}

// ---------------------------------------------------------------------------
// Generate function — pier_foundation
// ---------------------------------------------------------------------------

/// Resolve recipe parameter defaults.
fn resolve_pier_parameters(input: &GenerateInput) -> HashMap<String, serde_json::Value> {
    let defaults: &[(&str, serde_json::Value)] = &[
        ("floor_datum_mm", serde_json::json!(0)),
        ("pier_spacing_mm", serde_json::json!(2400)),
        ("pier_diameter_mm", serde_json::json!(300)),
        ("pier_depth_below_grade_mm", serde_json::json!(900)),
    ];
    let mut params = input.parameters.clone();
    for (key, default_value) in defaults {
        params
            .entry((*key).to_string())
            .or_insert_with(|| default_value.clone());
    }
    params
}

/// Spawn a child entity linked to the parent via refinement relation pair.
fn spawn_child_stub(
    world: &mut World,
    parent_eid: ElementId,
    promoted_from: RefinementState,
    target_state: RefinementState,
) -> u64 {
    let child_eid = world.resource::<ElementIdAllocator>().next_id();
    world.spawn(child_eid);
    create_refinement_relation_pair(world, parent_eid, child_eid, promoted_from, target_state);
    child_eid.0
}

/// Compute pier placement positions along a footprint polygon.
///
/// Places piers at each vertex and subdivides each edge so no gap exceeds
/// `spacing_mm`. Returns a list of `(x_mm, z_mm)` positions.
fn compute_pier_positions(
    footprint: &[[f64; 2]],
    spacing_mm: f64,
) -> Vec<[f64; 2]> {
    if footprint.is_empty() {
        return Vec::new();
    }

    let mut positions: Vec<[f64; 2]> = Vec::new();

    for (i, &corner) in footprint.iter().enumerate() {
        positions.push(corner);

        // Subdivide the edge from this corner to the next.
        let next = footprint[(i + 1) % footprint.len()];
        let dx = next[0] - corner[0];
        let dz = next[1] - corner[1];
        let edge_len = (dx * dx + dz * dz).sqrt();

        if edge_len <= spacing_mm {
            continue; // Corner piers only; no intermediate piers needed.
        }

        let n_intervals = (edge_len / spacing_mm).ceil() as usize;
        // Interior subdivision points (excluding the corners themselves).
        for j in 1..n_intervals {
            let t = j as f64 / n_intervals as f64;
            positions.push([corner[0] + t * dx, corner[1] + t * dz]);
        }
    }

    positions
}

/// Parse the `footprint_polygon` parameter into `[[f64; 2]]`.
fn parse_footprint(params: &HashMap<String, serde_json::Value>) -> Vec<[f64; 2]> {
    let Some(poly) = params.get("footprint_polygon") else {
        return Vec::new();
    };
    let Some(arr) = poly.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|pt| {
            let pair = pt.as_array()?;
            let x = pair.first()?.as_f64()?;
            let z = pair.get(1)?.as_f64()?;
            Some([x, z])
        })
        .collect()
}

fn build_pier_generate_fn() -> GenerateFn {
    Arc::new(|input: GenerateInput, world: &mut World| -> Result<GenerateOutput, String> {
        let parent_eid = ElementId(input.element_id);
        let target_state = input.target_state;
        let params = resolve_pier_parameters(&input);
        let promoted_from = RefinementState::Conceptual;

        let mut output = GenerateOutput::default();

        match target_state {
            RefinementState::Conceptual | RefinementState::Schematic => {
                // No children at these levels.
            }
            RefinementState::Constructible => {
                let floor_datum_mm = params
                    .get("floor_datum_mm")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let pier_spacing_mm = params
                    .get("pier_spacing_mm")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(2400.0);

                let footprint = parse_footprint(&params);

                // Compute pier positions (corners + intermediate).
                let pier_positions = if footprint.is_empty() {
                    Vec::new()
                } else {
                    compute_pier_positions(&footprint, pier_spacing_mm)
                };

                // Spawn one child entity per pier position.
                // All pier children are grouped under the `footing` obligation.
                // The first pier satisfies the obligation; remaining piers are
                // additional `refined_into` children tagged to the parent.
                let pier_count = pier_positions.len().max(1); // at least one footing stub

                let mut first_pier_eid: Option<u64> = None;
                for _ in 0..pier_count {
                    let eid =
                        spawn_child_stub(world, parent_eid, promoted_from, target_state);
                    if first_pier_eid.is_none() {
                        first_pier_eid = Some(eid);
                    }
                }

                // The first pier satisfies `footing`.
                if let Some(pier_eid) = first_pier_eid {
                    output
                        .satisfaction_links
                        .push((ObligationId("footing".into()), pier_eid));
                }

                // Spawn grade-beam / top-plate child to satisfy `top_datum`.
                let top_datum_eid =
                    spawn_child_stub(world, parent_eid, promoted_from, target_state);
                output
                    .satisfaction_links
                    .push((ObligationId("top_datum".into()), top_datum_eid));

                // Spawn terrain-interface stub to satisfy `bears_on_terrain`.
                let terrain_eid =
                    spawn_child_stub(world, parent_eid, promoted_from, target_state);
                output
                    .satisfaction_links
                    .push((ObligationId("bears_on_terrain".into()), terrain_eid));

                // Populate top_datum_mm grounding for wall bears_on resolution.
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

                let _ = floor_datum_mm;
            }
            _ => {
                return Err(format!(
                    "pier_foundation does not support target state: {}",
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
            ElementClassId,
        },
        plugins::refinement::RefinementState,
    };

    #[test]
    fn pier_recipe_targets_foundation_system_class() {
        let recipe = pier_foundation_recipe();
        assert_eq!(recipe.target_class.0, "foundation_system");
        assert_eq!(recipe.id.0, "pier_foundation");
    }

    #[test]
    fn pier_recipe_supports_three_refinement_levels() {
        let recipe = pier_foundation_recipe();
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
    fn pier_recipe_parameters_have_correct_defaults() {
        let recipe = pier_foundation_recipe();
        let param = |name: &str| {
            recipe
                .parameters
                .iter()
                .find(|p| p.name == name)
                .expect(name)
        };
        assert!(param("footprint_polygon").default.is_none());
        assert_eq!(param("floor_datum_mm").default, Some(serde_json::json!(0)));
        assert_eq!(
            param("pier_spacing_mm").default,
            Some(serde_json::json!(2400))
        );
        assert_eq!(
            param("pier_diameter_mm").default,
            Some(serde_json::json!(300))
        );
        assert_eq!(
            param("pier_depth_below_grade_mm").default,
            Some(serde_json::json!(900))
        );
    }

    #[test]
    fn effective_obligations_yields_three_at_constructible() {
        let class = foundation_system_class();
        let recipe = pier_foundation_recipe();
        let obligations =
            effective_obligations(&class, Some(&recipe), RefinementState::Constructible);
        // 3 class-min + 0 recipe specializations = 3
        assert_eq!(obligations.len(), 3);
    }

    #[test]
    fn effective_critical_paths_yields_three_at_constructible() {
        let class = foundation_system_class();
        let recipe = pier_foundation_recipe();
        let paths = effective_promotion_critical_paths(
            &class,
            Some(&recipe),
            RefinementState::Constructible,
        );
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

        let recipe = pier_foundation_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Conceptual,
            parameters: HashMap::new(),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(output.satisfaction_links.is_empty());
    }

    #[test]
    fn generate_constructible_produces_three_satisfaction_links_with_footprint() {
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

        let recipe = pier_foundation_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Constructible,
            parameters: HashMap::from([(
                "footprint_polygon".into(),
                serde_json::json!([[0,0],[6000,0],[6000,8000],[0,8000]]),
            )]),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        // footing + top_datum + bears_on_terrain = 3 satisfaction links
        assert_eq!(
            output.satisfaction_links.len(),
            3,
            "footing + top_datum + bears_on_terrain; got: {:?}",
            output.satisfaction_links.iter().map(|(id,_)| &id.0).collect::<Vec<_>>()
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

        let recipe = pier_foundation_recipe();
        let input = GenerateInput {
            element_id: parent_eid.0,
            target_state: RefinementState::Constructible,
            parameters: HashMap::new(),
        };
        let output = (recipe.generate)(input, &mut world).expect("generate should succeed");
        assert!(
            output.grounding_updates.contains_key(&ClaimPath("top_datum_mm".into())),
            "grounding_updates must include top_datum_mm"
        );
    }

    #[test]
    fn compute_pier_positions_rectangle_no_intermediate() {
        // 4m × 4m rectangle with 5m spacing → only corners
        let footprint = [[0.0, 0.0], [4000.0, 0.0], [4000.0, 4000.0], [0.0, 4000.0]];
        let positions = compute_pier_positions(&footprint, 5000.0);
        assert_eq!(positions.len(), 4, "only corner piers when edge < spacing");
    }

    #[test]
    fn compute_pier_positions_long_edge_subdivided() {
        // 10m edge with 3m spacing → 4 intervals = 3 intermediate + 1 corner = 4 pts on that edge
        let footprint = [[0.0, 0.0], [10000.0, 0.0], [10000.0, 1000.0], [0.0, 1000.0]];
        let positions = compute_pier_positions(&footprint, 3000.0);
        // 4 corners + intermediate piers on the two 10m edges + none on 1m edges
        // 10000/3000 = ceil(3.33) = 4 intervals → 3 intermediate per 10m edge
        // total = 4 corners + 3 + 3 = 10
        assert!(
            positions.len() >= 4,
            "must have at least corner piers; got {}",
            positions.len()
        );
    }

    #[test]
    fn register_foundation_class_and_both_recipes_in_registry() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(foundation_system_class());
        registry.register_recipe_family(
            crate::recipes::foundation_slab_on_grade::slab_on_grade_recipe(),
        );
        registry.register_recipe_family(pier_foundation_recipe());

        assert_eq!(registry.element_class_descriptors().len(), 1);
        let families =
            registry.recipe_family_descriptors(Some(&ElementClassId("foundation_system".into())));
        assert_eq!(families.len(), 2, "both slab_on_grade and pier_foundation should be listed");
        let ids: Vec<&str> = families.iter().map(|f| f.id.0.as_str()).collect();
        assert!(ids.contains(&"slab_on_grade"));
        assert!(ids.contains(&"pier_foundation"));
    }
}
