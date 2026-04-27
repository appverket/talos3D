pub mod array;
pub mod assembly;
pub mod bim_material_assignment;
pub mod bsp_csg;
pub mod composite_solid;
pub mod csg;
pub mod definition;
pub mod editable_mesh;
pub mod exchange_identity;
pub mod fillet;
pub mod foundation;
pub mod generic_factory;
pub mod generic_snapshot;
pub mod group;
pub mod mesh_generation;
pub mod mirror;
pub mod occurrence;
pub mod primitive_trait;
pub mod primitives;
pub mod profile;
pub mod profile_feature;
pub mod property_sets;
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
        clipping_planes::ClipPlaneFactory,
        command_registry::{
            activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
            CommandResult,
        },
        definition_browser::{
            execute_create_definition_draft, execute_derive_definition_draft,
            execute_instantiate_definition, execute_open_definition_draft,
            execute_patch_definition_draft, execute_publish_definition_draft,
            execute_toggle_definitions_browser, DefinitionSelectionContext, DefinitionsWindowState,
        },
        handles::arm_move_handles,
        identity::ElementId,
        import::ImportRegistryAppExt,
        modeling::{
            csg::CsgFactory,
            definition::{DefinitionLibraryRegistry, DefinitionRegistry},
            fillet::{
                ChamferFactory, ChamferNode, ChamferSnapshot, FilletFactory, FilletNode,
                FilletSnapshot,
            },
            generic_factory::PrimitiveFactory,
            group::{GroupEditContext, GroupFactory},
            occurrence::{
                evaluate_occurrences, propagate_definition_changes_with_commands,
                ChangedDefinitions, OccurrenceFactory,
            },
            primitives::{BoxPrimitive, CylinderPrimitive, PlanePrimitive, SpherePrimitive},
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
        // PP70: domain-neutral refinement-linkage relations are the core
        // vocabulary for cross-state identity; register them here so any
        // app that boots ModelingPlugin has the full relation discovery
        // surface without needing to touch the architectural capability.
        crate::plugins::refinement::register_refinement_relations(app);
        crate::plugins::support_graph::register_support_graph_relations(app);
        app.init_resource::<ModelingWorkbench>()
            .init_resource::<GroupEditContext>()
            .init_resource::<DefinitionsWindowState>()
            .init_resource::<DefinitionSelectionContext>()
            .init_resource::<crate::plugins::definition_authoring::DefinitionDraftRegistry>()
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
            .register_authored_entity_factory(OccurrenceFactory)
            .register_authored_entity_factory(PrimitiveFactory::<BoxPrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<CylinderPrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<SpherePrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<PlanePrimitive>::new())
            .register_authored_entity_factory(PrimitiveFactory::<ProfileExtrusion>::new())
            .register_authored_entity_factory(PrimitiveFactory::<ProfileSweep>::new())
            .register_authored_entity_factory(PrimitiveFactory::<ProfileRevolve>::new())
            .register_authored_entity_factory(PolylineFactory)
            .register_authored_entity_factory(TriangleMeshFactory)
            .register_authored_entity_factory(EditableMeshFactory)
            .register_authored_entity_factory(CsgFactory)
            .register_authored_entity_factory(mirror::MirrorFactory)
            .register_authored_entity_factory(array::LinearArrayFactory)
            .register_authored_entity_factory(array::PolarArrayFactory)
            .register_authored_entity_factory(FilletFactory)
            .register_authored_entity_factory(ChamferFactory)
            .register_authored_entity_factory(FaceProfileFeatureFactory)
            .register_authored_entity_factory(assembly::AssemblyFactory)
            .register_authored_entity_factory(assembly::RelationFactory)
            .register_authored_entity_factory(ClipPlaneFactory)
            .register_authored_entity_factory(foundation::FoundationFactory)
            .register_command(
                CommandDescriptor {
                    id: "modeling.clip_plane_create".to_string(),
                    label: "Add Section View".to_string(),
                    description: "Add a horizontal section-view clipping plane at height 2m as drawing metadata".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Add a section-view cut without changing the authored model".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_clip_plane_create,
            )
            .register_format_importer(ObjImporter)
            .register_format_importer(DxfImporter)
            .register_command(
                CommandDescriptor {
                    id: "modeling.toggle_definitions_browser".to_string(),
                    label: "Definitions".to_string(),
                    description: "Show or hide the Definitions browser".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+Shift+D".to_string()),
                    icon: None,
                    hint: Some("Browse document and library definitions".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_toggle_definitions_browser,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.instantiate_definition".to_string(),
                    label: "Instantiate Definition".to_string(),
                    description: "Instantiate a definition from the document or a library"
                        .to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Create an occurrence from a selected definition".to_string()),
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_instantiate_definition,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_definition_draft".to_string(),
                    label: "New Definition Draft".to_string(),
                    description: "Create a new editable definition draft".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Create a new draft definition".to_string()),
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_definition_draft,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.open_definition_draft".to_string(),
                    label: "Edit Definition As Draft".to_string(),
                    description: "Open a definition as an editable draft".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Open the selected definition in the inspector".to_string()),
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_open_definition_draft,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.derive_definition_draft".to_string(),
                    label: "Derive Definition Draft".to_string(),
                    description: "Create a derived editable definition draft".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: None,
                    hint: Some(
                        "Derive a reusable variant from the selected definition".to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_derive_definition_draft,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.publish_definition_draft".to_string(),
                    label: "Publish Definition Draft".to_string(),
                    description: "Publish a definition draft into the document".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Validate and publish the active draft".to_string()),
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_publish_definition_draft,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.patch_definition_draft".to_string(),
                    label: "Patch Definition Draft".to_string(),
                    description: "Apply a patch operation to a definition draft".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Apply a draft mutation".to_string()),
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_patch_definition_draft,
            )
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
                    id: "modeling.create_sphere".to_string(),
                    label: "Create Sphere".to_string(),
                    description: "Activate sphere placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: Some("icon.create_sphere".to_string()),
                    hint: Some("Click to place a sphere centre".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlaceSphere".to_string()),
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_sphere,
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
                    description: "Arm move handles for the current selection".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: Some("G".to_string()),
                    icon: Some("icon.move".to_string()),
                    hint: Some("Drag a corner or control point to move".to_string()),
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
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_fillet".to_string(),
                    label: "Create Fillet".to_string(),
                    description: "Create a fillet feature from a source entity".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({
                        "type":"object",
                        "properties": {
                            "source_element_id": {"type":"integer"},
                            "radius": {"type":"number"},
                            "segments": {"type":"integer"}
                        }
                    })),
                    default_shortcut: None,
                    icon: Some("icon.create_fillet".to_string()),
                    hint: Some("Round the sharp edges of the selected source".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_fillet,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.create_chamfer".to_string(),
                    label: "Create Chamfer".to_string(),
                    description: "Create a chamfer feature from a source entity".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({
                        "type":"object",
                        "properties": {
                            "source_element_id": {"type":"integer"},
                            "distance": {"type":"number"}
                        }
                    })),
                    default_shortcut: None,
                    icon: Some("icon.create_chamfer".to_string()),
                    hint: Some("Bevel the sharp edges of the selected source".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_create_chamfer,
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
                            "modeling.create_sphere".to_string(),
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
                    ToolbarSection {
                        label: "Features".to_string(),
                        command_ids: vec![
                            "modeling.create_fillet".to_string(),
                            "modeling.create_chamfer".to_string(),
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
            .register_command(
                CommandDescriptor {
                    id: "modeling.mirror_create".to_string(),
                    label: "Mirror".to_string(),
                    description: "Create a mirror of the selected entity across a plane"
                        .to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: Some("icon.mirror".to_string()),
                    hint: Some("Select entity, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_mirror_create,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.mirror_copy".to_string(),
                    label: "Mirror Copy".to_string(),
                    description: "Create an independent mirrored copy of the selected entity"
                        .to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: Some("icon.mirror_copy".to_string()),
                    hint: Some("Select entity, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_mirror_copy,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.linear_array".to_string(),
                    label: "Linear Array".to_string(),
                    description: "Create a linear array of the selected entity".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: Some("icon.linear_array".to_string()),
                    hint: Some("Select entity, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_linear_array,
            )
            .register_command(
                CommandDescriptor {
                    id: "modeling.polar_array".to_string(),
                    label: "Polar Array".to_string(),
                    description: "Create a polar (rotational) array of the selected entity"
                        .to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({"type":"object"})),
                    default_shortcut: None,
                    icon: Some("icon.polar_array".to_string()),
                    hint: Some("Select entity, then apply".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("modeling".to_string()),
                },
                execute_polar_array,
            )
            .add_plugins(mesh_generation::ModelingMeshPlugin)
            .add_plugins(tools::ModelingToolPlugin)
            .add_systems(
                Update,
                (
                    propagate_definition_changes_with_commands,
                    csg::propagate_operand_changes,
                    csg::evaluate_csg_nodes,
                    mirror::propagate_mirror_source_changes,
                    mirror::evaluate_mirror_nodes,
                    array::propagate_array_source_changes,
                    array::evaluate_linear_array_nodes,
                    array::evaluate_polar_array_nodes,
                    fillet::propagate_fillet_source_changes,
                    fillet::evaluate_fillet_nodes,
                    fillet::evaluate_chamfer_nodes,
                    profile_feature::propagate_feature_parent_changes,
                    profile_feature::evaluate_face_profile_features,
                    evaluate_occurrences,
                )
                    .chain()
                    .in_set(mesh_generation::EvaluationSet::Evaluate),
            );

        // ADR-034 Foundation planting: install the terrain-version
        // counter and the Foundation evaluation system. This is part of
        // the modeling plugin so any app that boots ModelingPlugin
        // gets foundation support out of the box.
        app.add_plugins(foundation::FoundationPlugin);

        // ADR-026 Phase 6a: BIM property-set substrate. Registers the
        // `PropertySetSchemaRegistry` resource and the
        // `PropertySetChanged` event channel. The geometry pipeline
        // never observes property-set state — that structural
        // separation is the architectural enforcement of ADR-026 §1's
        // "property-set changes must never set mesh_dirty" invariant.
        app.add_plugins(property_sets::PropertySetsPlugin);

        // ADR-026 Phase 6d: BIM material assignment substrate. Lives
        // separately from the render-side `MaterialAssignment` so the
        // geometry / render pipeline never observes BIM authoring
        // state (layer function codes, ventilation flags, constituent
        // fractions). Mirrors the property-sets pattern.
        app.add_plugins(bim_material_assignment::BimMaterialAssignmentPlugin);
    }
}

fn execute_create_box(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceBox)
}

fn execute_create_cylinder(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceCylinder)
}

fn execute_create_sphere(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceSphere)
}

fn execute_create_plane(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlacePlane)
}

fn execute_create_polyline(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlacePolyline)
}

fn execute_begin_move(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    arm_move_handles(world)?;
    Ok(CommandResult::empty())
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

fn execute_create_fillet(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    create_feature_snapshot(world, fillet_snapshot_from_params(world, params)?.into())
}

fn execute_create_chamfer(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    create_feature_snapshot(world, chamfer_snapshot_from_params(world, params)?.into())
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

fn execute_mirror_create(world: &mut World, _params: &Value) -> Result<CommandResult, String> {
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

    if selected_ids.len() != 1 {
        return Err(format!(
            "Mirror requires exactly 1 selected entity, got {}",
            selected_ids.len()
        ));
    }

    let source_id = selected_ids[0];
    let mirror_id = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = mirror::MirrorSnapshot {
        element_id: mirror_id,
        mirror_node: mirror::MirrorNode {
            source: source_id,
            plane: mirror::MirrorPlane::xz(),
            merge: false,
        },
    };

    snapshot.apply_to(world);

    // Deselect source, select the new mirror node.
    let selected_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    let mirror_entity = {
        let mut q = world.try_query::<EntityRef>().unwrap();
        q.iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(mirror_id))
            .map(|e| e.id())
    };
    for entity in &selected_entities {
        world.entity_mut(*entity).remove::<Selected>();
    }
    if let Some(mirror_entity) = mirror_entity {
        world.entity_mut(mirror_entity).insert(Selected);
    }

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    Ok(CommandResult::empty())
}

fn execute_mirror_copy(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    // For now, mirror_copy creates a live mirror node (same as mirror_create).
    // Dissolving the link into an independent mesh can be added as a follow-up.
    execute_mirror_create(world, params)
}

fn execute_linear_array(world: &mut World, _params: &Value) -> Result<CommandResult, String> {
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

    if selected_ids.len() != 1 {
        return Err(format!(
            "Linear Array requires exactly 1 selected entity, got {}",
            selected_ids.len()
        ));
    }

    let source_id = selected_ids[0];
    let array_id = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = array::LinearArraySnapshot {
        element_id: array_id,
        node: array::LinearArrayNode {
            source: source_id,
            count: 3,
            spacing: Vec3::X * 1.0,
        },
    };
    snapshot.apply_to(world);

    let selected_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    let array_entity = {
        let mut q = world.try_query::<EntityRef>().unwrap();
        q.iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(array_id))
            .map(|e| e.id())
    };
    for entity in &selected_entities {
        world.entity_mut(*entity).remove::<Selected>();
    }
    if let Some(array_entity) = array_entity {
        world.entity_mut(array_entity).insert(Selected);
    }

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    Ok(CommandResult::empty())
}

fn execute_polar_array(world: &mut World, _params: &Value) -> Result<CommandResult, String> {
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

    if selected_ids.len() != 1 {
        return Err(format!(
            "Polar Array requires exactly 1 selected entity, got {}",
            selected_ids.len()
        ));
    }

    let source_id = selected_ids[0];
    let array_id = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = array::PolarArraySnapshot {
        element_id: array_id,
        node: array::PolarArrayNode {
            source: source_id,
            count: 4,
            axis: Vec3::Y,
            total_angle_degrees: 360.0,
            center: Vec3::ZERO,
        },
    };

    snapshot.apply_to(world);

    let selected_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    let array_entity = {
        let mut q = world.try_query::<EntityRef>().unwrap();
        q.iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(array_id))
            .map(|e| e.id())
    };
    for entity in &selected_entities {
        world.entity_mut(*entity).remove::<Selected>();
    }
    if let Some(array_entity) = array_entity {
        world.entity_mut(array_entity).insert(Selected);
    }

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    Ok(CommandResult::empty())
}

fn execute_clip_plane_create(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    use crate::plugins::{
        clipping_planes::{ClipPlaneNode, ClipPlaneSnapshot},
        commands::CreateEntityCommand,
        identity::ElementIdAllocator,
    };

    let element_id = world.resource::<ElementIdAllocator>().next_id();

    let snapshot = ClipPlaneSnapshot {
        element_id,
        node: ClipPlaneNode::at_y(2.0),
    };

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    Ok(CommandResult::empty())
}

fn create_feature_snapshot(
    world: &mut World,
    snapshot: crate::authored_entity::BoxedEntity,
) -> Result<CommandResult, String> {
    use crate::plugins::{commands::CreateEntityCommand, selection::Selected};

    let feature_id = snapshot.element_id();
    snapshot.apply_to(world);

    let selected_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    let feature_entity = {
        let mut query = world.try_query::<EntityRef>().unwrap();
        query
            .iter(world)
            .find(|entity| entity.get::<ElementId>().copied() == Some(feature_id))
            .map(|entity| entity.id())
    };
    for entity in &selected_entities {
        world.entity_mut(*entity).remove::<Selected>();
    }
    if let Some(feature_entity) = feature_entity {
        world.entity_mut(feature_entity).insert(Selected);
    }

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand { snapshot });

    Ok(CommandResult::empty())
}

fn fillet_snapshot_from_params(world: &World, params: &Value) -> Result<FilletSnapshot, String> {
    let source = feature_source_from_params(world, params)?;
    let radius = params.get("radius").and_then(Value::as_f64).unwrap_or(0.1) as f32;
    let segments = params
        .get("segments")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .max(1) as u32;
    Ok(FilletSnapshot {
        element_id: world
            .resource::<crate::plugins::identity::ElementIdAllocator>()
            .next_id(),
        fillet_node: FilletNode {
            source,
            radius,
            segments,
        },
    })
}

fn chamfer_snapshot_from_params(world: &World, params: &Value) -> Result<ChamferSnapshot, String> {
    let source = feature_source_from_params(world, params)?;
    let distance = params
        .get("distance")
        .and_then(Value::as_f64)
        .unwrap_or(0.1) as f32;
    Ok(ChamferSnapshot {
        element_id: world
            .resource::<crate::plugins::identity::ElementIdAllocator>()
            .next_id(),
        chamfer_node: ChamferNode { source, distance },
    })
}

fn feature_source_from_params(world: &World, params: &Value) -> Result<ElementId, String> {
    if let Some(source_id) = params.get("source_element_id").and_then(Value::as_u64) {
        return Ok(ElementId(source_id));
    }

    use crate::plugins::selection::Selected;

    let selected_ids: Vec<ElementId> = {
        let mut query = world
            .try_query_filtered::<&ElementId, With<Selected>>()
            .unwrap();
        query.iter(world).copied().collect()
    };
    if selected_ids.len() != 1 {
        return Err(
            "Fillet/chamfer creation requires exactly one selected source or source_element_id"
                .to_string(),
        );
    }
    Ok(selected_ids[0])
}
