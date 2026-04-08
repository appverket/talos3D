pub mod assembly;
pub mod bsp_csg;
pub mod composite_solid;
pub mod csg;
pub mod definition;
pub mod editable_mesh;
pub mod generic_factory;
pub mod generic_snapshot;
pub mod group;
pub mod mesh_generation;
pub mod occurrence;
pub mod primitive_trait;
pub mod primitives;
pub mod profile;
pub mod profile_feature;
pub mod semantics;
pub mod snapshots;
pub mod tools;
pub mod triangulate;

use crate::{
    capability_registry::{
        CapabilityDescriptor, CapabilityDistribution, CapabilityRegistryAppExt, WorkbenchDescriptor,
    },
    importers::{dxf::DxfImporter, obj::ObjImporter},
    plugins::{
        command_registry::{
            activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
            CommandResult,
        },
        import::ImportRegistryAppExt,
        modeling::{
            csg::CsgFactory,
            definition::{DefinitionLibraryRegistry, DefinitionRegistry},
            generic_factory::PrimitiveFactory,
            group::{GroupEditContext, GroupFactory},
            occurrence::{
                evaluate_occurrences, propagate_definition_changes_with_commands,
                ChangedDefinitions, OccurrenceFactory,
            },
            primitives::{BoxPrimitive, CylinderPrimitive, PlanePrimitive},
            profile::{ProfileExtrusion, ProfileRevolve, ProfileSweep},
            profile_feature::FaceProfileFeatureFactory,
            snapshots::{EditableMeshFactory, PolylineFactory, TriangleMeshFactory},
        },
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
        tools::ActiveTool,
        transform::{start_transform_mode, TransformMode, TransformState},
    },
};
use bevy::{ecs::world::EntityRef, prelude::*};
use serde_json::Value;

pub struct ModelingPlugin;

#[derive(Resource, Default)]
pub struct ModelingWorkbench;

impl Plugin for ModelingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ModelingWorkbench>()
            .init_resource::<GroupEditContext>()
            .init_resource::<csg::CsgParentMap>()
            .init_resource::<DefinitionRegistry>()
            .init_resource::<DefinitionLibraryRegistry>()
            .init_resource::<ChangedDefinitions>()
            .register_workbench(
                WorkbenchDescriptor::new("modeling", "Modeling")
                    .with_description("Foundational geometric modeling capabilities")
                    .with_capabilities(["modeling"]),
            )
            .register_capability(
                CapabilityDescriptor::new("modeling", "Modeling")
                    .with_description("Geometric primitives and general-purpose modeling tools")
                    .with_distribution(CapabilityDistribution::Bundled),
            )
            .register_authored_entity_factory(GroupFactory)
            .register_authored_entity_factory(PrimitiveFactory::<BoxPrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<CylinderPrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<PlanePrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<ProfileExtrusion>::new())
            .register_authored_entity_factory(PrimitiveFactory::<ProfileSweep>::new())
            .register_authored_entity_factory(PrimitiveFactory::<ProfileRevolve>::new())
            .register_authored_entity_factory(PolylineFactory)
            .register_authored_entity_factory(TriangleMeshFactory)
            .register_authored_entity_factory(EditableMeshFactory)
            .register_authored_entity_factory(CsgFactory)
            .register_authored_entity_factory(FaceProfileFeatureFactory)
            .register_authored_entity_factory(OccurrenceFactory)
            .register_authored_entity_factory(assembly::AssemblyFactory)
            .register_authored_entity_factory(assembly::RelationFactory)
            .register_format_importer(ObjImporter)
            .register_format_importer(DxfImporter)
            .register_command(
                CommandDescriptor {
                    id: "modeling.group".to_string(),
                    label: "Group".to_string(),
                    description: "Group selected entities".to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+G".to_string()),
                    icon: Some("icon.group".to_string()),
                    hint: Some("Group the selected entities".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_group,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.ungroup".to_string(),
                    label: "Ungroup".to_string(),
                    description: "Ungroup the selected group".to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+Shift+G".to_string()),
                    icon: Some("icon.ungroup".to_string()),
                    hint: Some("Dissolve the selected group".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_ungroup,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_box".to_string(),
                    label: "Create Box".to_string(),
                    description: "Activate box placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("B".to_string()),
                    icon: Some("icon.create_box".to_string()),
                    hint: Some("Click to place a box center".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlaceBox".to_string()),
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_box,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_cylinder".to_string(),
                    label: "Create Cylinder".to_string(),
                    description: "Activate cylinder placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("C".to_string()),
                    icon: Some("icon.create_cylinder".to_string()),
                    hint: Some("Click to place a cylinder center".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlaceCylinder".to_string()),
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_cylinder,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_plane".to_string(),
                    label: "Create Plane".to_string(),
                    description: "Activate plane placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("P".to_string()),
                    icon: Some("icon.create_plane".to_string()),
                    hint: Some("Click two corners to place a plane".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlacePlane".to_string()),
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_plane,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_polyline".to_string(),
                    label: "Create Polyline".to_string(),
                    description: "Activate polyline placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("L".to_string()),
                    icon: Some("icon.create_polyline".to_string()),
                    hint: Some("Click to add points and press Enter to finish".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlacePolyline".to_string()),
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_polyline,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.move".to_string(),
                    label: "Move".to_string(),
                    description: "Begin move transform".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("G".to_string()),
                    icon: Some("icon.move".to_string()),
                    hint: Some("Begin move transform".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_begin_move,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.rotate".to_string(),
                    label: "Rotate".to_string(),
                    description: "Begin rotate transform".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("R".to_string()),
                    icon: Some("icon.rotate".to_string()),
                    hint: Some("Begin rotate transform".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_begin_rotate,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.scale".to_string(),
                    label: "Scale".to_string(),
                    description: "Begin scale transform".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("S".to_string()),
                    icon: Some("icon.scale".to_string()),
                    hint: Some("Begin scale transform".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_begin_scale,
            )
            .register_toolbar(ToolbarDescriptor {
                id: "modeling".to_string(),
                label: "Modeling".to_string(),
                default_dock: ToolbarDock::Left,
                default_visible: true,
                sections: vec![
                    ToolbarSection {
                        label: "Primitives".to_string(),
                        command_ids: vec![
                            "modeling.create_box".to_string(),
                            "modeling.create_cylinder".to_string(),
                            "modeling.create_plane".to_string(),
                            "modeling.create_polyline".to_string(),
                        ],
                    },
                    ToolbarSection {
                        label: "Transform".to_string(),
                        command_ids: vec![
                            "modeling.move".to_string(),
                            "modeling.rotate".to_string(),
                            "modeling.scale".to_string(),
                        ],
                    },
                ],
            })
            .register_command(
                CommandDescriptor {
                    id: "modeling.boolean_difference".to_string(),
                    label: "Boolean Difference".to_string(),
                    description: "Subtract second selection from first".to_string(),
                    category: CommandCategory::Create,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.boolean_difference".to_string()),
                    hint: Some("Select two entities, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_boolean_difference,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.boolean_union".to_string(),
                    label: "Boolean Union".to_string(),
                    description: "Combine two entities into one solid".to_string(),
                    category: CommandCategory::Create,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.boolean_union".to_string()),
                    hint: Some("Select two entities, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_boolean_union,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.boolean_intersection".to_string(),
                    label: "Boolean Intersection".to_string(),
                    description: "Keep only the overlapping volume".to_string(),
                    category: CommandCategory::Create,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.boolean_intersection".to_string()),
                    hint: Some("Select two entities, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_boolean_intersection,
            )
            .add_plugins(mesh_generation::ModelingMeshPlugin)
            .add_plugins(tools::ModelingToolPlugin)
            .add_systems(
                Update,
                (
                    propagate_definition_changes_with_commands,
                    csg::propagate_operand_changes,
                    csg::evaluate_csg_nodes,
                    profile_feature::propagate_feature_parent_changes,
                    profile_feature::evaluate_face_profile_features,
                    evaluate_occurrences,
                )
                    .chain()
                    .in_set(mesh_generation::EvaluationSet::Evaluate),
            );
    }
}

fn execute_create_box(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceBox)
}

fn execute_create_cylinder(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceCylinder)
}

fn execute_create_plane(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlacePlane)
}

fn execute_create_polyline(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlacePolyline)
}

fn execute_begin_move(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    start_transform_or_pend(world, TransformMode::Moving)
}

fn execute_begin_rotate(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    start_transform_or_pend(world, TransformMode::Rotating)
}

fn execute_begin_scale(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    start_transform_or_pend(world, TransformMode::Scaling)
}

fn start_transform_or_pend(
    world: &mut World,
    mode: TransformMode,
) -> Result<CommandResult, String> {
    match start_transform_mode(world, mode) {
        Ok(()) => Ok(CommandResult::empty()),
        Err(msg) if msg.contains("Cursor is not over") => {
            // Defer activation until cursor enters viewport
            world.resource_mut::<TransformState>().pending_mode = Some(mode);
            Ok(CommandResult::empty())
        }
        Err(msg) => Err(msg),
    }
}

fn execute_group(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::{
        commands::CreateEntityCommand,
        identity::{ElementId, ElementIdAllocator},
        selection::Selected,
    };

    let selected_ids: Vec<ElementId> = {
        let mut query = world.query_filtered::<&ElementId, With<Selected>>();
        query.iter(world).copied().collect()
    };

    if selected_ids.is_empty() {
        return Err("No entities selected to group".to_string());
    }

    let group_id = world.resource::<ElementIdAllocator>().next_id();
    let cached_bounds = group::compute_group_bounds_from_world(world, &selected_ids);
    let snapshot = group::GroupSnapshot {
        element_id: group_id,
        name: "Group".to_string(),
        member_ids: selected_ids,
        composite: None,
        cached_bounds,
    };

    // Apply immediately so the entity exists this frame for selection
    snapshot.apply_to(world);

    // Deselect members and select the new group
    let selected_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    let group_entity = {
        let mut q = world.try_query::<EntityRef>().unwrap();
        q.iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(group_id))
            .map(|e| e.id())
    };
    for entity in &selected_entities {
        world.entity_mut(*entity).remove::<Selected>();
    }
    if let Some(group_entity) = group_entity {
        world.entity_mut(group_entity).insert(Selected);
    }

    // Send event for undo/redo history
    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    Ok(CommandResult::empty())
}

fn execute_ungroup(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    use crate::plugins::{
        commands::ResolvedDeleteEntitiesCommand, identity::ElementId, selection::Selected,
    };

    let group_ids: Vec<ElementId> = {
        let mut query =
            world.query_filtered::<(&ElementId, &group::GroupMembers), With<Selected>>();
        query.iter(world).map(|(id, _)| *id).collect()
    };

    if group_ids.is_empty() {
        return Err("No groups selected to ungroup".to_string());
    }

    // Use ResolvedDeleteEntitiesCommand to skip cascade (don't delete members)
    world
        .resource_mut::<Messages<ResolvedDeleteEntitiesCommand>>()
        .write(ResolvedDeleteEntitiesCommand {
            element_ids: group_ids,
        });

    Ok(CommandResult::empty())
}

fn execute_boolean_difference(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    execute_boolean_op(world, bsp_csg::BooleanOp::Difference)
}

fn execute_boolean_union(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    execute_boolean_op(world, bsp_csg::BooleanOp::Union)
}

fn execute_boolean_intersection(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    execute_boolean_op(world, bsp_csg::BooleanOp::Intersection)
}

fn execute_boolean_op(world: &mut World, op: bsp_csg::BooleanOp) -> Result<CommandResult, String> {
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::{
        commands::CreateEntityCommand,
        identity::{ElementId, ElementIdAllocator},
        selection::Selected,
    };

    let selected_ids: Vec<ElementId> = {
        let mut query = world.query_filtered::<&ElementId, With<Selected>>();
        query.iter(world).copied().collect()
    };

    if selected_ids.len() != 2 {
        return Err(format!(
            "Boolean operations require exactly 2 selected entities, got {}",
            selected_ids.len()
        ));
    }

    let csg_id = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = csg::CsgSnapshot {
        element_id: csg_id,
        csg_node: csg::CsgNode {
            operand_a: selected_ids[0],
            operand_b: selected_ids[1],
            op,
        },
    };

    // Apply immediately
    snapshot.apply_to(world);

    // Deselect operands, select the CSG result
    let selected_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    let csg_entity = {
        let mut q = world.try_query::<EntityRef>().unwrap();
        q.iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(csg_id))
            .map(|e| e.id())
    };
    for entity in &selected_entities {
        world.entity_mut(*entity).remove::<Selected>();
    }
    if let Some(csg_entity) = csg_entity {
        world.entity_mut(csg_entity).insert(Selected);
    }

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    Ok(CommandResult::empty())
}
