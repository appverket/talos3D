//! PP73 integration test: `roof_system` element class with
//! `shed_roof_framing` recipe family.
//!
//! Covers:
//! - `list_element_classes` includes `roof_system`.
//! - `list_recipe_families(roof_system)` returns `shed_roof_framing`.
//! - Promote a roof with a pre-existing `bears_on` relation into a Constructible
//!   wall → roof promoted to Constructible, all 5 obligations satisfied
//!   (4 children + bears_on SatisfiedBy wall_eid).
//! - Promote a roof with NO bears_on relation → 4 obligations SatisfiedBy,
//!   bears_on remains Unresolved; validator emits a finding.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architectural::snapshots::WallFactory;
use talos3d_architecture_core::recipes::{
    foundation_slab_on_grade::{foundation_system_class, slab_on_grade_recipe},
    roof_shed_framing::{roof_system_class, shed_roof_framing_recipe},
    wall_light_frame_exterior::{light_frame_exterior_wall_recipe, wall_assembly_class},
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
            handle_create_entity, handle_list_element_classes, handle_list_recipe_families,
            handle_promote_refinement, handle_run_validation,
        },
        modeling::assembly::{
            AssemblyFactory, RelationFactory, RelationSnapshot, SemanticRelation,
        },
        refinement::{
            ClaimGrounding, ClaimPath, ClaimRecord, Grounding, ObligationSet, ObligationStatus,
            RefinementState, RefinementStateComponent,
        },
    },
};

// ---------------------------------------------------------------------------
// Test world setup
// ---------------------------------------------------------------------------

fn init_roof_test_world() -> World {
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

    // Register all element classes and recipe families needed for PP73.
    registry.register_element_class(foundation_system_class());
    registry.register_element_class(wall_assembly_class());
    registry.register_element_class(roof_system_class());
    registry.register_recipe_family(slab_on_grade_recipe());
    registry.register_recipe_family(light_frame_exterior_wall_recipe());
    registry.register_recipe_family(shed_roof_framing_recipe());

    // Refinement relation types.
    for (rt, lbl, desc) in [
        ("refinement_of", "Refinement Of", "child → parent"),
        ("refined_into", "Refined Into", "parent → child"),
        ("bears_on", "Bears On", "structural load path"),
    ] {
        registry.register_relation_type(
            talos3d_core::capability_registry::RelationTypeDescriptor {
                relation_type: rt.into(),
                label: lbl.into(),
                description: desc.into(),
                valid_source_types: vec![],
                valid_target_types: vec![],
                parameter_schema: serde_json::json!({}),
                participates_in_dependency_graph: false,
            },
        );
    }

    registry.register_factory(WallFactory);
    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);

    world.insert_resource(registry);
    world
}

/// Spawn a minimal entity tagged with the given element class and recipe.
/// Returns the element-id (u64).
fn spawn_element_entity(world: &mut World, class_id: &str, recipe_id: &str) -> u64 {
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
    use talos3d_core::authored_entity::AuthoredEntity;
    snapshot.apply_to(world);
}

/// Create a wall via `handle_create_entity` and tag it with wall class/recipe.
fn create_constructible_wall(world: &mut World) -> u64 {
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

    // Promote the wall to Constructible so it has top_datum_mm and the right state.
    // We manually insert the ClaimGrounding with top_datum_mm since the wall recipe
    // (unlike the slab recipe) doesn't set it, but the bears_on resolver looks for it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    world.entity_mut(entity).insert(ClaimGrounding {
        claims: std::collections::HashMap::from([(
            ClaimPath("top_datum_mm".into()),
            ClaimRecord {
                grounding: Grounding::Refined(wall_eid),
                set_at: now,
                set_by: None,
            },
        )]),
    });
    // Mark as Constructible so the readiness check passes.
    world.entity_mut(entity).insert(RefinementStateComponent {
        state: RefinementState::Constructible,
    });

    wall_eid
}

// ---------------------------------------------------------------------------
// Test: registry discovery
// ---------------------------------------------------------------------------

#[test]
fn list_element_classes_includes_roof_system() {
    let world = init_roof_test_world();
    let classes = handle_list_element_classes(&world);
    let ids: Vec<&str> = classes.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"roof_system"),
        "list_element_classes must include roof_system; got: {ids:?}"
    );
}

#[test]
fn list_recipe_families_for_roof_system_returns_shed_roof_framing() {
    let world = init_roof_test_world();
    let families = handle_list_recipe_families(&world, Some("roof_system".into()));
    assert_eq!(
        families.len(),
        1,
        "exactly one recipe for roof_system; got: {:?}",
        families.iter().map(|f| &f.id).collect::<Vec<_>>()
    );
    assert_eq!(families[0].id, "shed_roof_framing");
}

#[test]
fn list_recipe_families_for_wall_does_not_include_roof_recipe() {
    let world = init_roof_test_world();
    let families = handle_list_recipe_families(&world, Some("wall_assembly".into()));
    let ids: Vec<&str> = families.iter().map(|f| f.id.as_str()).collect();
    assert!(
        !ids.contains(&"shed_roof_framing"),
        "shed_roof_framing must not appear under wall_assembly"
    );
}

// ---------------------------------------------------------------------------
// Test: promote roof with bears_on relation → all obligations satisfied
// ---------------------------------------------------------------------------

#[test]
fn promote_roof_with_bears_on_relation_satisfies_all_five_obligations() {
    let mut world = init_roof_test_world();

    // 1. Create a Constructible wall entity (with top_datum_mm claim already set).
    let wall_eid = create_constructible_wall(&mut world);

    // 2. Spawn a roof entity.
    let roof_eid = spawn_element_entity(&mut world, "roof_system", "shed_roof_framing");

    // 3. Create a bears_on relation from roof to wall.
    spawn_bears_on_relation(&mut world, ElementId(roof_eid), ElementId(wall_eid));

    // 4. Promote the roof to Constructible.
    let result = handle_promote_refinement(
        &mut world,
        roof_eid,
        "Constructible".into(),
        Some("shed_roof_framing".into()),
        serde_json::json!({
            "footprint_polygon": [[0,0],[6000,0],[6000,8000],[0,8000]],
            "pitch_deg": 15,
            "high_plate_datum_mm": 2900,
            "low_plate_datum_mm": 2700
        }),
    )
    .expect("roof promotion should succeed");

    assert_eq!(result.previous_state, "Conceptual");
    assert_eq!(result.new_state, "Constructible");

    // 5. Inspect the resulting ObligationSet.
    let eid = ElementId(roof_eid);
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
        .expect("roof entity should have ObligationSet after promotion");

    // 5 class-min obligations, 0 recipe specializations = 5 total.
    assert_eq!(
        obligation_set.entries.len(),
        5,
        "primary_framing + sheathing + underlayment + finish + bears_on; got: {:?}",
        obligation_set
            .entries
            .iter()
            .map(|o| &o.id.0)
            .collect::<Vec<_>>()
    );

    // All 5 obligations must be satisfied.
    for obligation in &obligation_set.entries {
        assert!(
            matches!(obligation.status, ObligationStatus::SatisfiedBy(_)),
            "obligation '{}' should be SatisfiedBy, got: {:?}",
            obligation.id.0,
            obligation.status
        );
    }

    // Specifically: bears_on must be SatisfiedBy(wall_eid).
    let bears_on_ob = obligation_set
        .entries
        .iter()
        .find(|o| o.id.0 == "bears_on")
        .expect("obligation set must contain bears_on");

    assert!(
        matches!(
            &bears_on_ob.status,
            ObligationStatus::SatisfiedBy(weid) if *weid == wall_eid
        ),
        "bears_on must be SatisfiedBy(wall_eid={}); got: {:?}",
        wall_eid,
        bears_on_ob.status
    );
}

// ---------------------------------------------------------------------------
// Test: promote roof without bears_on relation → bears_on stays Unresolved,
//       validator emits a finding
// ---------------------------------------------------------------------------

#[test]
fn promote_roof_without_bears_on_relation_leaves_bears_on_unresolved() {
    let mut world = init_roof_test_world();

    // Spawn a roof entity with NO bears_on SemanticRelation.
    let roof_eid = spawn_element_entity(&mut world, "roof_system", "shed_roof_framing");

    // Promote to Constructible — no bears_on relation exists.
    handle_promote_refinement(
        &mut world,
        roof_eid,
        "Constructible".into(),
        Some("shed_roof_framing".into()),
        serde_json::json!({
            "footprint_polygon": [[0,0],[6000,0],[6000,8000],[0,8000]],
            "pitch_deg": 15,
        }),
    )
    .expect("roof promotion should succeed even without bears_on");

    // Inspect ObligationSet.
    let eid = ElementId(roof_eid);
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
        .expect("roof entity should have ObligationSet after promotion");

    // 4 obligations satisfied (by children), bears_on remains Unresolved.
    let satisfied_count = obligation_set
        .entries
        .iter()
        .filter(|o| matches!(o.status, ObligationStatus::SatisfiedBy(_)))
        .count();
    assert_eq!(
        satisfied_count,
        4,
        "exactly 4 child-spawned obligations should be SatisfiedBy; got: {:?}",
        obligation_set
            .entries
            .iter()
            .map(|o| (&o.id.0, &o.status))
            .collect::<Vec<_>>()
    );

    let bears_on_ob = obligation_set
        .entries
        .iter()
        .find(|o| o.id.0 == "bears_on")
        .expect("obligation set must contain bears_on");

    assert!(
        matches!(bears_on_ob.status, ObligationStatus::Unresolved),
        "bears_on obligation must remain Unresolved when no bears_on relation exists; got: {:?}",
        bears_on_ob.status
    );

    // Validator must emit an Error finding for the unresolved bears_on.
    let findings = handle_run_validation(&world, roof_eid).expect("run_validation should succeed");

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
