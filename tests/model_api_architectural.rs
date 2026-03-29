#![cfg(feature = "model-api")]

use bevy::prelude::*;
use serde_json::json;
use talos3d_architectural::{
    components::{BimData, Opening, OpeningKind, ParentWall, Wall},
    snapshots::{OpeningFactory, WallFactory},
};
use talos3d_core::{
    capability_registry::CapabilityRegistry,
    plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        history::{History, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        model_api::{
            get_entity_snapshot, handle_create_entity, handle_delete_entities, handle_list_handles,
            handle_set_property, handle_transform, list_entities, model_summary,
            TransformToolRequest,
        },
    },
};

fn init_architectural_test_world() -> World {
    let mut world = World::new();
    world.insert_resource(Messages::<CreateBoxCommand>::default());
    world.insert_resource(Messages::<CreateCylinderCommand>::default());
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
    registry.register_factory(WallFactory);
    registry.register_factory(OpeningFactory);
    world.insert_resource(registry);
    world
}

#[test]
fn architectural_entities_appear_in_list_and_summary() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(WallFactory);
    registry.register_factory(OpeningFactory);
    world.insert_resource(registry);

    let wall_entity = world
        .spawn((
            ElementId(1),
            Wall {
                start: Vec2::ZERO,
                end: Vec2::new(5.0, 0.0),
                height: 3.0,
                thickness: 0.2,
            },
            BimData::default(),
        ))
        .id();
    world.spawn((
        ElementId(2),
        Opening {
            width: 1.2,
            height: 1.5,
            sill_height: 0.9,
            kind: OpeningKind::Window,
        },
        ParentWall {
            wall_entity,
            position_along_wall: 0.4,
        },
        BimData::default(),
    ));

    let entities = list_entities(&world);
    assert_eq!(entities.len(), 2);
    assert_eq!(entities[0].entity_type, "wall");
    assert_eq!(entities[1].entity_type, "opening");

    let summary = model_summary(&world);
    assert_eq!(summary.entity_counts.get("wall"), Some(&1));
    assert_eq!(summary.entity_counts.get("opening"), Some(&1));
    assert_eq!(
        summary.metrics.get("total_wall_length"),
        Some(&serde_json::json!(5.0))
    );
    assert_eq!(
        summary.metrics.get("total_opening_count"),
        Some(&serde_json::json!(1))
    );
    let wall_openings: std::collections::HashMap<String, Vec<u64>> =
        serde_json::from_value(summary.metrics.get("wall_openings").cloned().unwrap()).unwrap();
    assert_eq!(wall_openings.get("1"), Some(&vec![2]));
}

#[test]
fn architectural_write_handlers_preserve_parent_child_behavior() {
    let mut world = init_architectural_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [4.0, 0.0],
            "height": 3.0,
            "thickness": 0.2
        }),
    )
    .expect("wall should be created");
    let opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "opening",
            "parent_wall_element_id": wall_id,
            "position_along_wall": 0.5,
            "kind": "window",
            "width": 1.0,
            "height": 1.2,
            "sill_height": 0.9
        }),
    )
    .expect("opening should be created");

    let transformed = handle_transform(
        &mut world,
        TransformToolRequest {
            element_ids: vec![wall_id],
            operation: "move".to_string(),
            axis: Some("X".to_string()),
            value: json!(2.5),
        },
    )
    .expect("wall transform should succeed");
    assert_eq!(transformed.len(), 1);

    let wall_snapshot =
        get_entity_snapshot(&world, ElementId(wall_id)).expect("wall snapshot should exist");
    assert_eq!(wall_snapshot["Wall"]["wall"]["start"], json!([2.5, 0.0]));
    assert_eq!(wall_snapshot["Wall"]["wall"]["end"], json!([6.5, 0.0]));

    let handles = handle_list_handles(&world, wall_id).expect("wall handles should exist");
    assert_eq!(handles.len(), 3);

    let transformed = handle_transform(
        &mut world,
        TransformToolRequest {
            element_ids: vec![wall_id, opening_id],
            operation: "move".to_string(),
            axis: Some("X".to_string()),
            value: json!(2.0),
        },
    )
    .expect("combined transform should succeed");
    assert_eq!(transformed.len(), 1);

    let opening_snapshot =
        get_entity_snapshot(&world, ElementId(opening_id)).expect("opening snapshot should exist");
    assert_eq!(
        opening_snapshot["Opening"]["position_along_wall"],
        json!(0.5)
    );

    let deleted_count =
        handle_delete_entities(&mut world, vec![wall_id]).expect("delete should cascade");
    assert_eq!(deleted_count, 2);
    assert!(get_entity_snapshot(&world, ElementId(wall_id)).is_none());
    assert!(get_entity_snapshot(&world, ElementId(opening_id)).is_none());
}

#[test]
fn architectural_set_property_validates_wall_fields() {
    let mut world = init_architectural_test_world();
    let wall_id = handle_create_entity(
        &mut world,
        json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [3.0, 0.0],
            "height": 3.0,
            "thickness": 0.2
        }),
    )
    .expect("wall should be created");

    let updated = handle_set_property(&mut world, wall_id, "height", json!(4.5))
        .expect("setting wall height should succeed");
    assert_eq!(updated["Wall"]["wall"]["height"], json!(4.5));

    let error = handle_set_property(&mut world, wall_id, "radius", json!(1.0))
        .expect_err("invalid wall property should fail");
    assert!(error.contains("Valid properties: start, end, height, thickness"));
}
