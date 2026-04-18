//! PP72 integration test: `foundation_system` element class with
//! `slab_on_grade` and `pier_foundation` recipe families.
//!
//! Covers:
//! - `select_recipe(foundation_system, {terrain_slope_pct: 2.0})` → slab first.
//! - `select_recipe(foundation_system, {terrain_slope_pct: 18.0})` → pier first.
//! - Promote slab foundation to Constructible → obligations all SatisfiedBy,
//!   top_datum_mm ClaimGrounding entry present.
//! - Wall with pre-existing `bears_on` SemanticRelation into a Constructible
//!   foundation → wall's `bears_on` obligation is `SatisfiedBy(foundation_eid)`.
//! - Wall whose `bears_on` target is still Conceptual → `bears_on` remains
//!   Unresolved; validator emits a finding.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architectural::snapshots::WallFactory;
use talos3d_architecture_core::{
    priors::terrain::{terrain_slope_pier_prior, terrain_slope_slab_prior},
    recipes::{
        foundation_pier::pier_foundation_recipe,
        foundation_slab_on_grade::{foundation_system_class, slab_on_grade_recipe},
        wall_light_frame_exterior::{light_frame_exterior_wall_recipe, wall_assembly_class},
    },
};
use talos3d_core::{
    authored_entity::AuthoredEntity,
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
            handle_create_entity, handle_promote_refinement, handle_run_validation,
            handle_select_recipe,
        },
        modeling::assembly::{AssemblyFactory, RelationFactory, RelationSnapshot, SemanticRelation},
        refinement::{
            ClaimGrounding, ClaimPath, Obligation, ObligationId, ObligationSet, ObligationStatus,
            RefinementState, SemanticRole,
        },
    },
};

// ---------------------------------------------------------------------------
// Test world setup
// ---------------------------------------------------------------------------

fn init_foundation_test_world() -> World {
    let mut world = World::new();

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

    let mut registry = CapabilityRegistry::default();

    // Register all element classes and recipe families needed for PP72.
    registry.register_element_class(foundation_system_class());
    registry.register_element_class(wall_assembly_class());
    registry.register_recipe_family(slab_on_grade_recipe());
    registry.register_recipe_family(pier_foundation_recipe());
    registry.register_recipe_family(light_frame_exterior_wall_recipe());

    // PP76: register terrain priors so slope ranking works via the prior
    // mechanism rather than the removed hardcoded stub.
    registry.register_generation_prior(terrain_slope_slab_prior());
    registry.register_generation_prior(terrain_slope_pier_prior());

    // Refinement relation types.
    for (rt, lbl, desc) in [
        ("refinement_of", "Refinement Of", "child → parent"),
        ("refined_into", "Refined Into", "parent → child"),
        ("bears_on", "Bears On", "structural load path"),
    ] {
        registry.register_relation_type(talos3d_core::capability_registry::RelationTypeDescriptor {
            relation_type: rt.into(),
            label: lbl.into(),
            description: desc.into(),
            valid_source_types: vec![],
            valid_target_types: vec![],
            parameter_schema: serde_json::json!({}),
            participates_in_dependency_graph: false,
        });
    }

    registry.register_factory(WallFactory);
    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);

    world.insert_resource(registry);
    world
}

/// Spawn a minimal entity tagged with the given element class and recipe.
/// Returns the element-id (u64).
fn spawn_foundation_entity(
    world: &mut World,
    class_id: &str,
    recipe_id: &str,
) -> u64 {
    let eid = world.resource::<ElementIdAllocator>().next_id();
    world.spawn((
        eid,
        ElementClassAssignment {
            element_class: ElementClassId(class_id.into()),
            active_recipe: Some(RecipeFamilyId(recipe_id.into())),
        },
    ));
    eid.0
}

/// Create a wall entity via `handle_create_entity` then tag it.
fn create_wall_entity(world: &mut World) -> u64 {
    let wall_eid = handle_create_entity(
        world,
        serde_json::json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [6.0, 0.0],
            "height": 2.7,
            "thickness": 0.2
        }),
    )
    .expect("wall entity should be created");

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

/// Spawn a `bears_on` SemanticRelation from source_eid to target_eid.
fn spawn_bears_on_relation(world: &mut World, source_eid: ElementId, target_eid: ElementId) {
    let rel_eid = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = RelationSnapshot {
        element_id: rel_eid,
        relation: SemanticRelation {
            source: source_eid,
            target: target_eid,
            relation_type: "bears_on".into(),
            parameters: serde_json::json!({}),
        },
    };
    use bevy::ecs::world::World;
    // apply_to spawns the relation entity.
    use talos3d_core::authored_entity::AuthoredEntity;
    snapshot.apply_to(world);
}

// ---------------------------------------------------------------------------
// Test: select_recipe slope ranking
// ---------------------------------------------------------------------------

#[test]
fn select_recipe_flat_terrain_ranks_slab_first() {
    let world = init_foundation_test_world();
    let ranking = handle_select_recipe(
        &world,
        "foundation_system".into(),
        serde_json::json!({"terrain_slope_pct": 2.0}),
    )
    .expect("select_recipe should succeed");

    assert!(!ranking.is_empty(), "must return at least one recipe");
    assert_eq!(
        ranking[0].id, "slab_on_grade",
        "flat terrain (slope 2%) must rank slab_on_grade first; got: {:?}",
        ranking.iter().map(|r| (&r.id, r.weight)).collect::<Vec<_>>()
    );
}

#[test]
fn select_recipe_steep_terrain_ranks_pier_first() {
    let world = init_foundation_test_world();
    let ranking = handle_select_recipe(
        &world,
        "foundation_system".into(),
        serde_json::json!({"terrain_slope_pct": 18.0}),
    )
    .expect("select_recipe should succeed");

    assert!(!ranking.is_empty(), "must return at least one recipe");
    assert_eq!(
        ranking[0].id, "pier_foundation",
        "steep terrain (slope 18%) must rank pier_foundation first; got: {:?}",
        ranking.iter().map(|r| (&r.id, r.weight)).collect::<Vec<_>>()
    );
}

#[test]
fn select_recipe_no_slope_all_weight_one() {
    let world = init_foundation_test_world();
    let ranking = handle_select_recipe(
        &world,
        "foundation_system".into(),
        serde_json::json!({}),
    )
    .expect("select_recipe should succeed");

    assert_eq!(ranking.len(), 2, "both foundation recipes should be returned");
    for entry in &ranking {
        assert!(
            (entry.weight - 1.0).abs() < f32::EPSILON,
            "without slope context, weight must be 1.0; got {} for {}",
            entry.weight,
            entry.id
        );
    }
}

// ---------------------------------------------------------------------------
// Test: promote slab to Constructible
// ---------------------------------------------------------------------------

#[test]
fn promote_slab_to_constructible_satisfies_all_obligations() {
    let mut world = init_foundation_test_world();
    let foundation_eid = spawn_foundation_entity(&mut world, "foundation_system", "slab_on_grade");

    let result = handle_promote_refinement(
        &mut world,
        foundation_eid,
        "Constructible".into(),
        Some("slab_on_grade".into()),
        serde_json::json!({
            "footprint_polygon": [[0,0],[6000,0],[6000,8000],[0,8000]],
            "floor_datum_mm": 0,
            "slab_thickness_mm": 150
        }),
    )
    .expect("promotion should succeed");

    assert_eq!(result.previous_state, "Conceptual");
    assert_eq!(result.new_state, "Constructible");

    let eid = ElementId(foundation_eid);
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

    // 3 class-min obligations, 0 specializations = 3.
    assert_eq!(
        obligation_set.entries.len(),
        3,
        "footing + top_datum + bears_on_terrain; got: {:?}",
        obligation_set.entries.iter().map(|o| &o.id.0).collect::<Vec<_>>()
    );

    for obligation in &obligation_set.entries {
        assert!(
            matches!(obligation.status, ObligationStatus::SatisfiedBy(_)),
            "obligation '{}' should be SatisfiedBy, got: {:?}",
            obligation.id.0,
            obligation.status
        );
    }
}

#[test]
fn promote_slab_to_constructible_installs_top_datum_mm_claim() {
    let mut world = init_foundation_test_world();
    let foundation_eid = spawn_foundation_entity(&mut world, "foundation_system", "slab_on_grade");

    handle_promote_refinement(
        &mut world,
        foundation_eid,
        "Constructible".into(),
        Some("slab_on_grade".into()),
        serde_json::json!({
            "footprint_polygon": [[0,0],[6000,0],[6000,8000],[0,8000]],
            "floor_datum_mm": 0
        }),
    )
    .expect("promotion should succeed");

    let eid = ElementId(foundation_eid);
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    let claim_grounding = q
        .iter(&world)
        .find_map(|(eref,)| {
            if eref.get::<ElementId>().copied() == Some(eid) {
                eref.get::<ClaimGrounding>().cloned()
            } else {
                None
            }
        })
        .expect("entity should have ClaimGrounding after promotion");

    assert!(
        claim_grounding
            .claims
            .contains_key(&ClaimPath("top_datum_mm".into())),
        "ClaimGrounding must have top_datum_mm after Constructible promotion"
    );
}

// ---------------------------------------------------------------------------
// Test: wall-foundation bears_on integration
// ---------------------------------------------------------------------------

/// Helpers to manually insert a bears_on obligation into an entity's ObligationSet.
fn insert_bears_on_obligation(world: &mut World, entity_eid: ElementId) {
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == entity_eid)
            .map(|(e, _)| e)
            .expect("entity should exist")
    };
    // Get or create ObligationSet and insert bears_on.
    let mut existing = world
        .get::<ObligationSet>(entity)
        .cloned()
        .unwrap_or_default();
    existing.entries.push(Obligation {
        id: ObligationId("bears_on".into()),
        role: SemanticRole("load_bearing".into()),
        required_by_state: RefinementState::Constructible,
        status: ObligationStatus::Unresolved,
    });
    world.entity_mut(entity).insert(existing);
}

#[test]
fn wall_bears_on_constructible_foundation_is_satisfied_automatically() {
    let mut world = init_foundation_test_world();

    // 1. Create and promote a slab foundation to Constructible.
    let foundation_eid =
        spawn_foundation_entity(&mut world, "foundation_system", "slab_on_grade");
    handle_promote_refinement(
        &mut world,
        foundation_eid,
        "Constructible".into(),
        Some("slab_on_grade".into()),
        serde_json::json!({
            "footprint_polygon": [[0,0],[6000,0],[6000,8000],[0,8000]],
            "floor_datum_mm": 0
        }),
    )
    .expect("foundation promotion should succeed");

    // 2. Create a wall entity with a bears_on obligation (manually injected).
    let wall_eid = create_wall_entity(&mut world);
    let wall_element_id = ElementId(wall_eid);
    insert_bears_on_obligation(&mut world, wall_element_id);

    // 3. Spawn a bears_on SemanticRelation from wall to foundation.
    spawn_bears_on_relation(
        &mut world,
        wall_element_id,
        ElementId(foundation_eid),
    );

    // 4. Promote the wall to Constructible.
    handle_promote_refinement(
        &mut world,
        wall_eid,
        "Constructible".into(),
        Some("light_frame_exterior_wall".into()),
        serde_json::json!({"length_mm": 6000, "height_mm": 2700, "thickness_mm": 200}),
    )
    .expect("wall promotion should succeed");

    // 5. Verify the bears_on obligation on the wall is SatisfiedBy(foundation_eid).
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
        .expect("wall should have ObligationSet after promotion");

    let bears_on_ob = obligation_set
        .entries
        .iter()
        .find(|o| o.id.0 == "bears_on")
        .expect("wall must have bears_on obligation");

    assert!(
        matches!(
            &bears_on_ob.status,
            ObligationStatus::SatisfiedBy(feid) if *feid == foundation_eid
        ),
        "bears_on obligation must be SatisfiedBy(foundation_eid={}); got: {:?}",
        foundation_eid,
        bears_on_ob.status
    );
}

#[test]
fn wall_bears_on_conceptual_foundation_remains_unresolved_with_validator_finding() {
    let mut world = init_foundation_test_world();

    // 1. Create a foundation entity but do NOT promote it (stays Conceptual).
    let foundation_eid =
        spawn_foundation_entity(&mut world, "foundation_system", "slab_on_grade");
    // Foundation stays at Conceptual (default). No ClaimGrounding, no obligations.

    // 2. Create a wall entity with a bears_on obligation.
    let wall_eid = create_wall_entity(&mut world);
    let wall_element_id = ElementId(wall_eid);
    insert_bears_on_obligation(&mut world, wall_element_id);

    // 3. Spawn a bears_on SemanticRelation from wall to foundation.
    spawn_bears_on_relation(
        &mut world,
        wall_element_id,
        ElementId(foundation_eid),
    );

    // 4. Promote the wall to Constructible. The bears_on target is Conceptual
    //    (no top_datum_mm claim, not Constructible), so the obligation stays Unresolved.
    handle_promote_refinement(
        &mut world,
        wall_eid,
        "Constructible".into(),
        Some("light_frame_exterior_wall".into()),
        serde_json::json!({"length_mm": 6000, "height_mm": 2700, "thickness_mm": 200}),
    )
    .expect("wall promotion should succeed");

    // 5. Verify bears_on stays Unresolved.
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
        .expect("wall should have ObligationSet after promotion");

    let bears_on_ob = obligation_set
        .entries
        .iter()
        .find(|o| o.id.0 == "bears_on")
        .expect("wall must have bears_on obligation");

    assert!(
        matches!(bears_on_ob.status, ObligationStatus::Unresolved),
        "bears_on obligation must remain Unresolved when foundation is Conceptual; got: {:?}",
        bears_on_ob.status
    );

    // 6. Verify run_validation emits an Error finding for bears_on.
    let findings =
        handle_run_validation(&world, wall_eid).expect("run_validation should succeed");

    let bears_on_finding = findings
        .iter()
        .find(|f| f.obligation_id.as_deref() == Some("bears_on"));

    assert!(
        bears_on_finding.is_some(),
        "validator must emit a finding for unresolved bears_on; findings: {:?}",
        findings.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
    assert_eq!(
        bears_on_finding.unwrap().severity,
        "error",
        "bears_on finding must be error severity at Constructible state"
    );
}
