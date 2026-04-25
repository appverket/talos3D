//! PP71 integration test: `light_frame_exterior_wall` recipe end-to-end.
//!
//! Covers:
//! - Registry round-trip: register element class + recipe family, query via handlers.
//! - Promote to Constructible: `ObligationSet` populated (5 class-min + 1 recipe = 6),
//!   all `SatisfiedBy(child)`, no unresolved findings.
//! - Promotion-critical paths: `get_claim_grounding` returns `is_promotion_critical = true`
//!   for `stud_spacing_mm` after Constructible promotion.
//! - `refined_into` links to child entities after promotion.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architectural::snapshots::WallFactory;
use talos3d_architecture_core::recipes::wall_light_frame_exterior::{
    light_frame_exterior_wall_recipe, wall_assembly_class,
};
use talos3d_core::{
    capability_registry::{
        CapabilityRegistry, ElementClassAssignment, ElementClassId, RecipeFamilyId,
    },
    plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        history::{History, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        model_api::{
            handle_create_entity, handle_get_claim_grounding, handle_list_element_classes,
            handle_list_recipe_families, handle_promote_refinement, handle_select_recipe,
        },
        modeling::assembly::{AssemblyFactory, RelationFactory},
        refinement::{
            query_refined_into, ClaimGrounding, ClaimPath, ClaimRecord, Grounding, HeuristicTag,
            ObligationSet, ObligationStatus,
        },
    },
};

// ---------------------------------------------------------------------------
// Test world setup
// ---------------------------------------------------------------------------

fn init_recipe_test_world() -> World {
    let mut world = World::new();

    // Message queues required by the command pipeline (matches assembly test world).
    world.insert_resource(Messages::<CreateBoxCommand>::default());
    world.insert_resource(Messages::<CreateCylinderCommand>::default());
    world.insert_resource(Messages::<CreateSphereCommand>::default());
    world.insert_resource(Messages::<CreatePlaneCommand>::default());
    world.insert_resource(Messages::<CreatePolylineCommand>::default());
    world.insert_resource(Messages::<CreateTriangleMeshCommand>::default());
    world.insert_resource(Messages::<CreateEntityCommand>::default());
    world.insert_resource(Messages::<DeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ResolvedDeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
    world.insert_resource(Messages::<BeginCommandGroup>::default());
    world.insert_resource(Messages::<EndCommandGroup>::default());
    world.insert_resource(PendingCommandQueue::default());
    world.insert_resource(History::default());
    world.insert_resource(ElementIdAllocator::default());

    // Capability registry with wall class + recipe registered.
    let mut registry = CapabilityRegistry::default();
    registry.register_element_class(wall_assembly_class());
    registry.register_recipe_family(light_frame_exterior_wall_recipe());

    // Refinement relation types (refinement_of, refined_into).
    registry.register_relation_type(talos3d_core::capability_registry::RelationTypeDescriptor {
        relation_type: "refinement_of".into(),
        label: "Refinement Of".into(),
        description: "child → parent".into(),
        valid_source_types: vec![],
        valid_target_types: vec![],
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: false,
    });
    registry.register_relation_type(talos3d_core::capability_registry::RelationTypeDescriptor {
        relation_type: "refined_into".into(),
        label: "Refined Into".into(),
        description: "parent → child".into(),
        valid_source_types: vec![],
        valid_target_types: vec![],
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: false,
    });

    // Register factories so ensure_entity_exists can capture snapshots.
    registry.register_factory(WallFactory);
    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);

    world.insert_resource(registry);
    world
}

/// Create a wall entity via `handle_create_entity` (registered factory path),
/// tag it with `ElementClassAssignment`, and return its element-id.
fn create_wall_entity(world: &mut World) -> u64 {
    let wall_eid = handle_create_entity(
        world,
        serde_json::json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [4.0, 0.0],
            "height": 2.7,
            "thickness": 0.2
        }),
    )
    .expect("wall entity should be created");

    // Tag with class + recipe assignment.
    let eid = ElementId(wall_eid);
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(e, _)| e)
            .expect("wall entity should exist")
    };
    world.entity_mut(entity).insert(ElementClassAssignment {
        element_class: ElementClassId("wall_assembly".into()),
        active_recipe: Some(RecipeFamilyId("light_frame_exterior_wall".into())),
    });

    wall_eid
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn list_element_classes_returns_wall_assembly() {
    let world = init_recipe_test_world();
    let classes = handle_list_element_classes(&world);
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].id, "wall_assembly");
    assert_eq!(classes[0].label, "Wall Assembly");
    assert!(classes[0]
        .semantic_roles
        .contains(&"exterior_envelope".to_string()));
}

#[test]
fn list_recipe_families_returns_light_frame_recipe() {
    let world = init_recipe_test_world();

    // Unfiltered
    let all = handle_list_recipe_families(&world, None);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "light_frame_exterior_wall");
    assert_eq!(all[0].target_class, "wall_assembly");

    // Filtered to wall_assembly
    let filtered = handle_list_recipe_families(&world, Some("wall_assembly".into()));
    assert_eq!(filtered.len(), 1);

    // Filtered to nonexistent class
    let empty = handle_list_recipe_families(&world, Some("roof_system".into()));
    assert!(empty.is_empty());
}

#[test]
fn select_recipe_returns_all_viable_at_constructible() {
    let world = init_recipe_test_world();
    let ranking = handle_select_recipe(
        &world,
        "wall_assembly".into(),
        serde_json::json!({"target_state": "Constructible"}),
    )
    .expect("select_recipe should succeed");

    assert_eq!(ranking.len(), 1);
    assert_eq!(ranking[0].id, "light_frame_exterior_wall");
    assert!(
        (ranking[0].weight - 1.0).abs() < f32::EPSILON,
        "weight must be 1.0 in PP71"
    );
}

#[test]
fn select_recipe_returns_empty_for_unsupported_state() {
    let world = init_recipe_test_world();
    // FabricationReady is not in supported_refinement_levels.
    let ranking = handle_select_recipe(
        &world,
        "wall_assembly".into(),
        serde_json::json!({"target_state": "FabricationReady"}),
    )
    .expect("select_recipe should succeed");
    assert!(ranking.is_empty());
}

#[test]
fn promote_to_constructible_populates_obligation_set_with_seven_entries() {
    let mut world = init_recipe_test_world();
    let wall_eid = create_wall_entity(&mut world);

    let result = handle_promote_refinement(
        &mut world,
        wall_eid,
        "Constructible".into(),
        Some("light_frame_exterior_wall".into()),
        serde_json::json!({"length_mm": 4000, "height_mm": 2700, "thickness_mm": 200}),
    )
    .expect("promotion should succeed");

    assert_eq!(result.previous_state, "Conceptual");
    assert_eq!(result.new_state, "Constructible");

    // Retrieve ObligationSet from the entity.
    let eid = ElementId(wall_eid);
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    let obligation_set = q
        .iter(&world)
        .find_map(|(eref,)| {
            if eref.get::<ElementId>().copied() == Some(eid) {
                eref.get::<ObligationSet>().cloned()
            } else {
                None
            }
        })
        .expect("entity should have ObligationSet after promotion");

    // 6 class-minimum (envelope layers + bears_on) + 1 recipe specialisation = 7 total.
    assert_eq!(
        obligation_set.entries.len(),
        7,
        "expected 7 obligations (6 class-min + 1 recipe lateral_bracing); got: {:?}",
        obligation_set
            .entries
            .iter()
            .map(|o| &o.id.0)
            .collect::<Vec<_>>()
    );

    // All envelope/recipe obligations must be SatisfiedBy a child entity.
    // bears_on is expected Unresolved since this test does not spawn a
    // foundation target; PP72 covers the bears_on satisfaction path.
    for obligation in &obligation_set.entries {
        if obligation.id.0 == "bears_on" {
            assert!(
                matches!(obligation.status, ObligationStatus::Unresolved),
                "bears_on should be Unresolved without a relation target, got: {:?}",
                obligation.status
            );
        } else {
            assert!(
                matches!(obligation.status, ObligationStatus::SatisfiedBy(_)),
                "obligation '{}' should be SatisfiedBy, got: {:?}",
                obligation.id.0,
                obligation.status
            );
        }
    }

    let ids: Vec<&str> = obligation_set
        .entries
        .iter()
        .map(|o| o.id.0.as_str())
        .collect();
    assert!(ids.contains(&"structure"));
    assert!(ids.contains(&"thermal_layer"));
    assert!(ids.contains(&"weather_control"));
    assert!(ids.contains(&"interior_finish"));
    assert!(ids.contains(&"exterior_finish"));
    assert!(ids.contains(&"lateral_bracing"));
    assert!(ids.contains(&"bears_on"));
}

#[test]
fn promote_to_constructible_links_children_via_refined_into() {
    let mut world = init_recipe_test_world();
    let wall_eid = create_wall_entity(&mut world);

    handle_promote_refinement(
        &mut world,
        wall_eid,
        "Constructible".into(),
        Some("light_frame_exterior_wall".into()),
        serde_json::json!({"length_mm": 4000, "height_mm": 2700, "thickness_mm": 200}),
    )
    .expect("promotion should succeed");

    let parent_eid = ElementId(wall_eid);
    let children = query_refined_into(&world, parent_eid);
    // The wall recipe now emits one refined_into link per satisfied
    // obligation plus the assembly children added by curation merges.
    // Updated from 6 → 14 after the curation/material substrate landed.
    assert_eq!(
        children.len(),
        14,
        "expected 14 refined_into child links; got {}",
        children.len()
    );
}

#[test]
fn get_claim_grounding_returns_promotion_critical_for_stud_spacing() {
    let mut world = init_recipe_test_world();
    let wall_eid = create_wall_entity(&mut world);

    // Promote to Constructible so the entity has state + class assignment.
    handle_promote_refinement(
        &mut world,
        wall_eid,
        "Constructible".into(),
        Some("light_frame_exterior_wall".into()),
        serde_json::json!({"length_mm": 4000, "height_mm": 2700, "thickness_mm": 200}),
    )
    .expect("promotion should succeed");

    // Manually insert a ClaimGrounding entry for stud_spacing_mm.
    let eid_value = ElementId(wall_eid);
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(&world)
            .find(|(_, id)| **id == eid_value)
            .map(|(e, _)| e)
            .expect("entity should exist")
    };
    let now = 1_700_000_000_i64;
    let grounding = ClaimGrounding {
        claims: {
            let mut map = std::collections::HashMap::new();
            map.insert(
                ClaimPath("stud_spacing_mm".into()),
                ClaimRecord {
                    grounding: Grounding::LLMHeuristic {
                        rationale: "default from recipe".into(),
                        heuristic_tag: HeuristicTag("recipe_default".into()),
                    },
                    set_at: now,
                    set_by: None,
                },
            );
            map.insert(
                ClaimPath("unrelated_param".into()),
                ClaimRecord {
                    grounding: Grounding::LLMHeuristic {
                        rationale: "arbitrary".into(),
                        heuristic_tag: HeuristicTag("test".into()),
                    },
                    set_at: now,
                    set_by: None,
                },
            );
            map
        },
    };
    world.entity_mut(entity).insert(grounding);

    // Query grounding — stud_spacing_mm should be promotion-critical.
    let entries =
        handle_get_claim_grounding(&world, wall_eid, None).expect("grounding query should succeed");

    let stud_entry = entries
        .iter()
        .find(|e| e.path == "stud_spacing_mm")
        .expect("stud_spacing_mm entry should be present");
    assert!(
        stud_entry.is_promotion_critical,
        "stud_spacing_mm must be promotion-critical at Constructible for this recipe"
    );

    let unrelated_entry = entries
        .iter()
        .find(|e| e.path == "unrelated_param")
        .expect("unrelated_param entry should be present");
    assert!(
        !unrelated_entry.is_promotion_critical,
        "unrelated_param must NOT be promotion-critical"
    );
}
