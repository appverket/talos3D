#![cfg(feature = "model-api")]

use bevy::prelude::*;
use serde_json::json;
use talos3d_architectural::snapshots::{OpeningFactory, WallFactory};
use talos3d_core::{
    capability_registry::{AssemblyTypeDescriptor, CapabilityRegistry, RelationTypeDescriptor},
    plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        history::{History, PendingCommandQueue},
        identity::ElementIdAllocator,
        model_api::{
            handle_create_assembly, handle_create_entity, handle_delete_entities,
            handle_get_assembly, handle_list_assemblies, handle_list_assembly_members,
            handle_query_relations, list_entities, model_summary, AssemblyMemberRefRequest,
            CreateAssemblyRequest, CreateRelationRequest,
        },
        modeling::assembly::{AssemblyFactory, RelationFactory},
    },
};

fn init_assembly_test_world() -> World {
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
    registry.register_factory(WallFactory);
    registry.register_factory(OpeningFactory);
    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);
    registry.register_assembly_type(AssemblyTypeDescriptor {
        assembly_type: "house".into(),
        label: "House".into(),
        description: "A house".into(),
        expected_member_types: vec!["wall".into()],
        expected_member_roles: vec!["exterior_wall".into()],
        expected_relation_types: vec!["hosted_on".into()],
        parameter_schema: json!({}),
    });
    registry.register_assembly_type(AssemblyTypeDescriptor {
        assembly_type: "room".into(),
        label: "Room".into(),
        description: "A room".into(),
        expected_member_types: vec!["wall".into()],
        expected_member_roles: vec!["boundary".into()],
        expected_relation_types: vec![],
        parameter_schema: json!({}),
    });
    registry.register_relation_type(RelationTypeDescriptor {
        relation_type: "hosted_on".into(),
        label: "Hosted On".into(),
        description: "Hosted on".into(),
        valid_source_types: vec!["opening".into()],
        valid_target_types: vec!["wall".into()],
        parameter_schema: json!({}),
        participates_in_dependency_graph: true,
    });
    registry.register_relation_type(RelationTypeDescriptor {
        relation_type: "adjacent_to".into(),
        label: "Adjacent To".into(),
        description: "Adjacent to".into(),
        valid_source_types: vec![],
        valid_target_types: vec![],
        parameter_schema: json!({}),
        participates_in_dependency_graph: false,
    });
    registry.register_relation_type(RelationTypeDescriptor {
        relation_type: "supports".into(),
        label: "Supports".into(),
        description: "Supports".into(),
        valid_source_types: vec![],
        valid_target_types: vec![],
        parameter_schema: json!({}),
        participates_in_dependency_graph: false,
    });
    registry.register_relation_type(RelationTypeDescriptor {
        relation_type: "bounds".into(),
        label: "Bounds".into(),
        description: "Bounds".into(),
        valid_source_types: vec![],
        valid_target_types: vec![],
        parameter_schema: json!({}),
        participates_in_dependency_graph: false,
    });
    world.insert_resource(registry);
    world
}

#[test]
fn create_assembly_via_create_entity() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [5.0, 0.0],
            "height": 3.0,
            "thickness": 0.2
        }),
    )
    .expect("wall should be created");

    let assembly_id = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_assembly",
            "assembly_type": "house",
            "label": "Test House",
            "members": [
                {"target": wall_id, "role": "exterior_wall"}
            ],
            "parameters": {"num_floors": 1}
        }),
    )
    .expect("assembly should be created");

    let entities = list_entities(&world);
    assert!(entities.iter().any(|e| e.element_id == assembly_id));
    assert!(entities
        .iter()
        .any(|e| e.entity_type == "semantic_assembly"));
}

#[test]
fn create_relation_via_create_entity() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [5.0, 0.0],
            "height": 3.0,
            "thickness": 0.2
        }),
    )
    .expect("wall");

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
    .expect("opening");

    let relation_id = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_relation",
            "source": opening_id,
            "target": wall_id,
            "relation_type": "hosted_on",
            "parameters": {"position_along_wall": 0.5}
        }),
    )
    .expect("relation should be created");

    let entities = list_entities(&world);
    assert!(entities.iter().any(|e| e.element_id == relation_id));
}

#[test]
fn list_and_get_assemblies() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [5.0, 0.0],
            "height": 3.0,
            "thickness": 0.2
        }),
    )
    .unwrap();

    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "My House".into(),
            members: vec![AssemblyMemberRefRequest {
                target: wall_id,
                role: "exterior_wall".into(),
            }],
            parameters: json!({"num_floors": 2}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .expect("assembly creation should succeed");

    let assemblies = handle_list_assemblies(&world);
    assert_eq!(assemblies.len(), 1);
    assert_eq!(assemblies[0].assembly_type, "house");
    assert_eq!(assemblies[0].label, "My House");
    assert_eq!(assemblies[0].member_count, 1);

    let details = handle_get_assembly(&world, result.assembly_id).unwrap();
    assert_eq!(details.assembly_type, "house");
    assert_eq!(details.members.len(), 1);
    assert_eq!(details.members[0].role, "exterior_wall");
    assert_eq!(details.members[0].member_kind, "entity");
    assert_eq!(details.members[0].member_type, "wall");
    assert_eq!(details.parameters, json!({"num_floors": 2}));
}

#[test]
fn create_assembly_with_relations() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({
            "type": "wall",
            "start": [0.0, 0.0],
            "end": [5.0, 0.0],
            "height": 3.0,
            "thickness": 0.2
        }),
    )
    .unwrap();

    let opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "opening",
            "parent_wall_element_id": wall_id,
            "position_along_wall": 0.5,
            "kind": "door",
            "width": 0.9,
            "height": 2.1,
            "sill_height": 0.0
        }),
    )
    .unwrap();

    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![
                AssemblyMemberRefRequest {
                    target: wall_id,
                    role: "exterior_wall".into(),
                },
                AssemblyMemberRefRequest {
                    target: opening_id,
                    role: "front_door".into(),
                },
            ],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![CreateRelationRequest {
                source: opening_id,
                target: wall_id,
                relation_type: "hosted_on".into(),
                parameters: json!({"position_along_wall": 0.5}),
            }],
        },
    )
    .expect("assembly + relations should be created");

    assert_eq!(result.relation_ids.len(), 1);

    let relations = handle_query_relations(&world, None, None, Some("hosted_on".into()));
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].source, opening_id);
    assert_eq!(relations[0].target, wall_id);
}

#[test]
fn query_relations_filters() {
    let mut world = init_assembly_test_world();

    let w1 = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();
    let w2 = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [5,0], "end": [5,5], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

    // Create two relations
    handle_create_entity(
        &mut world,
        json!({"type": "semantic_relation", "source": w1, "target": w2, "relation_type": "adjacent_to"}),
    )
    .unwrap();
    handle_create_entity(
        &mut world,
        json!({"type": "semantic_relation", "source": w2, "target": w1, "relation_type": "supports"}),
    )
    .unwrap();

    // Query by source
    let by_source = handle_query_relations(&world, Some(w1), None, None);
    assert_eq!(by_source.len(), 1);
    assert_eq!(by_source[0].relation_type, "adjacent_to");

    // Query by target
    let by_target = handle_query_relations(&world, None, Some(w1), None);
    assert_eq!(by_target.len(), 1);
    assert_eq!(by_target[0].relation_type, "supports");

    // Query by type
    let by_type = handle_query_relations(&world, None, None, Some("supports".into()));
    assert_eq!(by_type.len(), 1);

    // Query all
    let all = handle_query_relations(&world, None, None, None);
    assert_eq!(all.len(), 2);
}

#[test]
fn model_summary_includes_assembly_and_relation_counts() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

    handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![AssemblyMemberRefRequest {
                target: wall_id,
                role: "exterior_wall".into(),
            }],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .unwrap();

    handle_create_entity(
        &mut world,
        json!({"type": "semantic_relation", "source": wall_id, "target": wall_id, "relation_type": "bounds"}),
    )
    .unwrap();

    let summary = model_summary(&world);
    assert_eq!(summary.assembly_counts.get("house"), Some(&1));
    assert_eq!(summary.relation_counts.get("bounds"), Some(&1));
    assert_eq!(summary.entity_counts.get("wall"), Some(&1));
}

#[test]
fn delete_assembly_does_not_delete_members() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![AssemblyMemberRefRequest {
                target: wall_id,
                role: "exterior_wall".into(),
            }],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .unwrap();

    // Delete just the assembly
    handle_delete_entities(&mut world, vec![result.assembly_id]).unwrap();

    // Assembly gone
    let assemblies = handle_list_assemblies(&world);
    assert!(assemblies.is_empty());

    // Wall still exists
    let entities = list_entities(&world);
    assert!(entities.iter().any(|e| e.element_id == wall_id));
}

#[test]
fn delete_entity_cascades_to_relations() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

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
    .unwrap();

    handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_relation",
            "source": opening_id,
            "target": wall_id,
            "relation_type": "hosted_on"
        }),
    )
    .unwrap();

    // Deleting wall should cascade to opening (ParentWall) and the relation
    handle_delete_entities(&mut world, vec![wall_id]).unwrap();

    let relations = handle_query_relations(&world, None, None, None);
    assert!(relations.is_empty(), "relation should be cascade-deleted");
}

#[test]
fn delete_member_repairs_assembly_membership() {
    let mut world = init_assembly_test_world();

    let w1 = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();
    let w2 = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [5,0], "end": [5,5], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![
                AssemblyMemberRefRequest {
                    target: w1,
                    role: "exterior_wall".into(),
                },
                AssemblyMemberRefRequest {
                    target: w2,
                    role: "exterior_wall".into(),
                },
            ],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .unwrap();

    // Delete one wall
    handle_delete_entities(&mut world, vec![w1]).unwrap();

    // Assembly still exists, but with only one member
    let details = handle_get_assembly(&world, result.assembly_id).unwrap();
    assert_eq!(details.members.len(), 1);
    assert_eq!(details.members[0].target, w2);
}

#[test]
fn list_assembly_members_enriches_info() {
    let mut world = init_assembly_test_world();

    let wall_id = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![AssemblyMemberRefRequest {
                target: wall_id,
                role: "exterior_wall".into(),
            }],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .unwrap();

    let members = handle_list_assembly_members(&world, result.assembly_id).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].member_kind, "entity");
    assert_eq!(members[0].member_type, "wall");
    assert_eq!(members[0].role, "exterior_wall");
}

#[test]
fn nested_assembly_member_kind() {
    let mut world = init_assembly_test_world();

    // Create a sub-assembly (room)
    let room_id = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_assembly",
            "assembly_type": "room",
            "label": "Kitchen",
            "members": []
        }),
    )
    .unwrap();

    // Create a parent assembly (house) containing the room
    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![AssemblyMemberRefRequest {
                target: room_id,
                role: "space".into(),
            }],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .unwrap();

    let members = handle_list_assembly_members(&world, result.assembly_id).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].member_kind, "assembly");
    assert_eq!(members[0].member_type, "room");
    assert_eq!(members[0].label, "Kitchen");
}

#[test]
fn persistence_dependency_ordering() {
    use talos3d_core::plugins::commands::snapshot_dependency_order_by_name;

    assert!(
        snapshot_dependency_order_by_name("semantic_assembly")
            > snapshot_dependency_order_by_name("wall")
    );
    assert!(
        snapshot_dependency_order_by_name("semantic_assembly")
            > snapshot_dependency_order_by_name("opening")
    );
    assert!(
        snapshot_dependency_order_by_name("semantic_relation")
            > snapshot_dependency_order_by_name("semantic_assembly")
    );
}

// --- Validation tests ---

#[test]
fn rejects_unknown_assembly_type() {
    let mut world = init_assembly_test_world();

    let err = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_assembly",
            "assembly_type": "spaceship",
            "label": "Apollo",
            "members": []
        }),
    )
    .expect_err("should reject unknown assembly type");
    assert!(
        err.contains("Unknown assembly type 'spaceship'"),
        "got: {err}"
    );
}

#[test]
fn rejects_unknown_relation_type() {
    let mut world = init_assembly_test_world();

    let w1 = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [0,0], "end": [5,0], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();
    let w2 = handle_create_entity(
        &mut world,
        json!({"type": "wall", "start": [5,0], "end": [5,5], "height": 3.0, "thickness": 0.2}),
    )
    .unwrap();

    let err = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_relation",
            "source": w1,
            "target": w2,
            "relation_type": "teleports_to"
        }),
    )
    .expect_err("should reject unknown relation type");
    assert!(
        err.contains("Unknown relation type 'teleports_to'"),
        "got: {err}"
    );
}

#[test]
fn rejects_assembly_with_nonexistent_member() {
    let mut world = init_assembly_test_world();

    let err = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_assembly",
            "assembly_type": "house",
            "label": "Ghost House",
            "members": [{"target": 99999, "role": "exterior_wall"}]
        }),
    )
    .expect_err("should reject nonexistent member target");
    assert!(err.contains("99999"), "got: {err}");
}

#[test]
fn rejects_relation_with_nonexistent_endpoints() {
    let mut world = init_assembly_test_world();

    let err = handle_create_entity(
        &mut world,
        json!({
            "type": "semantic_relation",
            "source": 88888,
            "target": 99999,
            "relation_type": "hosted_on"
        }),
    )
    .expect_err("should reject nonexistent source");
    assert!(err.contains("88888"), "got: {err}");
}

#[test]
fn create_assembly_tool_rejects_unknown_type() {
    let mut world = init_assembly_test_world();

    let err = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "fortress".into(),
            label: "Castle".into(),
            members: vec![],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .expect_err("should reject unknown type via create_assembly");
    assert!(
        err.contains("Unknown assembly type 'fortress'"),
        "got: {err}"
    );
}

#[test]
fn create_assembly_tool_rejects_nonexistent_member() {
    let mut world = init_assembly_test_world();

    let err = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "House".into(),
            members: vec![AssemblyMemberRefRequest {
                target: 77777,
                role: "exterior_wall".into(),
            }],
            parameters: json!({}),
            metadata: json!({}),
            relations: vec![],
        },
    )
    .expect_err("should reject nonexistent member");
    assert!(err.contains("77777"), "got: {err}");
}
