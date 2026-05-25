use std::collections::HashMap;

#[cfg(feature = "model-api")]
use bevy::window::PrimaryWindow;
use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
#[cfg(feature = "model-api")]
use serde_json::json;
use serde_json::Value;

use crate::authored_entity::{BoxedEntity, PropertyValueKind};
use crate::capability_registry::CapabilityRegistry;
#[cfg(feature = "model-api")]
use crate::curation::api::{DraftMaterialSpecRequest, ListMaterialSpecsFilter, MaterialSpecInfo};
use crate::curation::MaterialSpecBody;
#[cfg(feature = "model-api")]
use crate::plugins::authoring_guidance::AuthoringGuidance;
#[cfg(feature = "model-api")]
use crate::plugins::command_registry::{
    CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
};
#[cfg(feature = "model-api")]
use crate::plugins::hosting_contracts::{
    HostAffectedRegion, HostingCheckId, HostingCheckStatus, HostingContractKindId,
    HostingValidationCheck, HostingValidationRequest, HostingValidationResult,
    HostingValidationStatus, MeasuredValue,
};
use crate::plugins::identity::ElementId;
#[cfg(feature = "model-api")]
use crate::plugins::identity::ElementIdAllocator;
use crate::plugins::materials::MaterialAssignment;
use crate::plugins::modeling::group::{GroupEditContext, GroupMembers};
#[cfg(feature = "model-api")]
use crate::plugins::modeling::occurrence::HostedAnchor;
use crate::plugins::modeling::semantics::{geometry_semantics_for_snapshot, GeometrySemantics};
#[cfg(feature = "model-api")]
use crate::plugins::render_pipeline::RenderSettings;
#[cfg(feature = "model-api")]
use crate::plugins::render_pipeline::RenderTonemapping;
#[cfg(feature = "model-api")]
use crate::plugins::{
    camera::{
        apply_orbit_state, focus_orbit_camera_on_bounds,
        perspective_distance_to_orthographic_scale, CameraProjectionMode, OrbitCamera,
    },
    commands::{
        find_entity_by_element_id, queue_command_events, ApplyEntityChangesCommand,
        BeginCommandGroup, CreateEntityCommand, DeleteEntitiesCommand, EndCommandGroup,
        ResolvedDeleteEntitiesCommand,
    },
    document_properties::DocumentProperties,
    history::{apply_pending_history_commands, HistorySet},
    import::{import_file_now, ImportRegistry, ImporterDescriptor},
    layers::{LayerAssignment, LayerRegistry, LayerState},
    lighting::{
        create_daylight_rig, scene_light_object_exposed, SceneLightNode, SceneLightingSettings,
    },
    materials::{
        normalize_material_textures, MaterialDef, MaterialRegistry, TextureRef, TextureRegistry,
    },
    named_views::NamedViewRegistry,
    persistence::{load_project_from_path, save_project_to_path},
    selection::Selected,
    toolbar::{
        update_toolbar_layout_entry, ToolbarDock, ToolbarLayoutState, ToolbarRegistry,
        ToolbarSection,
    },
};

#[cfg(feature = "model-api")]
use std::{
    env, fs,
    net::TcpListener as StdTcpListener,
    path::{Path, PathBuf},
    sync::{mpsc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "model-api")]
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    schemars::JsonSchema,
    tool, tool_handler, tool_router, transport, ErrorData as McpError, ServerHandler, ServiceExt,
};

#[cfg(feature = "model-api")]
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

#[cfg(feature = "model-api")]
use tokio::sync::oneshot;
#[cfg(feature = "model-api")]
use tokio::time::{sleep, Duration};

#[cfg(feature = "model-api")]
pub struct ModelApiPlugin;

#[cfg(feature = "model-api")]
const CMD_MODEL_API_CREATE_ENTITY: &str = "modeling.create_entity_direct";
#[cfg(feature = "model-api")]
const CMD_MODEL_API_CREATE_BOX: &str = "modeling.create_box_direct";
#[cfg(feature = "model-api")]
const CMD_MODEL_API_DELETE_ENTITIES: &str = "core.delete_entities";
#[cfg(feature = "model-api")]
const CMD_MODEL_API_TRANSFORM_ENTITIES: &str = "modeling.transform_entities";
#[cfg(feature = "model-api")]
const CMD_MODEL_API_SET_ENTITY_PROPERTY: &str = "core.set_entity_property";

#[cfg(feature = "model-api")]
impl Plugin for ModelApiPlugin {
    fn build(&self, app: &mut App) {
        let (runtime_info, http_listener) = match resolve_model_api_runtime() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("failed to configure model API runtime: {error}");
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        app.insert_resource(ModelApiReceiver(Mutex::new(receiver)));
        app.insert_resource(runtime_info.clone());
        // Ensure the Semantic Procedural Session substrate (ADR-051) is
        // installed so `procedural_session.*` tools have resources to
        // operate on. Idempotent — `ProceduralSessionMcpPlugin` adds
        // `ProceduralSessionPlugin` only if not already present.
        app.add_plugins(crate::plugins::procedural_session_mcp::ProceduralSessionMcpPlugin);
        // Ensure the parametric component substrate (ParametricRegistry +
        // ParametricStore) exists so the `parametric.*` tools have resources
        // to operate on. Idempotent — domain plugins populate the registry.
        app.add_plugins(crate::plugins::parametric_mcp::ParametricMcpPlugin);
        // Live UX automation enters through Bevy's input messages, not model
        // mutation APIs, so agents can verify the same viewport paths users
        // exercise with pointer and keyboard input.
        app.add_plugins(crate::plugins::ux_harness::UxHarnessPlugin);
        // Register agent-grade direct mutation primitives as hidden commands.
        // The public low-level MCP mutation tools call these commands and
        // return CommandResult; normal UI surfaces stay on interactive commands.
        register_model_api_primitive_commands(app);
        // Register the Model API tools a session may commit, so `eval`
        // accepts them and commit can route them to real geometry.
        {
            let mut session_tools =
                app.world_mut()
                    .resource_mut::<crate::curation::procedural_session::SessionToolRegistry>();
            register_model_api_session_tools(&mut session_tools);
        }
        app.add_systems(Update, poll_model_api_requests.before(HistorySet::Queue));
        app.add_systems(Startup, annotate_window_title_with_model_api_instance);
        spawn_model_api_server(sender, runtime_info, http_listener);
    }
}

#[cfg(feature = "model-api")]
fn register_model_api_primitive_commands(app: &mut App) {
    fn hidden_command(
        id: &str,
        label: &str,
        description: &str,
        category: CommandCategory,
        parameters: Value,
    ) -> CommandDescriptor {
        CommandDescriptor {
            id: id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            category,
            parameters: Some(parameters),
            default_shortcut: None,
            icon: None,
            hint: None,
            requires_selection: false,
            show_in_menu: false,
            version: 1,
            activates_tool: None,
            capability_id: Some("model-api".to_string()),
        }
    }

    app.register_command(
        hidden_command(
            CMD_MODEL_API_CREATE_ENTITY,
            "Create Entity Direct",
            "Create an authored entity from a typed JSON payload.",
            CommandCategory::Create,
            json!({
                "type": "object",
                "required": ["type"],
                "additionalProperties": true
            }),
        ),
        execute_model_api_create_entity_command,
    )
    .register_command(
        hidden_command(
            CMD_MODEL_API_CREATE_BOX,
            "Create Box Direct",
            "Create a box from explicit dimensions and transform values.",
            CommandCategory::Create,
            json!({
                "type": "object",
                "properties": {
                    "center": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                    "centre": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                    "half_extents": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                    "size": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                    "rotation": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4}
                }
            }),
        ),
        execute_model_api_create_box_command,
    )
    .register_command(
        hidden_command(
            CMD_MODEL_API_DELETE_ENTITIES,
            "Delete Entities Direct",
            "Delete explicit authored entities by element id.",
            CommandCategory::Edit,
            json!({
                "type": "object",
                "required": ["element_ids"],
                "properties": {
                    "element_ids": {"type": "array", "items": {"type": "integer"}}
                }
            }),
        ),
        execute_model_api_delete_entities_command,
    )
    .register_command(
        hidden_command(
            CMD_MODEL_API_TRANSFORM_ENTITIES,
            "Transform Entities Direct",
            "Apply an explicit move, rotate, or scale operation to authored entities.",
            CommandCategory::Edit,
            json!({
                "type": "object",
                "required": ["element_ids", "operation", "axis", "value"],
                "properties": {
                    "element_ids": {"type": "array", "items": {"type": "integer"}},
                    "operation": {"type": "string"},
                    "axis": {"type": ["string", "null"]},
                    "value": {}
                }
            }),
        ),
        execute_model_api_transform_entities_command,
    )
    .register_command(
        hidden_command(
            CMD_MODEL_API_SET_ENTITY_PROPERTY,
            "Set Entity Property Direct",
            "Set one JSON-authored property on an explicit entity.",
            CommandCategory::Edit,
            json!({
                "type": "object",
                "required": ["element_id", "property_name", "value"],
                "properties": {
                    "element_id": {"type": "integer"},
                    "property_name": {"type": "string"},
                    "value": {}
                }
            }),
        ),
        execute_model_api_set_entity_property_command,
    );
}

#[cfg(any(feature = "model-api", test))]
mod types;
#[cfg(any(feature = "model-api", test))]
pub use types::*;

pub fn get_editing_context(world: &World) -> EditingContextInfo {
    let edit_context = world.resource::<GroupEditContext>();
    EditingContextInfo {
        is_root: edit_context.is_root(),
        stack: edit_context
            .stack
            .iter()
            .filter_map(|id| {
                let mut q = world.try_query::<EntityRef>().unwrap();
                let entity = q
                    .iter(world)
                    .find(|e| e.get::<ElementId>().copied() == Some(*id))?;
                let name = entity
                    .get::<GroupMembers>()
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                Some(EditingContextEntry {
                    element_id: id.0,
                    name,
                })
            })
            .collect(),
        breadcrumb: edit_context.breadcrumb(world),
    }
}

pub fn list_entities(world: &World) -> Vec<EntityEntry> {
    let mut entries = Vec::new();
    let registry = world.resource::<CapabilityRegistry>();

    let mut q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in q.iter(world) {
        if let Some(element_id) = entity_ref.get::<ElementId>() {
            if crate::plugins::refinement::is_parked_refinement_entity(world, *element_id) {
                continue;
            }
        }
        let Some(snapshot) = registry.capture_user_facing_snapshot(&entity_ref, world) else {
            continue;
        };

        entries.push(EntityEntry {
            element_id: snapshot.element_id().0,
            entity_type: snapshot.type_name().to_string(),
            label: snapshot.label(),
        });
    }

    entries.sort_by_key(|entry| entry.element_id);
    entries
}

pub fn get_entity_snapshot(world: &World, element_id: ElementId) -> Option<serde_json::Value> {
    capture_entity_snapshot(world, element_id).map(|snapshot| snapshot.to_json())
}

pub fn get_entity_details(world: &World, element_id: ElementId) -> Option<EntityDetails> {
    let snapshot = capture_entity_snapshot(world, element_id)?;
    Some(entity_details_from_snapshot(world, &snapshot))
}

pub fn model_summary(world: &World) -> ModelSummary {
    let summary = world
        .resource::<CapabilityRegistry>()
        .build_model_summary(world);
    ModelSummary {
        entity_counts: summary.entity_counts,
        assembly_counts: summary.assembly_counts,
        relation_counts: summary.relation_counts,
        bounding_box: bounding_box_from_points(&summary.bounding_points),
        metrics: summary.metrics,
    }
}

#[cfg(feature = "model-api")]
pub fn list_toolbars(world: &World) -> Vec<ToolbarDetails> {
    let Some(registry) = world.get_resource::<ToolbarRegistry>() else {
        return Vec::new();
    };
    let Some(layout_state) = world.get_resource::<ToolbarLayoutState>() else {
        return Vec::new();
    };
    toolbar_details_from_resources(registry, layout_state)
}

fn capture_entity_snapshot(
    world: &World,
    element_id: ElementId,
) -> Option<crate::authored_entity::BoxedEntity> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|entity_ref| entity_ref.get::<ElementId>().copied() == Some(element_id))?;
    if crate::plugins::refinement::is_parked_refinement_entity(world, element_id) {
        return None;
    }
    world
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&entity_ref, world)
}

fn entity_details_from_snapshot(world: &World, snapshot: &BoxedEntity) -> EntityDetails {
    EntityDetails {
        element_id: snapshot.element_id().0,
        entity_type: snapshot.type_name().to_string(),
        label: snapshot.label(),
        snapshot: snapshot.to_json(),
        geometry_semantics: geometry_semantics_for_snapshot(world, snapshot),
        semantic: semantic_details_for_entity(world, snapshot.element_id()),
        properties: snapshot
            .property_fields()
            .into_iter()
            .map(|field| EntityPropertyDetails {
                name: field.name.to_string(),
                label: field.label.to_string(),
                kind: property_kind_name(&field.kind).to_string(),
                value: field
                    .value
                    .as_ref()
                    .map_or(serde_json::Value::Null, |value| value.to_json()),
                editable: field.editable,
            })
            .collect(),
    }
}

fn semantic_details_for_entity(
    world: &World,
    element_id: ElementId,
) -> Option<EntitySemanticDetails> {
    use crate::capability_registry::ElementClassAssignment;
    use crate::plugins::refinement::{
        AuthoringProvenance, RefinementStateComponent, SemanticIntent,
    };

    let mut q = world.try_query::<EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|entity_ref| entity_ref.get::<ElementId>().copied() == Some(element_id))?;

    let assignment = entity_ref.get::<ElementClassAssignment>();
    let refinement_state = entity_ref.get::<RefinementStateComponent>();
    let semantic_intent = entity_ref.get::<SemanticIntent>();
    let provenance = entity_ref.get::<AuthoringProvenance>();

    if assignment.is_none()
        && refinement_state.is_none()
        && semantic_intent.is_none()
        && provenance.is_none()
    {
        return None;
    }

    let semantic_roles = assignment
        .and_then(|assignment| {
            world
                .get_resource::<CapabilityRegistry>()
                .and_then(|registry| registry.element_class_descriptor(&assignment.element_class))
        })
        .map(|descriptor| {
            descriptor
                .semantic_roles
                .iter()
                .map(|role| role.0.clone())
                .collect()
        })
        .unwrap_or_default();

    Some(EntitySemanticDetails {
        element_class: assignment.map(|assignment| assignment.element_class.0.clone()),
        semantic_roles,
        refinement_state: refinement_state.map(|state| state.state.as_str().to_string()),
        parameters: semantic_intent
            .map(|intent| intent.parameters.clone())
            .unwrap_or(serde_json::Value::Null),
        unresolved_decisions: semantic_intent
            .map(|intent| intent.unresolved_decisions.clone())
            .unwrap_or_default(),
        source_refs: semantic_intent
            .map(|intent| intent.source_refs.clone())
            .unwrap_or_default(),
        authoring_rationale: provenance.and_then(|provenance| provenance.rationale.clone()),
    })
}

#[cfg(feature = "model-api")]
fn toolbar_details_from_resources(
    registry: &ToolbarRegistry,
    layout_state: &ToolbarLayoutState,
) -> Vec<ToolbarDetails> {
    let mut toolbars = registry
        .toolbars()
        .filter_map(|descriptor| {
            let entry = layout_state.entries.get(&descriptor.id)?;
            Some(ToolbarDetails {
                id: descriptor.id.clone(),
                label: descriptor.label.clone(),
                dock: entry.dock.as_str().to_string(),
                order: entry.order,
                visible: entry.visible,
                sections: descriptor
                    .sections
                    .iter()
                    .map(toolbar_section_details)
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    toolbars.sort_by(|left, right| {
        left.dock
            .cmp(&right.dock)
            .then(left.order.cmp(&right.order))
            .then(left.label.cmp(&right.label))
    });
    toolbars
}

#[cfg(feature = "model-api")]
fn toolbar_section_details(section: &ToolbarSection) -> ToolbarSectionDetails {
    ToolbarSectionDetails {
        label: section.label.clone(),
        command_ids: section.command_ids.clone(),
    }
}

fn property_kind_name(kind: &PropertyValueKind) -> &'static str {
    match kind {
        PropertyValueKind::Scalar => "scalar",
        PropertyValueKind::Vec2 => "vec2",
        PropertyValueKind::Vec3 => "vec3",
        PropertyValueKind::Text => "text",
    }
}

fn bounding_box_from_points(points: &[Vec3]) -> Option<BoundingBox> {
    let first = points.first().copied()?;
    let (min, max) = points
        .iter()
        .copied()
        .fold((first, first), |(min, max), point| {
            (min.min(point), max.max(point))
        });

    Some(BoundingBox {
        min: [min.x, min.y, min.z],
        max: [max.x, max.y, max.z],
    })
}

#[cfg(feature = "model-api")]
mod request;
#[cfg(all(feature = "model-api", test))]
use request::handle_model_api_request;
#[cfg(feature = "model-api")]
use request::{poll_model_api_requests, ApiResult, ModelApiReceiver, ModelApiRequest};

#[cfg(feature = "model-api")]
#[path = "model_api/transport.rs"]
mod runtime_transport;
#[cfg(feature = "model-api")]
use runtime_transport::{
    annotate_window_title_with_model_api_instance, resolve_model_api_runtime,
    spawn_model_api_server,
};

#[cfg(feature = "model-api")]
mod server;
#[cfg(feature = "model-api")]
pub use server::*;

#[cfg(feature = "model-api")]
fn handle_enter_group(world: &mut World, element_id: u64) -> Result<EditingContextInfo, String> {
    let eid = ElementId(element_id);
    // Verify the entity is a group
    let mut q = world.try_query::<EntityRef>().unwrap();
    let is_group = q
        .iter(world)
        .any(|e| e.get::<ElementId>().copied() == Some(eid) && e.get::<GroupMembers>().is_some());
    if !is_group {
        return Err(format!("Entity {element_id} is not a group"));
    }
    let mut edit_context = world.resource::<GroupEditContext>().clone();
    edit_context.enter(eid);
    world.insert_resource(edit_context);
    Ok(get_editing_context(world))
}

#[cfg(feature = "model-api")]
fn handle_exit_group(world: &mut World) -> Result<EditingContextInfo, String> {
    let mut edit_context = world.resource::<GroupEditContext>().clone();
    edit_context.exit();
    world.insert_resource(edit_context);
    Ok(get_editing_context(world))
}

#[cfg(feature = "model-api")]
fn handle_list_group_members(
    world: &World,
    element_id: u64,
) -> Result<Vec<GroupMemberEntry>, String> {
    let eid = ElementId(element_id);
    let mut q = world.try_query::<EntityRef>().unwrap();
    let members = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .and_then(|e| e.get::<GroupMembers>().cloned())
        .ok_or_else(|| format!("Entity {element_id} is not a group"))?;

    let registry = world.resource::<CapabilityRegistry>();
    let entries: Vec<GroupMemberEntry> = members
        .member_ids
        .iter()
        .filter_map(|member_id| {
            let mut q = world.try_query::<EntityRef>().unwrap();
            let entity_ref = q
                .iter(world)
                .find(|e| e.get::<ElementId>().copied() == Some(*member_id))?;
            let snapshot = registry.capture_snapshot(&entity_ref, world)?;
            Some(GroupMemberEntry {
                element_id: member_id.0,
                entity_type: snapshot.type_name().to_string(),
                label: snapshot.label(),
                is_group: entity_ref.get::<GroupMembers>().is_some(),
            })
        })
        .collect();
    Ok(entries)
}

// --- Layer Management Handlers ---

#[cfg(feature = "model-api")]
fn handle_list_layers(world: &World) -> Vec<LayerInfo> {
    let registry = world.resource::<LayerRegistry>();
    let state = world.resource::<LayerState>();
    registry
        .sorted_layers()
        .into_iter()
        .map(|def| LayerInfo {
            name: def.name.clone(),
            visible: def.visible,
            locked: def.locked,
            color: def.color,
            active: def.name == state.active_layer,
        })
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_set_layer_visibility(
    world: &mut World,
    name: &str,
    visible: bool,
) -> Result<Vec<LayerInfo>, String> {
    {
        let mut registry = world.resource_mut::<LayerRegistry>();
        let def = registry
            .layers
            .get_mut(name)
            .ok_or_else(|| format!("Layer '{name}' not found"))?;
        def.visible = visible;
    }
    Ok(handle_list_layers(world))
}

#[cfg(feature = "model-api")]
fn handle_set_layer_locked(
    world: &mut World,
    name: &str,
    locked: bool,
) -> Result<Vec<LayerInfo>, String> {
    {
        let mut registry = world.resource_mut::<LayerRegistry>();
        let def = registry
            .layers
            .get_mut(name)
            .ok_or_else(|| format!("Layer '{name}' not found"))?;
        def.locked = locked;
    }
    Ok(handle_list_layers(world))
}

#[cfg(feature = "model-api")]
fn handle_assign_layer(
    world: &mut World,
    element_id: u64,
    layer_name: &str,
) -> Result<Vec<LayerInfo>, String> {
    // Ensure layer exists
    world
        .resource_mut::<LayerRegistry>()
        .ensure_layer(layer_name);

    let entity = find_entity_by_element_id(world, ElementId(element_id))
        .ok_or_else(|| format!("Entity not found: {element_id}"))?;

    // Insert or update LayerAssignment
    world
        .entity_mut(entity)
        .insert(LayerAssignment::new(layer_name));

    Ok(handle_list_layers(world))
}

#[cfg(feature = "model-api")]
fn handle_create_layer(world: &mut World, name: &str) -> Result<Vec<LayerInfo>, String> {
    {
        let mut registry = world.resource_mut::<LayerRegistry>();
        if registry.layers.contains_key(name) {
            return Err(format!("Layer '{name}' already exists"));
        }
        registry.create_layer(name.to_string());
    }
    Ok(handle_list_layers(world))
}

#[cfg(feature = "model-api")]
fn handle_rename_layer(
    world: &mut World,
    old_name: &str,
    new_name: &str,
) -> Result<Vec<LayerInfo>, String> {
    world
        .resource_mut::<LayerRegistry>()
        .rename_layer(old_name, new_name.to_string())?;

    // Re-home every object that referenced the old layer name.
    let to_update: Vec<Entity> = {
        let mut query = world.query::<(Entity, &LayerAssignment)>();
        query
            .iter(world)
            .filter(|(_, assignment)| assignment.layer == old_name)
            .map(|(entity, _)| entity)
            .collect()
    };
    for entity in to_update {
        world
            .entity_mut(entity)
            .insert(LayerAssignment::new(new_name));
    }

    {
        let mut state = world.resource_mut::<LayerState>();
        if state.active_layer == old_name {
            state.active_layer = new_name.to_string();
        }
    }

    Ok(handle_list_layers(world))
}

#[cfg(feature = "model-api")]
fn handle_delete_layer(world: &mut World, name: &str) -> Result<Vec<LayerInfo>, String> {
    // Move any objects on this layer back to Default before removing it, so no
    // assignment dangles to a non-existent layer.
    let to_update: Vec<Entity> = {
        let mut query = world.query::<(Entity, &LayerAssignment)>();
        query
            .iter(world)
            .filter(|(_, assignment)| assignment.layer == name)
            .map(|(entity, _)| entity)
            .collect()
    };

    world.resource_mut::<LayerRegistry>().delete_layer(name)?;

    for entity in to_update {
        world
            .entity_mut(entity)
            .insert(LayerAssignment::default_layer());
    }

    {
        let mut state = world.resource_mut::<LayerState>();
        if state.active_layer == name {
            state.active_layer = crate::plugins::layers::DEFAULT_LAYER_NAME.to_string();
        }
    }

    Ok(handle_list_layers(world))
}

// --- Named View Handlers ---

#[cfg(feature = "model-api")]
fn named_view_orthographic_scale(view: &crate::plugins::named_views::NamedView) -> f32 {
    view.orthographic_scale.unwrap_or_else(|| {
        if view.projection_mode == CameraProjectionMode::Isometric {
            view.radius.max(0.05)
        } else {
            perspective_distance_to_orthographic_scale(view.radius, view.focal_length_mm)
        }
    })
}

#[cfg(feature = "model-api")]
fn camera_projection_name(mode: CameraProjectionMode) -> String {
    match mode {
        CameraProjectionMode::Perspective => "perspective".to_string(),
        CameraProjectionMode::Isometric => "orthographic".to_string(),
    }
}

#[cfg(feature = "model-api")]
fn named_view_info_from_view(view: &crate::plugins::named_views::NamedView) -> NamedViewInfo {
    NamedViewInfo {
        name: view.name.clone(),
        description: view.description.clone(),
        focus: view.focus,
        radius: view.radius,
        orthographic_scale: named_view_orthographic_scale(view),
        yaw: view.yaw,
        pitch: view.pitch,
        projection: camera_projection_name(view.projection_mode),
        focal_length_mm: view.focal_length_mm,
    }
}

#[cfg(feature = "model-api")]
fn projection_mode_from_str(s: &str) -> Result<CameraProjectionMode, String> {
    match s.to_lowercase().as_str() {
        "perspective" => Ok(CameraProjectionMode::Perspective),
        "orthographic" | "isometric" => Ok(CameraProjectionMode::Isometric),
        other => Err(format!(
            "Unknown projection '{other}'. Expected 'perspective' or 'orthographic'."
        )),
    }
}

/// Snapshot of live `OrbitCamera` state we can read without keeping a borrow.
#[cfg(feature = "model-api")]
struct LiveCameraSnapshot {
    focus: bevy::math::Vec3,
    radius: f32,
    orthographic_scale: f32,
    yaw: f32,
    pitch: f32,
    projection_mode: CameraProjectionMode,
    focal_length_mm: f32,
}

#[cfg(feature = "model-api")]
fn camera_state_info_from_live(snapshot: &LiveCameraSnapshot) -> CameraStateInfo {
    CameraStateInfo {
        focus: snapshot.focus.into(),
        radius: snapshot.radius,
        orthographic_scale: snapshot.orthographic_scale,
        yaw: snapshot.yaw,
        pitch: snapshot.pitch,
        projection: camera_projection_name(snapshot.projection_mode),
        focal_length_mm: snapshot.focal_length_mm,
    }
}

#[cfg(feature = "model-api")]
fn live_camera_snapshot(world: &World) -> LiveCameraSnapshot {
    let mut q = world.try_query::<&OrbitCamera>().unwrap();
    if let Some(orbit) = q.iter(world).next() {
        LiveCameraSnapshot {
            focus: orbit.focus,
            radius: orbit.radius,
            orthographic_scale: orbit.orthographic_scale,
            yaw: orbit.yaw,
            pitch: orbit.pitch,
            projection_mode: orbit.projection_mode,
            focal_length_mm: orbit.focal_length_mm,
        }
    } else {
        let default = OrbitCamera::default();
        LiveCameraSnapshot {
            focus: default.focus,
            radius: default.radius,
            orthographic_scale: default.orthographic_scale,
            yaw: default.yaw,
            pitch: default.pitch,
            projection_mode: default.projection_mode,
            focal_length_mm: default.focal_length_mm,
        }
    }
}

/// Build an `OrbitCamera` from optional `CameraParams`, falling back to the live camera state.
#[cfg(feature = "model-api")]
fn orbit_from_camera_params(
    world: &World,
    params: Option<&CameraParams>,
) -> Result<OrbitCamera, String> {
    let live = live_camera_snapshot(world);

    let Some(params) = params else {
        return Ok(OrbitCamera {
            focus: live.focus,
            radius: live.radius,
            orthographic_scale: live.orthographic_scale,
            yaw: live.yaw,
            pitch: live.pitch,
            projection_mode: live.projection_mode,
            focal_length_mm: live.focal_length_mm,
        });
    };

    let projection_mode = if let Some(ref proj) = params.projection {
        projection_mode_from_str(proj)?
    } else {
        live.projection_mode
    };

    Ok(OrbitCamera {
        focus: params
            .focus
            .map(bevy::math::Vec3::from)
            .unwrap_or(live.focus),
        radius: params.radius.unwrap_or(live.radius),
        orthographic_scale: params.orthographic_scale.unwrap_or(live.orthographic_scale),
        yaw: params.yaw.unwrap_or(live.yaw),
        pitch: params.pitch.unwrap_or(live.pitch),
        projection_mode,
        focal_length_mm: params.focal_length_mm.unwrap_or(live.focal_length_mm),
    })
}

#[cfg(feature = "model-api")]
fn handle_view_list(world: &World) -> Vec<NamedViewInfo> {
    world
        .resource::<NamedViewRegistry>()
        .list()
        .iter()
        .map(named_view_info_from_view)
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_view_save(
    world: &mut World,
    name: String,
    description: Option<String>,
    camera_params: Option<CameraParams>,
) -> Result<NamedViewInfo, String> {
    let orbit = orbit_from_camera_params(world, camera_params.as_ref())?;
    let mut view = crate::plugins::named_views::NamedView::from_orbit(&name, &orbit);
    view.description = description;

    world
        .resource_mut::<NamedViewRegistry>()
        .save(view.clone())?;

    Ok(named_view_info_from_view(&view))
}

#[cfg(feature = "model-api")]
fn handle_view_restore(world: &mut World, name: String) -> Result<NamedViewInfo, String> {
    // Read everything we need from the registry while we hold the immutable borrow.
    let (orbit_state, view_info) = {
        let registry = world.resource::<NamedViewRegistry>();
        let view = registry
            .get(&name)
            .ok_or_else(|| format!("No view named '{name}' exists"))?;
        (view.to_orbit(), named_view_info_from_view(view))
    };

    // Apply directly to the camera entity — borrow released above.
    let mut q = world.query::<(&mut OrbitCamera, &mut Transform, &mut Projection)>();
    if let Some((mut orbit, mut transform, mut projection)) = q.iter_mut(world).next() {
        *orbit = orbit_state;
        apply_orbit_state(&orbit, &mut transform, &mut projection);
    }

    Ok(view_info)
}

#[cfg(feature = "model-api")]
fn handle_view_update(
    world: &mut World,
    name: String,
    new_name: Option<String>,
    description: Option<String>,
    camera_params: Option<CameraParams>,
) -> Result<NamedViewInfo, String> {
    // Resolve camera params against the current live camera or existing view.
    let orbit = if camera_params.is_some() {
        orbit_from_camera_params(world, camera_params.as_ref())?
    } else {
        // Keep existing camera params from the stored view.
        world
            .resource::<NamedViewRegistry>()
            .get(&name)
            .ok_or_else(|| format!("No view named '{name}' exists"))?
            .to_orbit()
    };

    {
        let mut registry = world.resource_mut::<NamedViewRegistry>();
        let view = registry
            .get_mut(&name)
            .ok_or_else(|| format!("No view named '{name}' exists"))?;

        view.focus = orbit.focus.into();
        view.radius = orbit.radius;
        view.orthographic_scale = Some(orbit.orthographic_scale);
        view.yaw = orbit.yaw;
        view.pitch = orbit.pitch;
        view.projection_mode = orbit.projection_mode;
        view.focal_length_mm = orbit.focal_length_mm;

        if let Some(ref desc) = description {
            view.description = Some(desc.clone());
        }
    }

    // Rename if requested.
    if let Some(ref target_name) = new_name {
        world
            .resource_mut::<NamedViewRegistry>()
            .rename(&name, target_name)?;
    }

    let final_name = new_name.as_deref().unwrap_or(&name);
    let registry = world.resource::<NamedViewRegistry>();
    let view = registry
        .get(final_name)
        .ok_or_else(|| format!("View '{final_name}' not found after update"))?;
    Ok(named_view_info_from_view(view))
}

#[cfg(feature = "model-api")]
fn handle_view_delete(world: &mut World, name: String) -> Result<(), String> {
    world.resource_mut::<NamedViewRegistry>().delete(&name)
}

// --- Clipping Plane Handlers ---

#[cfg(feature = "model-api")]
fn clip_plane_info_from_world(world: &World, element_id: ElementId) -> Option<ClipPlaneInfo> {
    use crate::plugins::clipping_planes::ClipPlaneNode;

    let mut q = world.try_query::<(&ElementId, &ClipPlaneNode)>().unwrap();
    q.iter(world).find_map(|(eid, node)| {
        (*eid == element_id).then(|| ClipPlaneInfo {
            element_id: eid.0,
            name: node.name.clone(),
            origin: node.origin.into(),
            normal: node.normal.into(),
            active: node.active,
        })
    })
}

#[cfg(feature = "model-api")]
fn handle_clip_plane_list(world: &World) -> Vec<ClipPlaneInfo> {
    use crate::plugins::clipping_planes::ClipPlaneNode;

    let mut q = world.try_query::<(&ElementId, &ClipPlaneNode)>().unwrap();
    q.iter(world)
        .map(|(eid, node)| ClipPlaneInfo {
            element_id: eid.0,
            name: node.name.clone(),
            origin: node.origin.into(),
            normal: node.normal.into(),
            active: node.active,
        })
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_clip_plane_create(
    world: &mut World,
    name: String,
    origin: [f32; 3],
    normal: [f32; 3],
    active: bool,
) -> ApiResult<u64> {
    use crate::plugins::clipping_planes::{ClipPlaneNode, ClipPlaneSnapshot};

    let element_id = world
        .resource::<crate::plugins::identity::ElementIdAllocator>()
        .next_id();

    let snapshot = ClipPlaneSnapshot {
        element_id,
        node: ClipPlaneNode {
            name,
            origin: bevy::math::Vec3::from(origin),
            normal: bevy::math::Vec3::from(normal),
            active,
        },
    };

    send_event(
        world,
        crate::plugins::commands::CreateEntityCommand {
            snapshot: snapshot.into(),
        },
    );
    flush_model_api_write_pipeline(world);

    clip_plane_info_from_world(world, element_id)
        .map(|_| element_id.0)
        .ok_or_else(|| "Failed to create clipping plane entity".to_string())
}

#[cfg(feature = "model-api")]
fn handle_clip_plane_update(
    world: &mut World,
    element_id: u64,
    name: Option<String>,
    origin: Option<[f32; 3]>,
    normal: Option<[f32; 3]>,
    active: Option<bool>,
) -> ApiResult<ClipPlaneInfo> {
    use serde_json::json;

    ensure_entity_exists(world, ElementId(element_id))?;

    // Apply each supplied field via set_property.
    if let Some(n) = name {
        handle_set_property(world, element_id, "name", json!(n))?;
    }
    if let Some([x, y, z]) = origin {
        handle_set_property(world, element_id, "origin_x", json!(x))?;
        handle_set_property(world, element_id, "origin_y", json!(y))?;
        handle_set_property(world, element_id, "origin_z", json!(z))?;
    }
    if let Some([x, y, z]) = normal {
        handle_set_property(world, element_id, "normal_x", json!(x))?;
        handle_set_property(world, element_id, "normal_y", json!(y))?;
        handle_set_property(world, element_id, "normal_z", json!(z))?;
    }
    if let Some(a) = active {
        handle_set_property(
            world,
            element_id,
            "active",
            json!(if a { "true" } else { "false" }),
        )?;
    }

    clip_plane_info_from_world(world, ElementId(element_id))
        .ok_or_else(|| format!("Clipping plane {element_id} not found after update"))
}

#[cfg(feature = "model-api")]
fn handle_clip_plane_toggle(
    world: &mut World,
    element_id: u64,
    active: bool,
) -> ApiResult<ClipPlaneInfo> {
    handle_clip_plane_update(world, element_id, None, None, None, Some(active))
}

// --- Material Handlers ---

#[cfg(feature = "model-api")]
fn handle_list_materials(world: &World) -> Vec<MaterialInfo> {
    let texture_registry = world.resource::<TextureRegistry>();
    world
        .resource::<MaterialRegistry>()
        .all()
        .map(|def| MaterialInfo::from_def(def, texture_registry))
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_get_material(world: &World, id: &str) -> Result<MaterialInfo, String> {
    let texture_registry = world.resource::<TextureRegistry>();
    world
        .resource::<MaterialRegistry>()
        .get(id)
        .map(|def| MaterialInfo::from_def(def, texture_registry))
        .ok_or_else(|| format!("Material '{id}' not found"))
}

#[cfg(feature = "model-api")]
fn handle_create_material(
    world: &mut World,
    req: CreateMaterialRequest,
) -> Result<MaterialInfo, String> {
    let mut def = material_def_from_request(req);
    if let Some(mut textures) = world.get_resource_mut::<TextureRegistry>() {
        normalize_material_textures(&mut def, &mut textures);
    }
    let id = def.id.clone();
    world.resource_mut::<MaterialRegistry>().upsert(def);
    handle_get_material(world, &id)
}

#[cfg(feature = "model-api")]
fn handle_update_material(
    world: &mut World,
    id: &str,
    req: CreateMaterialRequest,
) -> Result<MaterialInfo, String> {
    let mut def = world
        .resource::<MaterialRegistry>()
        .get(id)
        .cloned()
        .ok_or_else(|| format!("Material '{id}' not found"))?;
    apply_request_to_def(req, &mut def);
    if let Some(mut textures) = world.get_resource_mut::<TextureRegistry>() {
        normalize_material_textures(&mut def, &mut textures);
    }
    world.resource_mut::<MaterialRegistry>().upsert(def);
    handle_get_material(world, id)
}

#[cfg(feature = "model-api")]
fn handle_delete_material(world: &mut World, id: &str) -> Result<String, String> {
    // Remove or downgrade assignments that explicitly reference this render material.
    let assignment_updates: Vec<(Entity, Option<MaterialAssignment>)> = {
        let mut q = world.query::<(Entity, &MaterialAssignment)>();
        q.iter(world)
            .filter(|(_, assignment)| assignment.contains_explicit_render_material_id(id))
            .map(|(entity, assignment)| {
                (entity, assignment.without_explicit_render_material_id(id))
            })
            .collect()
    };
    for (entity, assignment) in assignment_updates {
        let mut entity_mut = world.entity_mut(entity);
        if let Some(assignment) = assignment {
            entity_mut.insert(assignment);
        } else {
            entity_mut.remove::<MaterialAssignment>();
        }
    }
    world
        .resource_mut::<MaterialRegistry>()
        .remove(id)
        .ok_or_else(|| format!("Material '{id}' not found"))?;
    Ok(id.to_string())
}

#[cfg(feature = "model-api")]
fn handle_apply_material(world: &mut World, req: ApplyMaterialRequest) -> Result<Vec<u64>, String> {
    if !world
        .resource::<MaterialRegistry>()
        .contains(&req.material_id)
    {
        return Err(format!("Material '{}' not found", req.material_id));
    }
    let mut applied = Vec::new();
    for &eid in &req.element_ids {
        let entity = find_entity_by_element_id(world, ElementId(eid))
            .ok_or_else(|| format!("Entity {eid} not found"))?;
        world
            .entity_mut(entity)
            .insert(MaterialAssignment::new(req.material_id.clone()));
        applied.push(eid);
    }
    Ok(applied)
}

#[cfg(feature = "model-api")]
fn handle_assign_material(
    world: &mut World,
    req: AssignMaterialRequest,
) -> Result<AssignMaterialResponse, String> {
    if req.element_ids.is_empty() {
        return Err("assign_material requires at least one element_id".to_string());
    }

    let requested_id = req
        .material_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);

    let has_material_payload = assign_material_request_has_material_payload(&req);
    let (material_id, created_material) = if has_material_payload {
        let mut def = MaterialDef::new(
            req.name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Assigned Material".to_string()),
        );
        if let Some(id) = requested_id {
            def.id = id;
        }
        if let Some(base_color) = req.base_color {
            def.base_color = base_color;
        }
        if let Some(perceptual_roughness) = req.perceptual_roughness {
            def.perceptual_roughness = perceptual_roughness;
        }
        if let Some(metallic) = req.metallic {
            def.metallic = metallic;
        }
        apply_assign_material_texture_refs(&req, &mut def)?;
        let material_id = def.id.clone();
        let created_material = !world.resource::<MaterialRegistry>().contains(&material_id);
        if let Some(mut textures) = world.get_resource_mut::<TextureRegistry>() {
            normalize_material_textures(&mut def, &mut textures);
        }
        world.resource_mut::<MaterialRegistry>().upsert(def);
        (material_id, created_material)
    } else {
        let material_id = requested_id
            .ok_or_else(|| "assign_material requires material_id or base_color".to_string())?;
        if !world.resource::<MaterialRegistry>().contains(&material_id) {
            return Err(format!("Material '{material_id}' not found"));
        }
        (material_id, false)
    };

    let assignments = handle_set_material_assignment(
        world,
        SetMaterialAssignmentRequest {
            element_ids: req.element_ids,
            assignment: MaterialAssignment::new(material_id.clone()),
        },
    )?;

    Ok(AssignMaterialResponse {
        material_id,
        created_material,
        assignments,
    })
}

#[cfg(feature = "model-api")]
fn assign_material_request_has_material_payload(req: &AssignMaterialRequest) -> bool {
    req.base_color.is_some()
        || req.perceptual_roughness.is_some()
        || req.metallic.is_some()
        || req.base_color_texture.is_some()
        || req.normal_map_texture.is_some()
        || req.metallic_roughness_texture.is_some()
        || req.emissive_texture.is_some()
        || req.occlusion_texture.is_some()
}

#[cfg(feature = "model-api")]
fn apply_assign_material_texture_refs(
    req: &AssignMaterialRequest,
    def: &mut MaterialDef,
) -> Result<(), String> {
    def.base_color_texture =
        assign_material_texture_ref(req.base_color_texture.as_ref(), "base_color_texture")?;
    def.normal_map_texture =
        assign_material_texture_ref(req.normal_map_texture.as_ref(), "normal_map_texture")?;
    def.metallic_roughness_texture = assign_material_texture_ref(
        req.metallic_roughness_texture.as_ref(),
        "metallic_roughness_texture",
    )?;
    def.emissive_texture =
        assign_material_texture_ref(req.emissive_texture.as_ref(), "emissive_texture")?;
    def.occlusion_texture =
        assign_material_texture_ref(req.occlusion_texture.as_ref(), "occlusion_texture")?;
    Ok(())
}

#[cfg(feature = "model-api")]
fn assign_material_texture_ref(
    texture: Option<&AssignMaterialTextureRef>,
    field: &str,
) -> Result<Option<TextureRef>, String> {
    let Some(texture) = texture else {
        return Ok(None);
    };
    match (&texture.asset, &texture.embedded) {
        (Some(asset), None) => {
            let path = asset.path.trim();
            if path.is_empty() {
                Err(format!("{field}.asset.path must be non-empty"))
            } else {
                Ok(Some(TextureRef::AssetPath {
                    path: path.to_string(),
                }))
            }
        }
        (None, Some(embedded)) => {
            if embedded.data.is_empty() {
                Err(format!("{field}.embedded.data must be non-empty"))
            } else {
                Ok(Some(TextureRef::Embedded {
                    data: embedded.data.clone(),
                    mime: embedded
                        .mime
                        .clone()
                        .unwrap_or_else(|| "image/png".to_string()),
                }))
            }
        }
        (None, None) => Err(format!("{field} requires asset or embedded")),
        (Some(_), Some(_)) => Err(format!("{field} accepts only one of asset or embedded")),
    }
}

#[cfg(feature = "model-api")]
fn handle_remove_material(world: &mut World, element_ids: Vec<u64>) -> Result<Vec<u64>, String> {
    let mut removed = Vec::new();
    for eid in element_ids {
        let entity = find_entity_by_element_id(world, ElementId(eid))
            .ok_or_else(|| format!("Entity {eid} not found"))?;
        world.entity_mut(entity).remove::<MaterialAssignment>();
        removed.push(eid);
    }
    Ok(removed)
}

#[cfg(feature = "model-api")]
fn handle_get_material_assignment(
    world: &World,
    element_id: u64,
) -> Result<EntityMaterialAssignmentInfo, String> {
    let entity = find_entity_by_element_id_readonly(world, ElementId(element_id))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;
    Ok(EntityMaterialAssignmentInfo {
        element_id,
        assignment: world.entity(entity).get::<MaterialAssignment>().cloned(),
    })
}

#[cfg(feature = "model-api")]
fn handle_set_material_assignment(
    world: &mut World,
    request: SetMaterialAssignmentRequest,
) -> Result<Vec<EntityMaterialAssignmentInfo>, String> {
    crate::plugins::materials::validate_material_assignment(world, &request.assignment)?;

    let mut updated = Vec::new();
    for element_id in request.element_ids {
        let entity = find_entity_by_element_id(world, ElementId(element_id))
            .ok_or_else(|| format!("Entity {element_id} not found"))?;
        world.entity_mut(entity).insert(request.assignment.clone());
        updated.push(EntityMaterialAssignmentInfo {
            element_id,
            assignment: Some(request.assignment.clone()),
        });
    }
    Ok(updated)
}

#[cfg(feature = "model-api")]
fn parse_bim_material_ref(
    value: &str,
) -> Result<crate::plugins::modeling::bim_material_assignment::BimMaterialRef, String> {
    let id = value.trim();
    if id.is_empty() {
        return Err("material id must be non-empty".to_string());
    }
    Ok(crate::plugins::modeling::bim_material_assignment::BimMaterialRef::new(id))
}

#[cfg(feature = "model-api")]
fn parse_bim_layer_function(
    value: Option<&str>,
) -> Result<crate::plugins::modeling::bim_material_assignment::LayerFunction, String> {
    use crate::plugins::modeling::bim_material_assignment::LayerFunction;

    match value
        .unwrap_or("other")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "structural" => Ok(LayerFunction::Structural),
        "insulation" => Ok(LayerFunction::Insulation),
        "finish" => Ok(LayerFunction::Finish),
        "membrane" => Ok(LayerFunction::Membrane),
        "air" => Ok(LayerFunction::Air),
        "other" => Ok(LayerFunction::Other),
        other => Err(format!("Unsupported BIM material layer function '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn build_bim_layered_assignment(
    request: &BimMaterialAssignLayeredRequest,
) -> Result<crate::plugins::modeling::bim_material_assignment::BimMaterialAssignment, String> {
    use crate::plugins::modeling::bim_material_assignment::{
        BimMaterialAssignment, BimMaterialLayer, BimMaterialLayerSet,
    };

    if request.layers.is_empty() {
        return Err("layers must be non-empty".to_string());
    }
    let mut layers = Vec::with_capacity(request.layers.len());
    for layer in &request.layers {
        if !layer.thickness_m.is_finite() || layer.thickness_m <= 0.0 {
            return Err("layer thickness_m must be finite and greater than zero".to_string());
        }
        let mut parsed =
            BimMaterialLayer::new(parse_bim_material_ref(&layer.material)?, layer.thickness_m)
                .with_function(parse_bim_layer_function(layer.function.as_deref())?);
        if layer.is_ventilated.unwrap_or(false) {
            parsed = parsed.ventilated();
        }
        if let Some(label) = layer
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parsed = parsed.with_label(label);
        }
        layers.push(parsed);
    }
    let mut set = BimMaterialLayerSet::new(layers);
    if let Some(param) = request
        .total_thickness_param
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        set = set.with_total_thickness_param(param);
    }
    Ok(BimMaterialAssignment::LayerSet(set))
}

#[cfg(feature = "model-api")]
fn build_bim_constituent_assignment(
    request: &BimMaterialAssignConstituentsRequest,
) -> Result<crate::plugins::modeling::bim_material_assignment::BimMaterialAssignment, String> {
    use crate::plugins::modeling::bim_material_assignment::{
        BimMaterialAssignment, BimMaterialConstituent, BimMaterialConstituentSet,
    };

    if request.constituents.is_empty() {
        return Err("constituents must be non-empty".to_string());
    }
    let mut constituents = Vec::with_capacity(request.constituents.len());
    for constituent in &request.constituents {
        let mut parsed = BimMaterialConstituent::new(
            parse_bim_material_ref(&constituent.material)?,
            constituent.fraction,
        );
        if let Some(label) = constituent
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parsed = parsed.with_label(label);
        }
        constituents.push(parsed);
    }
    let set = BimMaterialConstituentSet::new(constituents);
    if !set.is_well_formed(1.0e-6) {
        return Err(
            "constituent fractions must be finite, within [0, 1], non-empty, and sum to 1.0"
                .to_string(),
        );
    }
    Ok(BimMaterialAssignment::ConstituentSet(set))
}

#[cfg(feature = "model-api")]
enum BimMaterialTarget {
    Definition(crate::plugins::modeling::definition::DefinitionId),
    Occurrence {
        element_id: u64,
        entity: Entity,
        definition_id: crate::plugins::modeling::definition::DefinitionId,
    },
}

#[cfg(feature = "model-api")]
fn resolve_bim_material_target(
    world: &World,
    definition_id: Option<&str>,
    element_id: Option<u64>,
) -> Result<BimMaterialTarget, String> {
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};
    use crate::plugins::modeling::occurrence::OccurrenceIdentity;

    match (definition_id, element_id) {
        (Some(_), Some(_)) | (None, None) => {
            Err("provide exactly one of definition_id or element_id".to_string())
        }
        (Some(definition_id), None) => {
            let id = definition_id.trim();
            if id.is_empty() {
                return Err("definition_id must be non-empty".to_string());
            }
            let id = DefinitionId(id.to_string());
            world
                .resource::<DefinitionRegistry>()
                .get(&id)
                .ok_or_else(|| format!("Definition '{}' not found", id))?;
            Ok(BimMaterialTarget::Definition(id))
        }
        (None, Some(element_id)) => {
            let entity = find_entity_by_element_id_readonly(world, ElementId(element_id))
                .ok_or_else(|| format!("Entity {element_id} not found"))?;
            let identity = world
                .entity(entity)
                .get::<OccurrenceIdentity>()
                .ok_or_else(|| {
                    format!("Entity {element_id} is not an occurrence and cannot carry a BIM material override")
                })?;
            world
                .resource::<DefinitionRegistry>()
                .get(&identity.definition_id)
                .ok_or_else(|| format!("Definition '{}' not found", identity.definition_id))?;
            Ok(BimMaterialTarget::Occurrence {
                element_id,
                entity,
                definition_id: identity.definition_id.clone(),
            })
        }
    }
}

#[cfg(feature = "model-api")]
fn ensure_bim_material_registry(world: &mut World) {
    if world
        .get_resource::<crate::plugins::modeling::bim_material_assignment::BimMaterialAssignmentRegistry>()
        .is_none()
    {
        world.insert_resource(
            crate::plugins::modeling::bim_material_assignment::BimMaterialAssignmentRegistry::default(),
        );
    }
}

#[cfg(feature = "model-api")]
fn serialize_bim_assignment(
    assignment: Option<&crate::plugins::modeling::bim_material_assignment::BimMaterialAssignment>,
) -> Value {
    assignment
        .and_then(|assignment| serde_json::to_value(assignment).ok())
        .unwrap_or(Value::Null)
}

#[cfg(feature = "model-api")]
fn assign_bim_material(
    world: &mut World,
    definition_id: Option<&str>,
    element_id: Option<u64>,
    assignment: crate::plugins::modeling::bim_material_assignment::BimMaterialAssignment,
) -> Result<Value, String> {
    use crate::plugins::modeling::bim_material_assignment::{
        BimMaterialAssignmentOverride, BimMaterialAssignmentRegistry,
    };

    ensure_bim_material_registry(world);
    let target = resolve_bim_material_target(world, definition_id, element_id)?;
    match target {
        BimMaterialTarget::Definition(definition_id) => {
            let prior = world
                .resource_mut::<BimMaterialAssignmentRegistry>()
                .register(definition_id.clone(), assignment.clone());
            Ok(serde_json::json!({
                "target": "definition",
                "definition_id": definition_id.to_string(),
                "assignment": serialize_bim_assignment(Some(&assignment)),
                "prior": serialize_bim_assignment(prior.as_ref()),
            }))
        }
        BimMaterialTarget::Occurrence {
            element_id,
            entity,
            definition_id,
        } => {
            let prior = world
                .entity(entity)
                .get::<BimMaterialAssignmentOverride>()
                .map(|override_component| override_component.0.clone());
            world
                .entity_mut(entity)
                .insert(BimMaterialAssignmentOverride(assignment.clone()));
            Ok(serde_json::json!({
                "target": "element",
                "element_id": element_id,
                "definition_id": definition_id.to_string(),
                "assignment": serialize_bim_assignment(Some(&assignment)),
                "prior": serialize_bim_assignment(prior.as_ref()),
            }))
        }
    }
}

#[cfg(feature = "model-api")]
pub fn handle_bim_material_assign_layered(
    world: &mut World,
    request: BimMaterialAssignLayeredRequest,
) -> Result<Value, String> {
    let assignment = build_bim_layered_assignment(&request)?;
    assign_bim_material(
        world,
        request.definition_id.as_deref(),
        request.element_id,
        assignment,
    )
}

#[cfg(feature = "model-api")]
pub fn handle_bim_material_assign_constituents(
    world: &mut World,
    request: BimMaterialAssignConstituentsRequest,
) -> Result<Value, String> {
    let assignment = build_bim_constituent_assignment(&request)?;
    assign_bim_material(
        world,
        request.definition_id.as_deref(),
        request.element_id,
        assignment,
    )
}

#[cfg(feature = "model-api")]
pub fn handle_bim_material_get_effective(
    world: &mut World,
    request: BimMaterialGetEffectiveRequest,
) -> Result<Value, String> {
    use crate::plugins::modeling::bim_material_assignment::{
        effective_assignment, BimMaterialAssignmentOverride, BimMaterialAssignmentRegistry,
    };

    ensure_bim_material_registry(world);
    let target =
        resolve_bim_material_target(world, request.definition_id.as_deref(), request.element_id)?;
    let registry = world.resource::<BimMaterialAssignmentRegistry>();
    match target {
        BimMaterialTarget::Definition(definition_id) => {
            let assignment = registry.get(&definition_id);
            Ok(serde_json::json!({
                "target": "definition",
                "definition_id": definition_id.to_string(),
                "source": if assignment.is_some() { "definition" } else { "none" },
                "assignment": serialize_bim_assignment(assignment),
            }))
        }
        BimMaterialTarget::Occurrence {
            element_id,
            entity,
            definition_id,
        } => {
            let override_component = world.entity(entity).get::<BimMaterialAssignmentOverride>();
            let assignment = effective_assignment(registry, &definition_id, override_component);
            let source = if override_component.is_some() {
                "override"
            } else if assignment.is_some() {
                "definition"
            } else {
                "none"
            };
            Ok(serde_json::json!({
                "target": "element",
                "element_id": element_id,
                "definition_id": definition_id.to_string(),
                "source": source,
                "assignment": serialize_bim_assignment(assignment),
            }))
        }
    }
}

#[cfg(feature = "model-api")]
fn parse_quantity_provenance(
    input: &QuantityProvenanceInput,
) -> Result<crate::plugins::modeling::quantity_set::QuantityProvenance, String> {
    use crate::plugins::modeling::quantity_set::QuantityProvenance;

    match input.kind.trim().to_ascii_lowercase().as_str() {
        "authored_parameter" | "parameter" => {
            let parameter = input
                .parameter
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "authored_parameter provenance requires parameter".to_string())?;
            Ok(QuantityProvenance::AuthoredParameter {
                parameter: parameter.to_string(),
            })
        }
        "evaluator_node" | "evaluator" => {
            let node = input
                .node
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "evaluator_node provenance requires node".to_string())?;
            Ok(QuantityProvenance::EvaluatorNode {
                node: node.to_string(),
            })
        }
        "imported" | "import" => {
            let source = input
                .source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "imported provenance requires source".to_string())?;
            Ok(QuantityProvenance::Imported {
                source: source.to_string(),
            })
        }
        "mesh_approximation" | "mesh" => Ok(QuantityProvenance::MeshApproximation),
        "user_override" | "user" => Ok(QuantityProvenance::UserOverride {
            rationale: input
                .rationale
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
        }),
        other => Err(format!("Unsupported quantity provenance kind '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_quantity_f64(field: &str, value: &Value) -> Result<f64, String> {
    let value = value
        .as_f64()
        .ok_or_else(|| format!("quantity field '{field}' must be a number"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!(
            "quantity field '{field}' must be finite and non-negative"
        ));
    }
    Ok(value)
}

#[cfg(feature = "model-api")]
fn parse_quantity_count(field: &str, value: &Value) -> Result<u32, String> {
    let value = value
        .as_u64()
        .ok_or_else(|| format!("quantity field '{field}' must be an unsigned integer"))?;
    u32::try_from(value).map_err(|_| format!("quantity field '{field}' exceeds u32::MAX"))
}

#[cfg(feature = "model-api")]
fn normalized_quantity_field(field: &str) -> String {
    field.trim().to_ascii_lowercase()
}

#[cfg(feature = "model-api")]
fn set_primary_quantity_field(
    set: &mut crate::plugins::modeling::quantity_set::QuantitySet,
    field: &str,
    value: &Value,
    provenance: crate::plugins::modeling::quantity_set::QuantityProvenance,
) -> Result<(), String> {
    use crate::plugins::modeling::quantity_set::QuantityValue;

    match normalized_quantity_field(field).as_str() {
        "area_gross" | "area_gross_m2" => {
            set.area_gross_m2 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "area_net" | "area_net_m2" => {
            set.area_net_m2 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "volume_gross" | "volume_gross_m3" => {
            set.volume_gross_m3 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "volume_net" | "volume_net_m3" => {
            set.volume_net_m3 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "length" | "length_m" => {
            set.length_m = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "opening_area_deducted" | "opening_area_deducted_m2" => {
            set.opening_area_deducted_m2 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "count" => {
            set.count = Some(QuantityValue::new(
                parse_quantity_count(field, value)?,
                provenance,
            ));
        }
        other => {
            return Err(format!("Unsupported primary quantity field '{other}'"));
        }
    }
    Ok(())
}

#[cfg(feature = "model-api")]
fn get_primary_quantity_field(
    set: &crate::plugins::modeling::quantity_set::QuantitySet,
    field: &str,
) -> Result<Value, String> {
    let value = match normalized_quantity_field(field).as_str() {
        "area_gross" | "area_gross_m2" => set
            .area_gross_m2
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "area_net" | "area_net_m2" => set
            .area_net_m2
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "volume_gross" | "volume_gross_m3" => set
            .volume_gross_m3
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "volume_net" | "volume_net_m3" => set
            .volume_net_m3
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "length" | "length_m" => set
            .length_m
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "opening_area_deducted" | "opening_area_deducted_m2" => set
            .opening_area_deducted_m2
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "count" => set
            .count
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        other => return Err(format!("Unsupported primary quantity field '{other}'")),
    };
    Ok(value.unwrap_or(Value::Null))
}

#[cfg(feature = "model-api")]
fn set_material_quantity_field(
    quantity: &mut crate::plugins::modeling::quantity_set::MaterialQuantity,
    field: &str,
    value: &Value,
    provenance: crate::plugins::modeling::quantity_set::QuantityProvenance,
) -> Result<(), String> {
    use crate::plugins::modeling::quantity_set::QuantityValue;

    match normalized_quantity_field(field).as_str() {
        "volume" | "volume_m3" => {
            quantity.volume_m3 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "area" | "area_m2" => {
            quantity.area_m2 = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "length" | "length_m" => {
            quantity.length_m = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "mass" | "mass_kg" => {
            quantity.mass_kg = Some(QuantityValue::new(
                parse_quantity_f64(field, value)?,
                provenance,
            ));
        }
        "count" => {
            quantity.count = Some(QuantityValue::new(
                parse_quantity_count(field, value)?,
                provenance,
            ));
        }
        other => return Err(format!("Unsupported material quantity field '{other}'")),
    }
    Ok(())
}

#[cfg(feature = "model-api")]
fn get_material_quantity_field(
    quantity: &crate::plugins::modeling::quantity_set::MaterialQuantity,
    field: &str,
) -> Result<Value, String> {
    let value = match normalized_quantity_field(field).as_str() {
        "volume" | "volume_m3" => quantity
            .volume_m3
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "area" | "area_m2" => quantity
            .area_m2
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "length" | "length_m" => quantity
            .length_m
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "mass" | "mass_kg" => quantity
            .mass_kg
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        "count" => quantity
            .count
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
        other => return Err(format!("Unsupported material quantity field '{other}'")),
    };
    Ok(value.unwrap_or(Value::Null))
}

#[cfg(feature = "model-api")]
pub fn handle_quantity_set(
    world: &mut World,
    request: QuantitySetRequest,
) -> Result<Value, String> {
    use crate::plugins::modeling::quantity_set::{MaterialQuantity, QuantitySet};

    let entity = find_entity_by_element_id(world, ElementId(request.element_id))
        .ok_or_else(|| format!("Entity {} not found", request.element_id))?;
    if crate::plugins::refinement::is_parked_refinement_entity(world, ElementId(request.element_id))
    {
        return Err(format!(
            "Entity {} is in a parked refinement branch",
            request.element_id
        ));
    }
    if world.get::<QuantitySet>(entity).is_none() {
        world.entity_mut(entity).insert(QuantitySet::empty());
    }
    let provenance = parse_quantity_provenance(&request.provenance)?;
    let mut set = world
        .get_mut::<QuantitySet>(entity)
        .ok_or_else(|| "QuantitySet component missing after insert".to_string())?;
    if let Some(material) = request.material.as_deref() {
        let material = parse_bim_material_ref(material)?;
        let mut quantity = set
            .material_quantity(&material)
            .cloned()
            .unwrap_or_else(|| MaterialQuantity::new(material));
        set_material_quantity_field(&mut quantity, &request.field, &request.value, provenance)?;
        set.upsert_material_quantity(quantity);
    } else {
        set_primary_quantity_field(&mut set, &request.field, &request.value, provenance)?;
    }
    serde_json::to_value(&*set).map_err(|error| error.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_quantity_get(
    world: &mut World,
    request: QuantityGetRequest,
) -> Result<Value, String> {
    use crate::plugins::modeling::quantity_set::QuantitySet;

    let entity = find_entity_by_element_id_readonly(world, ElementId(request.element_id))
        .ok_or_else(|| format!("Entity {} not found", request.element_id))?;
    if crate::plugins::refinement::is_parked_refinement_entity(world, ElementId(request.element_id))
    {
        return Ok(Value::Null);
    }
    let Some(set) = world.entity(entity).get::<QuantitySet>() else {
        return Ok(Value::Null);
    };
    let Some(field) = request.field.as_deref() else {
        return serde_json::to_value(set).map_err(|error| error.to_string());
    };
    if let Some(material) = request.material.as_deref() {
        let material = parse_bim_material_ref(material)?;
        let Some(quantity) = set.material_quantity(&material) else {
            return Ok(Value::Null);
        };
        return get_material_quantity_field(quantity, field);
    }
    get_primary_quantity_field(set, field)
}

#[cfg(feature = "model-api")]
fn provenance_json(
    field: &str,
    material: Option<&str>,
    provenance: &crate::plugins::modeling::quantity_set::QuantityProvenance,
) -> Value {
    serde_json::json!({
        "field": field,
        "material": material,
        "provenance": provenance,
        "grounded": provenance.is_grounded(),
    })
}

#[cfg(feature = "model-api")]
pub fn handle_quantity_list_provenance(
    world: &mut World,
    request: QuantityListProvenanceRequest,
) -> Result<Value, String> {
    use crate::plugins::modeling::quantity_set::QuantitySet;

    let entity = find_entity_by_element_id_readonly(world, ElementId(request.element_id))
        .ok_or_else(|| format!("Entity {} not found", request.element_id))?;
    if crate::plugins::refinement::is_parked_refinement_entity(world, ElementId(request.element_id))
    {
        return Ok(Value::Array(Vec::new()));
    }
    let Some(set) = world.entity(entity).get::<QuantitySet>() else {
        return Ok(Value::Array(Vec::new()));
    };
    let mut out: Vec<Value> = set
        .provenances()
        .into_iter()
        .map(|(field, provenance)| provenance_json(field, None, provenance))
        .collect();
    for quantity in &set.material_quantities {
        let material = quantity.material.as_str();
        if let Some(value) = &quantity.volume_m3 {
            out.push(provenance_json(
                "volume_m3",
                Some(material),
                &value.provenance,
            ));
        }
        if let Some(value) = &quantity.area_m2 {
            out.push(provenance_json(
                "area_m2",
                Some(material),
                &value.provenance,
            ));
        }
        if let Some(value) = &quantity.length_m {
            out.push(provenance_json(
                "length_m",
                Some(material),
                &value.provenance,
            ));
        }
        if let Some(value) = &quantity.mass_kg {
            out.push(provenance_json(
                "mass_kg",
                Some(material),
                &value.provenance,
            ));
        }
        if let Some(value) = &quantity.count {
            out.push(provenance_json("count", Some(material), &value.provenance));
        }
    }
    Ok(Value::Array(out))
}

#[cfg(feature = "model-api")]
pub fn handle_quantity_check_invariants(
    world: &mut World,
    request: QuantityCheckInvariantsRequest,
) -> Result<Value, String> {
    use crate::plugins::modeling::quantity_set::QuantitySet;

    let entity = find_entity_by_element_id_readonly(world, ElementId(request.element_id))
        .ok_or_else(|| format!("Entity {} not found", request.element_id))?;
    if crate::plugins::refinement::is_parked_refinement_entity(world, ElementId(request.element_id))
    {
        return Ok(serde_json::json!({
            "has_quantity_set": false,
            "ok": true,
            "net_le_gross_violations": [],
            "area_deduction_consistent": true,
            "all_grounded": true,
            "parked_refinement_branch": true,
        }));
    }
    let Some(set) = world.entity(entity).get::<QuantitySet>() else {
        return Ok(serde_json::json!({
            "has_quantity_set": false,
            "ok": true,
            "net_le_gross_violations": [],
            "area_deduction_consistent": true,
            "all_grounded": true,
        }));
    };
    let tol = request.tolerance.unwrap_or(1.0e-6);
    if !tol.is_finite() || tol < 0.0 {
        return Err("tolerance must be finite and non-negative".to_string());
    }
    let net_le_gross_violations = set.net_le_gross_violations();
    let area_deduction_consistent = set.area_deduction_consistent(tol);
    let all_grounded = set.all_grounded();
    Ok(serde_json::json!({
        "has_quantity_set": true,
        "ok": net_le_gross_violations.is_empty() && area_deduction_consistent && all_grounded,
        "net_le_gross_violations": net_le_gross_violations,
        "area_deduction_consistent": area_deduction_consistent,
        "all_grounded": all_grounded,
    }))
}

#[cfg(feature = "model-api")]
fn handle_list_material_specs(
    world: &World,
    filter: ListMaterialSpecsFilter,
) -> Result<Vec<MaterialSpecInfo>, String> {
    Ok(crate::curation::api::list_material_specs(world, filter))
}

#[cfg(feature = "model-api")]
fn handle_get_material_spec(world: &World, asset_id: &str) -> Result<MaterialSpecInfo, String> {
    crate::curation::api::get_material_spec(world, asset_id).map_err(|failure| failure.message)
}

#[cfg(feature = "model-api")]
fn handle_create_material_spec(
    world: &mut World,
    request: DraftMaterialSpecRequest,
) -> Result<MaterialSpecInfo, String> {
    crate::curation::api::create_material_spec(world, request).map_err(|failure| failure.message)
}

#[cfg(feature = "model-api")]
fn handle_update_material_spec(
    world: &mut World,
    asset_id: &str,
    body: MaterialSpecBody,
    rationale: Option<String>,
) -> Result<MaterialSpecInfo, String> {
    crate::curation::api::update_material_spec(world, asset_id, body, rationale)
        .map_err(|failure| failure.message)
}

#[cfg(feature = "model-api")]
fn handle_save_material_spec(
    world: &mut World,
    asset_id: &str,
    scope: &str,
) -> Result<MaterialSpecInfo, String> {
    crate::curation::api::save_material_spec(world, asset_id, scope)
        .map_err(|failure| failure.message)
}

#[cfg(feature = "model-api")]
fn handle_publish_material_spec(
    world: &mut World,
    asset_id: &str,
) -> Result<MaterialSpecInfo, String> {
    crate::curation::api::publish_material_spec(world, asset_id).map_err(|failure| failure.message)
}

#[cfg(feature = "model-api")]
fn handle_delete_material_spec(world: &mut World, asset_id: &str) -> Result<String, String> {
    crate::curation::api::delete_material_spec(world, asset_id).map_err(|failure| failure.message)
}

#[cfg(feature = "model-api")]
fn ambient_light_info_from_settings(settings: &SceneLightingSettings) -> AmbientLightInfo {
    AmbientLightInfo {
        color: settings.ambient_color,
        brightness: settings.ambient_brightness,
        affects_lightmapped_meshes: settings.affects_lightmapped_meshes,
    }
}

#[cfg(feature = "model-api")]
fn scene_light_info_from_parts(
    element_id: ElementId,
    node: &SceneLightNode,
    transform: &Transform,
) -> SceneLightInfo {
    let (yaw, pitch, _roll) = transform.rotation.to_euler(EulerRot::YXZ);
    SceneLightInfo {
        element_id: element_id.0,
        name: node.name.clone(),
        kind: node.kind.as_str().to_string(),
        enabled: node.enabled,
        color: node.color,
        intensity: node.intensity,
        shadows_enabled: node.shadows_enabled,
        position: transform.translation.to_array(),
        yaw_deg: yaw.to_degrees(),
        pitch_deg: pitch.to_degrees(),
        range: node.range,
        radius: node.radius,
        inner_angle_deg: node.inner_angle_deg,
        outer_angle_deg: node.outer_angle_deg,
    }
}

#[cfg(feature = "model-api")]
fn scene_light_info_from_world(world: &World, element_id: ElementId) -> Option<SceneLightInfo> {
    let mut query = world.try_query::<(&ElementId, &SceneLightNode, &Transform)>()?;
    query.iter(world).find_map(|(current_id, node, transform)| {
        (*current_id == element_id)
            .then(|| scene_light_info_from_parts(*current_id, node, transform))
    })
}

#[cfg(feature = "model-api")]
fn handle_get_lighting_scene(world: &World) -> LightingSceneInfo {
    LightingSceneInfo {
        ambient: ambient_light_info_from_settings(world.resource::<SceneLightingSettings>()),
        lights: handle_list_lights(world),
    }
}

#[cfg(feature = "model-api")]
fn handle_list_lights(world: &World) -> Vec<SceneLightInfo> {
    let Some(mut query) = world.try_query::<(&ElementId, &SceneLightNode, &Transform)>() else {
        return Vec::new();
    };
    query
        .iter(world)
        .map(|(element_id, node, transform)| {
            scene_light_info_from_parts(*element_id, node, transform)
        })
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_create_light(
    world: &mut World,
    request: CreateLightRequest,
) -> Result<SceneLightInfo, String> {
    let element_id = handle_create_entity(world, create_light_request_json(&request))?;
    scene_light_info_from_world(world, ElementId(element_id))
        .ok_or_else(|| format!("Light {element_id} was not found after creation"))
}

#[cfg(feature = "model-api")]
fn create_guide_line_request_json(request: &PlaceGuideLineRequest) -> Value {
    let mut request_json = json!({
        "type": "guide_line",
        "anchor": request.anchor,
        "visible": request.visible.unwrap_or(true),
        "label": request.label,
    });
    let object = request_json
        .as_object_mut()
        .expect("guide line create request should serialize as an object");
    if let Some(direction) = request.direction {
        object.insert("direction".to_string(), json!(direction));
    }
    if let Some(through) = request.through {
        object.insert("through".to_string(), json!(through));
    }
    if let Some(reference_direction) = request.reference_direction {
        object.insert(
            "reference_direction".to_string(),
            json!(reference_direction),
        );
    }
    if let Some(angle_degrees) = request.angle_degrees {
        object.insert("angle_degrees".to_string(), json!(angle_degrees));
    }
    if let Some(plane_normal) = request.plane_normal {
        object.insert("plane_normal".to_string(), json!(plane_normal));
    }
    if let Some(finite_length) = request.finite_length {
        object.insert("finite_length".to_string(), json!(finite_length));
    }
    request_json
}

#[cfg(feature = "model-api")]
fn create_dimension_line_request_json(request: &PlaceDimensionLineRequest) -> Value {
    let mut request_json = json!({
        "type": "dimension_line",
        "start": request.start,
        "end": request.end,
        "visible": request.visible.unwrap_or(true),
        "label": request.label,
    });
    let object = request_json
        .as_object_mut()
        .expect("dimension line create request should serialize as an object");
    if let Some(extension) = request.extension {
        object.insert("extension".to_string(), json!(extension));
    }
    if let Some(line_point) = request.line_point {
        object.insert("line_point".to_string(), json!(line_point));
    }
    if let Some(offset) = request.offset {
        object.insert("offset".to_string(), json!(offset));
    }
    if let Some(display_unit) = &request.display_unit {
        object.insert("display_unit".to_string(), json!(display_unit));
    }
    if let Some(precision) = request.precision {
        object.insert("precision".to_string(), json!(precision));
    }
    request_json
}

#[cfg(feature = "model-api")]
fn create_box_request_json(request: &CreateBoxRequest) -> Result<Value, String> {
    let center = request.center.unwrap_or([0.0, 0.0, 0.0]);
    let half_extents = match (request.half_extents, request.size) {
        (Some(_), Some(_)) => {
            return Err("create_box expects either `size` or `half_extents`, not both".to_string());
        }
        (Some(half_extents), None) => half_extents,
        (None, Some(size)) => {
            if size.iter().any(|value| !value.is_finite() || *value <= 0.0) {
                return Err(
                    "create_box `size` values must be finite and greater than zero".to_string(),
                );
            }
            [size[0] * 0.5, size[1] * 0.5, size[2] * 0.5]
        }
        (None, None) => {
            return Err("create_box requires either `size` or `half_extents`".to_string());
        }
    };
    if half_extents
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(
            "create_box `half_extents` values must be finite and greater than zero".to_string(),
        );
    }

    let mut request_json = json!({
        "type": "box",
        "centre": center,
        "half_extents": half_extents,
    });
    if let Some(rotation) = request.rotation {
        request_json
            .as_object_mut()
            .expect("box request should serialize as an object")
            .insert("rotation".to_string(), json!(rotation));
    }
    if let Some(semantic) = &request.semantic {
        request_json
            .as_object_mut()
            .expect("box request should serialize as an object")
            .insert(
                "semantic".to_string(),
                serde_json::to_value(semantic).map_err(|error| error.to_string())?,
            );
    }
    Ok(request_json)
}

#[cfg(feature = "model-api")]
fn boolean_request_json(base: u64, tool: u64, op: &str) -> Value {
    json!({
        "type": "csg",
        "operand_a": base,
        "operand_b": tool,
        "op": op,
    })
}

#[cfg(feature = "model-api")]
fn handle_update_light(
    world: &mut World,
    request: UpdateLightRequest,
) -> Result<SceneLightInfo, String> {
    let element_id = ElementId(request.element_id);
    let before = capture_snapshot_by_id(world, element_id)?;
    if before.type_name() != "scene_light" {
        return Err(format!(
            "Entity {} is not a scene light",
            request.element_id
        ));
    }

    let mut updated = before.clone();
    if let Some(name) = request.name {
        updated = updated.set_property_json("name", &json!(name))?;
    }
    if let Some(kind) = request.kind {
        updated = updated.set_property_json("kind", &json!(kind))?;
    }
    if let Some(enabled) = request.enabled {
        updated = updated.set_property_json("enabled", &json!(enabled))?;
    }
    if let Some(color) = request.color {
        updated = updated.set_property_json("color", &json!(color))?;
    }
    if let Some(intensity) = request.intensity {
        updated = updated.set_property_json("intensity", &json!(intensity))?;
    }
    if let Some(shadows_enabled) = request.shadows_enabled {
        updated = updated.set_property_json("shadows_enabled", &json!(shadows_enabled))?;
    }
    if let Some(position) = request.position {
        updated = updated.set_property_json("position", &json!(position))?;
    }
    if let Some(yaw_deg) = request.yaw_deg {
        updated = updated.set_property_json("yaw_deg", &json!(yaw_deg))?;
    }
    if let Some(pitch_deg) = request.pitch_deg {
        updated = updated.set_property_json("pitch_deg", &json!(pitch_deg))?;
    }
    if let Some(range) = request.range {
        updated = updated.set_property_json("range", &json!(range))?;
    }
    if let Some(radius) = request.radius {
        updated = updated.set_property_json("radius", &json!(radius))?;
    }
    if let Some(inner_angle_deg) = request.inner_angle_deg {
        updated = updated.set_property_json("inner_angle_deg", &json!(inner_angle_deg))?;
    }
    if let Some(outer_angle_deg) = request.outer_angle_deg {
        updated = updated.set_property_json("outer_angle_deg", &json!(outer_angle_deg))?;
    }

    send_event(
        world,
        ApplyEntityChangesCommand {
            label: "AI update light",
            before: vec![before],
            after: vec![updated],
        },
    );
    flush_model_api_write_pipeline(world);

    scene_light_info_from_world(world, element_id)
        .ok_or_else(|| format!("Light {} was not found after update", request.element_id))
}

#[cfg(feature = "model-api")]
fn handle_delete_light(world: &mut World, element_id: u64) -> Result<usize, String> {
    scene_light_info_from_world(world, ElementId(element_id))
        .ok_or_else(|| format!("Light {element_id} not found"))?;
    handle_delete_entities(world, vec![element_id])
}

#[cfg(feature = "model-api")]
fn handle_set_ambient_light(
    world: &mut World,
    request: AmbientLightUpdateRequest,
) -> Result<AmbientLightInfo, String> {
    let mut settings = world.resource::<SceneLightingSettings>().clone();
    if let Some(color) = request.color {
        settings.ambient_color = color;
    }
    if let Some(brightness) = request.brightness {
        settings.ambient_brightness = brightness.max(0.0);
    }
    if let Some(affects_lightmapped_meshes) = request.affects_lightmapped_meshes {
        settings.affects_lightmapped_meshes = affects_lightmapped_meshes;
    }
    let info = ambient_light_info_from_settings(&settings);
    world.insert_resource(settings);
    Ok(info)
}

#[cfg(feature = "model-api")]
fn handle_restore_default_light_rig(world: &mut World) -> Result<Vec<SceneLightInfo>, String> {
    let existing_ids = handle_list_lights(world)
        .into_iter()
        .map(|light| light.element_id)
        .collect::<Vec<_>>();
    if !existing_ids.is_empty() {
        handle_delete_entities(world, existing_ids)?;
    }

    send_event(
        world,
        BeginCommandGroup {
            label: "Restore default light rig",
        },
    );
    for snapshot in create_daylight_rig(world.resource::<ElementIdAllocator>()) {
        send_event(
            world,
            CreateEntityCommand {
                snapshot: snapshot.into(),
            },
        );
    }
    send_event(world, EndCommandGroup);
    flush_model_api_write_pipeline(world);

    Ok(handle_list_lights(world))
}

#[cfg(feature = "model-api")]
fn create_light_request_json(request: &CreateLightRequest) -> Value {
    let mut value = json!({
        "type": "scene_light",
        "kind": request.kind,
    });
    if let Some(name) = &request.name {
        value["name"] = json!(name);
    }
    if let Some(enabled) = request.enabled {
        value["enabled"] = json!(enabled);
    }
    if let Some(color) = request.color {
        value["color"] = json!(color);
    }
    if let Some(intensity) = request.intensity {
        value["intensity"] = json!(intensity);
    }
    if let Some(shadows_enabled) = request.shadows_enabled {
        value["shadows_enabled"] = json!(shadows_enabled);
    }
    if let Some(position) = request.position {
        value["position"] = json!(position);
    }
    if let Some(yaw_deg) = request.yaw_deg {
        value["yaw_deg"] = json!(yaw_deg);
    }
    if let Some(pitch_deg) = request.pitch_deg {
        value["pitch_deg"] = json!(pitch_deg);
    }
    if let Some(range) = request.range {
        value["range"] = json!(range);
    }
    if let Some(radius) = request.radius {
        value["radius"] = json!(radius);
    }
    if let Some(inner_angle_deg) = request.inner_angle_deg {
        value["inner_angle_deg"] = json!(inner_angle_deg);
    }
    if let Some(outer_angle_deg) = request.outer_angle_deg {
        value["outer_angle_deg"] = json!(outer_angle_deg);
    }
    value
}

#[cfg(feature = "model-api")]
fn handle_get_render_settings(world: &World) -> RenderSettingsInfo {
    RenderSettingsInfo::from_settings(world.resource::<RenderSettings>())
}

#[cfg(feature = "model-api")]
fn handle_set_render_settings(
    world: &mut World,
    request: RenderSettingsUpdateRequest,
) -> Result<RenderSettingsInfo, String> {
    let mut settings = world.resource::<RenderSettings>().clone();

    if let Some(tonemapping) = request.tonemapping {
        settings.tonemapping = RenderTonemapping::from_str(&tonemapping)
            .ok_or_else(|| format!("Unknown tonemapping mode '{tonemapping}'"))?;
    }
    if let Some(exposure_ev100) = request.exposure_ev100 {
        settings.exposure_ev100 = exposure_ev100;
    }
    if let Some(ssao_enabled) = request.ssao_enabled {
        settings.ssao_enabled = ssao_enabled;
    }
    if let Some(thickness) = request.ssao_constant_object_thickness {
        settings.ssao_constant_object_thickness = thickness.max(0.0);
    }
    if let Some(quality) = request.ambient_occlusion_quality {
        settings.ambient_occlusion_quality = quality.min(3);
    }
    if let Some(bloom_enabled) = request.bloom_enabled {
        settings.bloom_enabled = bloom_enabled;
    }
    if let Some(value) = request.bloom_intensity {
        settings.bloom_intensity = value.max(0.0);
    }
    if let Some(value) = request.bloom_low_frequency_boost {
        settings.bloom_low_frequency_boost = value.clamp(0.0, 1.0);
    }
    if let Some(value) = request.bloom_low_frequency_boost_curvature {
        settings.bloom_low_frequency_boost_curvature = value.clamp(0.0, 1.0);
    }
    if let Some(value) = request.bloom_high_pass_frequency {
        settings.bloom_high_pass_frequency = value.clamp(0.0, 1.0);
    }
    if let Some(value) = request.bloom_threshold {
        settings.bloom_threshold = value.max(0.0);
    }
    if let Some(value) = request.bloom_threshold_softness {
        settings.bloom_threshold_softness = value.clamp(0.0, 1.0);
    }
    if let Some(scale) = request.bloom_scale {
        settings.bloom_scale = [scale[0].max(0.0), scale[1].max(0.0)];
    }
    if let Some(ssr_enabled) = request.ssr_enabled {
        settings.ssr_enabled = ssr_enabled;
    }
    if let Some(value) = request.ssr_perceptual_roughness_threshold {
        settings.ssr_perceptual_roughness_threshold = value.clamp(0.0, 1.0);
    }
    if let Some(value) = request.ssr_thickness {
        settings.ssr_thickness = value.max(0.0);
    }
    if let Some(value) = request.ssr_linear_steps {
        settings.ssr_linear_steps = value.max(1);
    }
    if let Some(value) = request.ssr_linear_march_exponent {
        settings.ssr_linear_march_exponent = value.max(0.1);
    }
    if let Some(value) = request.ssr_bisection_steps {
        settings.ssr_bisection_steps = value;
    }
    if let Some(value) = request.ssr_use_secant {
        settings.ssr_use_secant = value;
    }
    if let Some(value) = request.wireframe_overlay_enabled {
        settings.wireframe_overlay_enabled = value;
    }
    if let Some(value) = request.contour_overlay_enabled {
        settings.contour_overlay_enabled = value;
    }
    if let Some(value) = request.visible_edge_overlay_enabled {
        settings.visible_edge_overlay_enabled = value;
    }
    if let Some(value) = request.grid_enabled {
        settings.grid_enabled = value;
    }
    if let Some(value) = request.background_rgb {
        settings.background_rgb = [
            value[0].clamp(0.0, 1.0),
            value[1].clamp(0.0, 1.0),
            value[2].clamp(0.0, 1.0),
        ];
    }
    if let Some(value) = request.paper_fill_enabled {
        settings.paper_fill_enabled = value;
    }

    let info = RenderSettingsInfo::from_settings(&settings);
    world.insert_resource(settings);
    Ok(info)
}

#[cfg(feature = "model-api")]
fn handle_get_camera(world: &World) -> CameraStateInfo {
    camera_state_info_from_live(&live_camera_snapshot(world))
}

#[cfg(feature = "model-api")]
fn handle_set_camera(world: &mut World, params: CameraParams) -> Result<CameraStateInfo, String> {
    let orbit = orbit_from_camera_params(world, Some(&params))?;
    let mut q = world.query::<(&mut OrbitCamera, &mut Transform, &mut Projection)>();
    let Some((mut live_orbit, mut transform, mut projection)) = q.iter_mut(world).next() else {
        return Err("No orbit camera is available".to_string());
    };
    *live_orbit = orbit;
    apply_orbit_state(&live_orbit, &mut transform, &mut projection);
    Ok(camera_state_info_from_live(&live_camera_snapshot(world)))
}

#[cfg(feature = "model-api")]
fn material_def_from_request(req: CreateMaterialRequest) -> MaterialDef {
    let mut def = MaterialDef::new(req.name.clone());
    apply_request_to_def(req, &mut def);
    def
}

#[cfg(feature = "model-api")]
fn apply_request_to_def(req: CreateMaterialRequest, def: &mut MaterialDef) {
    use crate::plugins::materials::TextureRef;

    /// Convert an API texture field (base64 string + optional mime) into a
    /// `TextureRef::Embedded`.  Returns `None` when `data` is `None`.
    fn to_texture_ref(data: Option<String>, mime: Option<String>) -> Option<TextureRef> {
        data.map(|d| TextureRef::Embedded {
            data: d,
            mime: mime.unwrap_or_else(|| "image/png".to_string()),
        })
    }

    def.name = req.name;
    def.base_color = req.base_color;
    def.perceptual_roughness = req.perceptual_roughness;
    def.metallic = req.metallic;
    def.reflectance = req.reflectance;
    def.specular_tint = req.specular_tint;
    def.emissive = req.emissive;
    def.emissive_exposure_weight = req.emissive_exposure_weight;
    def.diffuse_transmission = req.diffuse_transmission;
    def.specular_transmission = req.specular_transmission;
    def.thickness = req.thickness;
    def.ior = req.ior;
    def.attenuation_distance = req.attenuation_distance;
    def.attenuation_color = req.attenuation_color;
    def.clearcoat = req.clearcoat;
    def.clearcoat_perceptual_roughness = req.clearcoat_perceptual_roughness;
    def.anisotropy_strength = req.anisotropy_strength;
    def.anisotropy_rotation = req.anisotropy_rotation_deg.to_radians();
    def.spec_ref = req.spec_ref.map(crate::curation::AssetId::new);
    def.alpha_mode = parse_alpha_mode(&req.alpha_mode);
    def.alpha_cutoff = req.alpha_cutoff;
    def.double_sided = req.double_sided;
    def.unlit = req.unlit;
    def.fog_enabled = req.fog_enabled;
    def.depth_bias = req.depth_bias;
    def.uv_scale = req.uv_scale;
    def.uv_rotation = req.uv_rotation_deg.to_radians();
    def.base_color_texture = to_texture_ref(req.base_color_texture, req.base_color_texture_mime);
    def.normal_map_texture = to_texture_ref(req.normal_map_texture, req.normal_map_texture_mime);
    def.metallic_roughness_texture = to_texture_ref(
        req.metallic_roughness_texture,
        req.metallic_roughness_texture_mime,
    );
    def.emissive_texture = to_texture_ref(req.emissive_texture, req.emissive_texture_mime);
    def.occlusion_texture = to_texture_ref(req.occlusion_texture, req.occlusion_texture_mime);
}

#[cfg(feature = "model-api")]
fn parse_alpha_mode(s: &str) -> crate::plugins::materials::MaterialAlphaMode {
    use crate::plugins::materials::MaterialAlphaMode;
    match s.to_lowercase().as_str() {
        "mask" => MaterialAlphaMode::Mask,
        "blend" => MaterialAlphaMode::Blend,
        "premultiplied" => MaterialAlphaMode::Premultiplied,
        "add" => MaterialAlphaMode::Add,
        _ => MaterialAlphaMode::Opaque,
    }
}

// --- Selection Handlers ---

#[cfg(feature = "model-api")]
fn handle_get_instance_info(world: &World) -> InstanceInfo {
    InstanceInfo::from(world.resource::<ModelApiRuntimeInfo>())
}

#[cfg(feature = "model-api")]
fn handle_get_selection(world: &mut World) -> Vec<u64> {
    let selected: Vec<(Entity, u64)> = {
        let mut query = world.query_filtered::<(Entity, &ElementId), With<Selected>>();
        query
            .iter(world)
            .map(|(entity, id)| (entity, id.0))
            .collect()
    };
    let registry = world.resource::<CapabilityRegistry>();
    selected
        .into_iter()
        .filter_map(|(entity, id)| registry.is_user_facing_entity(world, entity).then_some(id))
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_set_selection(world: &mut World, element_ids: Vec<u64>) -> Result<Vec<u64>, String> {
    use std::collections::HashSet;

    let target_ids: HashSet<ElementId> = element_ids.iter().copied().map(ElementId).collect();

    // Verify all target entities exist and are part of the user-facing model.
    for eid in &target_ids {
        ensure_user_editable_entity(world, *eid, "selected")?;
    }

    // Remove Selected from all currently selected entities
    let currently_selected: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    for entity in currently_selected {
        world.entity_mut(entity).remove::<Selected>();
    }

    // Add Selected to target entities
    let mut result_ids = Vec::new();
    for eid in &target_ids {
        if let Some(entity) = find_entity_by_element_id(world, *eid) {
            world.entity_mut(entity).insert(Selected);
            result_ids.push(eid.0);
        }
    }

    result_ids.sort();
    Ok(result_ids)
}

#[cfg(feature = "model-api")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpatialAxis {
    X,
    Y,
    Z,
}

#[cfg(feature = "model-api")]
impl SpatialAxis {
    fn parse(value: &str) -> ApiResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "x" => Ok(Self::X),
            "y" => Ok(Self::Y),
            "z" => Ok(Self::Z),
            _ => Err(format!("Invalid axis '{value}'. Valid axes: x, y, z")),
        }
    }

    fn unit_vector(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }

    fn component(self, value: Vec3) -> f32 {
        match self {
            Self::X => value.x,
            Self::Y => value.y,
            Self::Z => value.z,
        }
    }

    fn bounds_min(self, bounds: crate::authored_entity::EntityBounds) -> f32 {
        self.component(bounds.min)
    }

    fn bounds_max(self, bounds: crate::authored_entity::EntityBounds) -> f32 {
        self.component(bounds.max)
    }
}

#[cfg(feature = "model-api")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpatialAlignMode {
    Min,
    Max,
    Center,
}

#[cfg(feature = "model-api")]
impl SpatialAlignMode {
    fn parse(value: &str) -> ApiResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "min" => Ok(Self::Min),
            "max" => Ok(Self::Max),
            "center" => Ok(Self::Center),
            _ => Err(format!(
                "Invalid align mode '{value}'. Valid modes: min, max, center"
            )),
        }
    }

    fn coordinate(self, axis: SpatialAxis, bounds: crate::authored_entity::EntityBounds) -> f32 {
        match self {
            Self::Min => axis.bounds_min(bounds),
            Self::Max => axis.bounds_max(bounds),
            Self::Center => axis.component(bounds.center()),
        }
    }
}

#[cfg(feature = "model-api")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpatialDistributeMode {
    Spacing,
    Gap,
}

#[cfg(feature = "model-api")]
impl SpatialDistributeMode {
    fn parse(value: &str) -> ApiResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "spacing" => Ok(Self::Spacing),
            "gap" => Ok(Self::Gap),
            _ => Err(format!(
                "Invalid distribute mode '{value}'. Valid modes: spacing, gap"
            )),
        }
    }
}

#[cfg(feature = "model-api")]
#[derive(Debug, Clone)]
struct SpatialEntityPlan {
    element_id: ElementId,
    snapshot: BoxedEntity,
    bounds: crate::authored_entity::EntityBounds,
    locked: bool,
}

#[cfg(feature = "model-api")]
fn handle_align_preview(
    world: &mut World,
    request: AlignRequest,
) -> ApiResult<Vec<SpatialPreviewEntry>> {
    preview_align(world, &request).map(|(entries, _, _)| entries)
}

#[cfg(feature = "model-api")]
fn handle_align_execute(
    world: &mut World,
    request: AlignRequest,
) -> ApiResult<Vec<SpatialPreviewEntry>> {
    let (entries, before, after) = preview_align(world, &request)?;
    apply_spatial_preview(world, "Align selection", before, after);
    Ok(entries)
}

#[cfg(feature = "model-api")]
fn handle_distribute_preview(
    world: &mut World,
    request: DistributeRequest,
) -> ApiResult<Vec<SpatialPreviewEntry>> {
    preview_distribute(world, &request).map(|(entries, _, _)| entries)
}

#[cfg(feature = "model-api")]
fn handle_distribute_execute(
    world: &mut World,
    request: DistributeRequest,
) -> ApiResult<Vec<SpatialPreviewEntry>> {
    let (entries, before, after) = preview_distribute(world, &request)?;
    apply_spatial_preview(world, "Distribute selection", before, after);
    Ok(entries)
}

#[cfg(feature = "model-api")]
fn preview_align(
    world: &World,
    request: &AlignRequest,
) -> ApiResult<(Vec<SpatialPreviewEntry>, Vec<BoxedEntity>, Vec<BoxedEntity>)> {
    let axis = SpatialAxis::parse(&request.axis)?;
    let mode = SpatialAlignMode::parse(&request.mode)?;
    let plans = gather_spatial_entity_plans(world, &request.element_ids)?;

    if plans.is_empty() {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    }

    let reference_bounds = match (request.reference_element_id, request.reference_value) {
        (_, Some(value)) => Some(value),
        (Some(element_id), None) => {
            let snapshot = capture_snapshot_by_id(world, ElementId(element_id))?;
            Some(mode.coordinate(axis, alignment_bounds(&snapshot)))
        }
        (None, None) => None,
    };

    let aggregate_bounds = plans
        .iter()
        .map(|plan| plan.bounds)
        .reduce(|acc, bounds| merge_bounds(Some(acc), bounds))
        .expect("plans is not empty");
    let target_value = reference_bounds.unwrap_or_else(|| mode.coordinate(axis, aggregate_bounds));

    let mut entries = Vec::with_capacity(plans.len());
    let mut before = Vec::new();
    let mut after = Vec::new();

    for plan in plans {
        let current_position = plan.snapshot.center().to_array();
        let proposed_snapshot =
            if plan.locked || Some(plan.element_id.0) == request.reference_element_id {
                plan.snapshot.clone()
            } else {
                let current_value = mode.coordinate(axis, plan.bounds);
                let delta = target_value - current_value;
                if delta.abs() < 1e-5 {
                    plan.snapshot.clone()
                } else {
                    plan.snapshot.translate_by(axis.unit_vector() * delta)
                }
            };

        if proposed_snapshot != plan.snapshot {
            before.push(plan.snapshot.clone());
            after.push(proposed_snapshot.clone());
        }

        entries.push(SpatialPreviewEntry {
            element_id: plan.element_id.0,
            current_position,
            proposed_position: proposed_snapshot.center().to_array(),
        });
    }

    Ok((entries, before, after))
}

#[cfg(feature = "model-api")]
fn preview_distribute(
    world: &World,
    request: &DistributeRequest,
) -> ApiResult<(Vec<SpatialPreviewEntry>, Vec<BoxedEntity>, Vec<BoxedEntity>)> {
    let axis = SpatialAxis::parse(&request.axis)?;
    let mode = SpatialDistributeMode::parse(&request.mode)?;
    let plans = gather_spatial_entity_plans(world, &request.element_ids)?;
    let movable_count = plans.iter().filter(|plan| !plan.locked).count();
    if movable_count < 3 {
        return Err("Distribute requires at least three movable entities".to_string());
    }

    let mut unlocked: Vec<&SpatialEntityPlan> = plans.iter().filter(|plan| !plan.locked).collect();
    unlocked.sort_by(|a, b| {
        axis.component(a.bounds.center())
            .total_cmp(&axis.component(b.bounds.center()))
    });

    let mut target_centers = std::collections::HashMap::<u64, f32>::new();
    match mode {
        SpatialDistributeMode::Spacing => {
            let first = unlocked.first().expect("at least three movable entities");
            let first_center = axis.component(first.bounds.center());
            let last_center = axis.component(unlocked.last().expect("non-empty").bounds.center());
            let step = request
                .value
                .unwrap_or_else(|| (last_center - first_center) / (unlocked.len() - 1) as f32);
            for (index, plan) in unlocked.iter().enumerate() {
                target_centers.insert(plan.element_id.0, first_center + step * index as f32);
            }
        }
        SpatialDistributeMode::Gap => {
            let first = unlocked.first().expect("at least three movable entities");
            let first_min = axis.bounds_min(first.bounds);
            let gap = if let Some(value) = request.value {
                value
            } else {
                let last = unlocked.last().expect("non-empty");
                let span = axis.bounds_max(last.bounds) - first_min;
                let total_size: f32 = unlocked
                    .iter()
                    .map(|plan| axis.bounds_max(plan.bounds) - axis.bounds_min(plan.bounds))
                    .sum();
                (span - total_size) / (unlocked.len() - 1) as f32
            };
            let mut current_min = first_min;
            for plan in unlocked {
                let size = axis.bounds_max(plan.bounds) - axis.bounds_min(plan.bounds);
                target_centers.insert(plan.element_id.0, current_min + size * 0.5);
                current_min += size + gap;
            }
        }
    }

    let mut entries = Vec::with_capacity(plans.len());
    let mut before = Vec::new();
    let mut after = Vec::new();
    for plan in plans {
        let current_position = plan.snapshot.center().to_array();
        let proposed_snapshot = if plan.locked {
            plan.snapshot.clone()
        } else if let Some(target_center) = target_centers.get(&plan.element_id.0) {
            let current_center = axis.component(plan.bounds.center());
            let delta = *target_center - current_center;
            if delta.abs() < 1e-5 {
                plan.snapshot.clone()
            } else {
                plan.snapshot.translate_by(axis.unit_vector() * delta)
            }
        } else {
            plan.snapshot.clone()
        };

        if proposed_snapshot != plan.snapshot {
            before.push(plan.snapshot.clone());
            after.push(proposed_snapshot.clone());
        }

        entries.push(SpatialPreviewEntry {
            element_id: plan.element_id.0,
            current_position,
            proposed_position: proposed_snapshot.center().to_array(),
        });
    }

    Ok((entries, before, after))
}

#[cfg(feature = "model-api")]
fn gather_spatial_entity_plans(
    world: &World,
    element_ids: &[u64],
) -> ApiResult<Vec<SpatialEntityPlan>> {
    let mut plans = Vec::with_capacity(element_ids.len());
    for element_id in element_ids {
        let element_id = ElementId(*element_id);
        let snapshot = capture_snapshot_by_id(world, element_id)?;
        let entity = find_entity_by_element_id_readonly(world, element_id)
            .ok_or_else(|| format!("Entity {} not found", element_id.0))?;
        let locked = crate::plugins::layers::entity_on_locked_layer(world, entity);
        plans.push(SpatialEntityPlan {
            element_id,
            bounds: alignment_bounds(&snapshot),
            snapshot,
            locked,
        });
    }
    Ok(plans)
}

#[cfg(feature = "model-api")]
fn find_entity_by_element_id_readonly(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut query = world.try_query::<(Entity, &ElementId)>()?;
    query
        .iter(world)
        .find_map(|(entity, current)| (*current == element_id).then_some(entity))
}

#[cfg(feature = "model-api")]
fn alignment_bounds(snapshot: &BoxedEntity) -> crate::authored_entity::EntityBounds {
    snapshot.bounds().unwrap_or_else(|| {
        let center = snapshot.center();
        crate::authored_entity::EntityBounds {
            min: center,
            max: center,
        }
    })
}

#[cfg(feature = "model-api")]
fn apply_spatial_preview(
    world: &mut World,
    label: &'static str,
    before: Vec<BoxedEntity>,
    after: Vec<BoxedEntity>,
) {
    if before.is_empty() {
        return;
    }

    send_event(
        world,
        ApplyEntityChangesCommand {
            label,
            before,
            after,
        },
    );
    flush_model_api_write_pipeline(world);
}

// --- Face Subdivision Handler ---

#[cfg(feature = "model-api")]
fn handle_split_box_face(
    world: &mut World,
    element_id: u64,
    face_id: u32,
    split_position: f32,
) -> Result<SplitResult, String> {
    use crate::authored_entity::AuthoredEntity;
    use crate::capability_registry::FaceId;
    use crate::plugins::identity::ElementIdAllocator;
    use crate::plugins::modeling::{
        composite_solid::{CompositeSolid, SharedFace},
        generic_snapshot::PrimitiveSnapshot,
        group::GroupSnapshot,
        primitives::{BoxPrimitive, ShapeRotation},
    };

    if face_id > 5 {
        return Err(format!(
            "Invalid face_id {face_id}: must be 0-5 (-X,+X,-Y,+Y,-Z,+Z)"
        ));
    }
    if split_position <= 0.0 || split_position >= 1.0 {
        return Err(format!(
            "split_position must be strictly between 0.0 and 1.0, got {split_position}"
        ));
    }

    let eid = ElementId(element_id);
    let entity = find_entity_by_element_id(world, eid)
        .ok_or_else(|| format!("Entity not found: {element_id}"))?;

    // Read box primitive and rotation
    let box_prim = world
        .get::<BoxPrimitive>(entity)
        .cloned()
        .ok_or_else(|| format!("Entity {element_id} is not a box primitive"))?;
    let rotation = world
        .get::<ShapeRotation>(entity)
        .copied()
        .unwrap_or_default();

    let face = FaceId(face_id);
    let (face_axis, _face_sign) = face.box_axis_sign();

    // The two tangent axes of this face
    let tangent_axes: [usize; 2] = match face_axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };

    // For a face split, we split perpendicular to one of the tangent axes.
    // We use the first tangent axis as the split axis.
    let split_axis = tangent_axes[0];

    let half = [
        box_prim.half_extents.x,
        box_prim.half_extents.y,
        box_prim.half_extents.z,
    ];

    // Map 0.0-1.0 to the box extent range [-half, +half]
    let split_pos = -half[split_axis] + split_position * 2.0 * half[split_axis];

    // Compute the two new boxes
    let half_a = (split_pos + half[split_axis]) * 0.5;
    let half_b = (half[split_axis] - split_pos) * 0.5;
    let centre_a_local = (split_pos - half[split_axis]) * 0.5;
    let centre_b_local = (split_pos + half[split_axis]) * 0.5;

    let mut half_extents_a = box_prim.half_extents;
    let mut half_extents_b = box_prim.half_extents;
    let mut offset_a = Vec3::ZERO;
    let mut offset_b = Vec3::ZERO;

    match split_axis {
        0 => {
            half_extents_a.x = half_a;
            half_extents_b.x = half_b;
            offset_a.x = centre_a_local;
            offset_b.x = centre_b_local;
        }
        1 => {
            half_extents_a.y = half_a;
            half_extents_b.y = half_b;
            offset_a.y = centre_a_local;
            offset_b.y = centre_b_local;
        }
        _ => {
            half_extents_a.z = half_a;
            half_extents_b.z = half_b;
            offset_a.z = centre_a_local;
            offset_b.z = centre_b_local;
        }
    }

    let centre_a = box_prim.centre + rotation.0 * offset_a;
    let centre_b = box_prim.centre + rotation.0 * offset_b;

    let prim_a = BoxPrimitive {
        centre: centre_a,
        half_extents: half_extents_a,
    };
    let prim_b = BoxPrimitive {
        centre: centre_b,
        half_extents: half_extents_b,
    };

    let id_a = world.resource::<ElementIdAllocator>().next_id();
    let id_b = world.resource::<ElementIdAllocator>().next_id();
    let group_id = world.resource::<ElementIdAllocator>().next_id();

    let face_a = FaceId(split_axis as u32 * 2 + 1);
    let face_b = FaceId(split_axis as u32 * 2);

    let snapshot_a: PrimitiveSnapshot<BoxPrimitive> = PrimitiveSnapshot {
        element_id: id_a,
        primitive: prim_a,
        rotation,
        material_assignment: None,
        opening_context: None,
    };
    let snapshot_b: PrimitiveSnapshot<BoxPrimitive> = PrimitiveSnapshot {
        element_id: id_b,
        primitive: prim_b,
        rotation,
        material_assignment: None,
        opening_context: None,
    };
    let group_snapshot = GroupSnapshot {
        element_id: group_id,
        name: "Solid".to_string(),
        member_ids: vec![id_a, id_b],
        composite: Some(CompositeSolid {
            shared_faces: vec![SharedFace {
                entity_a: id_a,
                face_a,
                entity_b: id_b,
                face_b,
            }],
        }),
        cached_bounds: None,
    };

    // Begin undo group
    send_event(world, BeginCommandGroup { label: "Split Box" });

    // Delete original
    send_event(
        world,
        DeleteEntitiesCommand {
            element_ids: vec![eid],
        },
    );

    // Create two new boxes
    snapshot_a.apply_to(world);
    send_event(
        world,
        CreateEntityCommand {
            snapshot: snapshot_a.into(),
        },
    );

    snapshot_b.apply_to(world);
    send_event(
        world,
        CreateEntityCommand {
            snapshot: snapshot_b.into(),
        },
    );

    // Create the CompositeSolid group
    group_snapshot.apply_to(world);
    send_event(
        world,
        CreateEntityCommand {
            snapshot: group_snapshot.into(),
        },
    );

    // End undo group
    send_event(world, EndCommandGroup);

    flush_model_api_write_pipeline(world);

    Ok(SplitResult {
        box_a_element_id: id_a.0,
        box_b_element_id: id_b.0,
        group_element_id: group_id.0,
    })
}

// --- Screenshot Handler ---

#[cfg(feature = "model-api")]
fn handle_take_screenshot(world: &mut World, path: &str) -> Result<String, String> {
    use std::path::PathBuf;

    let path_buf = PathBuf::from(path);
    let path_owned = path.to_string();
    crate::plugins::drawing_export::queue_viewport_export(world, &path_buf)?;

    Ok(path_owned)
}

#[cfg(feature = "model-api")]
fn handle_export_drawing(world: &mut World, path: &str) -> Result<String, String> {
    let path_buf = crate::plugins::drawing_export::export_drawing_to_path(
        world,
        std::path::PathBuf::from(path),
    )?;
    Ok(path_buf.to_string_lossy().to_string())
}

#[cfg(feature = "model-api")]
fn handle_export_drafting_sheet(
    world: &mut World,
    path: &str,
    scale_denominator: Option<f32>,
) -> Result<String, String> {
    let path_buf = crate::plugins::drafting_sheet::export_sheet_to_path(
        world,
        std::path::PathBuf::from(path),
        scale_denominator,
    )?;
    Ok(path_buf.to_string_lossy().to_string())
}

#[cfg(feature = "model-api")]
fn handle_place_sheet_dimension(
    world: &mut World,
    request: PlaceSheetDimensionRequest,
) -> Result<u64, String> {
    use crate::plugins::drafting_sheet::{
        sheet_paper_to_world, sheet_view_from_active_camera, DEFAULT_MARGIN_MM,
        DEFAULT_SCALE_DENOMINATOR,
    };
    let scale = request
        .scale_denominator
        .unwrap_or(DEFAULT_SCALE_DENOMINATOR);
    let view = sheet_view_from_active_camera(world, scale, DEFAULT_MARGIN_MM).ok_or_else(|| {
        "no active orthographic camera — sheet dims require an ortho view".to_string()
    })?;

    let a_paper = Vec2::new(request.a[0], request.a[1]);
    let b_paper = Vec2::new(request.b[0], request.b[1]);
    let offset_paper = Vec2::new(request.offset[0], request.offset[1]);
    let midpoint_paper = (a_paper + b_paper) * 0.5;

    let a_world = sheet_paper_to_world(&view, a_paper)
        .ok_or_else(|| "degenerate sheet view — cannot inverse-project A".to_string())?;
    let b_world = sheet_paper_to_world(&view, b_paper)
        .ok_or_else(|| "degenerate sheet view — cannot inverse-project B".to_string())?;
    let mid_world = sheet_paper_to_world(&view, midpoint_paper)
        .ok_or_else(|| "degenerate sheet view — cannot inverse-project midpoint".to_string())?;
    let mid_plus_offset_world = sheet_paper_to_world(&view, midpoint_paper + offset_paper)
        .ok_or_else(|| "degenerate sheet view — cannot inverse-project offset".to_string())?;
    let offset_world = mid_plus_offset_world - mid_world;

    let direction = (b_world - a_world).try_normalize().unwrap_or(Vec3::X);
    let style = request
        .style
        .unwrap_or_else(|| "architectural_metric".to_string());

    let mut body = serde_json::json!({
        "type": "drafting_dimension",
        "kind": "linear",
        "direction": [direction.x, direction.y, direction.z],
        "a": [a_world.x, a_world.y, a_world.z],
        "b": [b_world.x, b_world.y, b_world.z],
        "offset": [offset_world.x, offset_world.y, offset_world.z],
        "style": style,
    });
    if let Some(text) = request.text_override {
        body["text_override"] = serde_json::Value::String(text);
    }

    handle_create_entity(world, body)
}

#[cfg(feature = "model-api")]
fn resolve_handle_position(
    world: &World,
    element_id: u64,
    handle_id: &str,
) -> Result<Vec3, String> {
    let handles = handle_list_handles(world, element_id)?;
    handles
        .into_iter()
        .find(|handle| handle.id == handle_id)
        .map(|handle| Vec3::new(handle.position.x, handle.position.y, handle.position.z))
        .ok_or_else(|| format!("Entity {element_id} has no handle '{handle_id}'"))
}

#[cfg(feature = "model-api")]
fn handle_place_dimension_between_handles(
    world: &mut World,
    request: PlaceDimensionBetweenHandlesRequest,
) -> Result<u64, String> {
    let start = resolve_handle_position(world, request.start_element_id, &request.start_handle_id)?;
    let end = resolve_handle_position(world, request.end_element_id, &request.end_handle_id)?;
    let params = PlaceDimensionLineRequest {
        start: start.into(),
        end: end.into(),
        line_point: request.line_point,
        offset: request.offset,
        extension: request.extension,
        visible: request.visible,
        label: request.label,
        display_unit: request.display_unit,
        precision: request.precision,
    };
    handle_create_entity(world, create_dimension_line_request_json(&params))
}

#[cfg(feature = "model-api")]
fn handle_frame_model(world: &mut World) -> Result<BoundingBox, String> {
    let bounds = authored_model_bounds(world)
        .ok_or_else(|| "No authored entities with bounds available to frame".to_string())?;
    if !focus_orbit_camera_on_bounds(world, bounds) {
        return Err("No orbit camera available to frame the model".to_string());
    }
    Ok(BoundingBox {
        min: [bounds.min.x, bounds.min.y, bounds.min.z],
        max: [bounds.max.x, bounds.max.y, bounds.max.z],
    })
}

#[cfg(feature = "model-api")]
fn handle_frame_entities(world: &mut World, element_ids: &[u64]) -> Result<BoundingBox, String> {
    let snapshots = capture_snapshots_by_ids(world, element_ids)?;
    let model_snapshots = snapshots
        .iter()
        .filter(|(_, snapshot)| {
            snapshot.scope() == crate::authored_entity::EntityScope::AuthoredModel
        })
        .collect::<Vec<_>>();
    let bounds = aggregate_snapshot_bounds(
        if model_snapshots.is_empty() {
            snapshots
                .iter()
                .map(|(_, snapshot)| snapshot)
                .collect::<Vec<_>>()
        } else {
            model_snapshots
                .into_iter()
                .map(|(_, snapshot)| snapshot)
                .collect()
        }
        .into_iter(),
    )
    .ok_or_else(|| "No bounded entities available to frame".to_string())?;
    if !focus_orbit_camera_on_bounds(world, bounds) {
        return Err("No orbit camera available to frame the entities".to_string());
    }
    Ok(BoundingBox {
        min: [bounds.min.x, bounds.min.y, bounds.min.z],
        max: [bounds.max.x, bounds.max.y, bounds.max.z],
    })
}

#[cfg(feature = "model-api")]
fn handle_save_project(world: &mut World, path: &str) -> Result<String, String> {
    save_project_to_path(world, std::path::PathBuf::from(path))
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(feature = "model-api")]
fn handle_load_project(world: &mut World, path: &str) -> Result<String, String> {
    load_project_from_path(world, std::path::PathBuf::from(path))
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(feature = "model-api")]
fn authored_model_bounds(world: &World) -> Option<crate::authored_entity::EntityBounds> {
    let registry = world.resource::<CapabilityRegistry>();
    let mut query = world.try_query::<EntityRef>()?;
    let mut aggregate = None;

    for entity_ref in query.iter(world) {
        if !scene_light_object_exposed(&entity_ref, world) {
            continue;
        }
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        if snapshot.scope() != crate::authored_entity::EntityScope::AuthoredModel {
            continue;
        }
        let Some(bounds) = snapshot.bounds() else {
            continue;
        };
        aggregate = Some(merge_bounds(aggregate, bounds));
    }

    aggregate
}

#[cfg(feature = "model-api")]
fn aggregate_snapshot_bounds<'a>(
    snapshots: impl Iterator<Item = &'a BoxedEntity>,
) -> Option<crate::authored_entity::EntityBounds> {
    let mut aggregate = None;
    for snapshot in snapshots {
        let Some(bounds) = snapshot.bounds() else {
            continue;
        };
        aggregate = Some(merge_bounds(aggregate, bounds));
    }
    aggregate
}

#[cfg(feature = "model-api")]
fn merge_bounds(
    existing: Option<crate::authored_entity::EntityBounds>,
    bounds: crate::authored_entity::EntityBounds,
) -> crate::authored_entity::EntityBounds {
    match existing {
        Some(existing) => crate::authored_entity::EntityBounds {
            min: existing.min.min(bounds.min),
            max: existing.max.max(bounds.max),
        },
        None => bounds,
    }
}

#[cfg(feature = "model-api")]
fn handle_invoke_command(
    world: &mut World,
    command_id: &str,
    parameters: Value,
) -> Result<Value, String> {
    use crate::plugins::command_registry::{execute_command, CommandResult};

    // Route through the same canonical executor the UI uses, so an MCP
    // `invoke_command` is equivalent to triggering the command from a menu,
    // toolbar or shortcut.
    let result: CommandResult = execute_command(world, command_id, &parameters)?;
    flush_model_api_write_pipeline(world);
    serde_json::to_value(result).map_err(|e| e.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_prepare_site_surface(
    world: &mut World,
    request: PrepareSiteSurfaceRequest,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    use crate::plugins::command_registry::{CommandRegistry, CommandResult};

    let previous_selection = if request.source_element_ids.is_empty() {
        None
    } else {
        Some(handle_get_selection(world))
    };

    if !request.source_element_ids.is_empty() {
        handle_set_selection(world, request.source_element_ids.clone())?;
    }

    let mut parameters = serde_json::Map::new();
    if let Some(name) = request.name {
        parameters.insert("name".to_string(), Value::String(name));
    }
    parameters.insert(
        "delete_source".to_string(),
        Value::Bool(request.delete_source),
    );
    parameters.insert(
        "center_at_origin".to_string(),
        Value::Bool(request.center_at_origin),
    );
    if !request.contour_layers.is_empty() {
        parameters.insert(
            "contour_layers".to_string(),
            Value::Array(
                request
                    .contour_layers
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(join_tolerance) = request.join_tolerance {
        parameters.insert("join_tolerance".to_string(), Value::from(join_tolerance));
    }
    if let Some(drape_sample_spacing) = request.drape_sample_spacing {
        parameters.insert(
            "drape_sample_spacing".to_string(),
            Value::from(drape_sample_spacing),
        );
    }
    if let Some(max_triangle_area) = request.max_triangle_area {
        parameters.insert(
            "max_triangle_area".to_string(),
            Value::from(max_triangle_area),
        );
    }
    if let Some(minimum_angle) = request.minimum_angle {
        parameters.insert("minimum_angle".to_string(), Value::from(minimum_angle));
    }
    if let Some(contour_interval) = request.contour_interval {
        parameters.insert(
            "contour_interval".to_string(),
            Value::from(contour_interval),
        );
    }

    let result = {
        let handler = world
            .resource::<CommandRegistry>()
            .handler_for("terrain.prepare_site_surface")
            .ok_or_else(|| "Unknown command: terrain.prepare_site_surface".to_string())?;
        let result: CommandResult = handler(world, &Value::Object(parameters))?;
        flush_model_api_write_pipeline(world);
        result
    };

    if let Some(previous_selection) = previous_selection {
        let _ = handle_set_selection(world, previous_selection);
    }

    Ok(result)
}

#[cfg(feature = "model-api")]
pub fn handle_terrain_cut_fill_analysis(
    world: &mut World,
    request: TerrainCutFillAnalysisRequest,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    use crate::plugins::command_registry::{CommandRegistry, CommandResult};

    let mut parameters = serde_json::Map::new();
    parameters.insert(
        "existing_surface_id".to_string(),
        Value::from(request.existing_surface_id),
    );
    if let Some(proposed_surface_id) = request.proposed_surface_id {
        parameters.insert(
            "proposed_surface_id".to_string(),
            Value::from(proposed_surface_id),
        );
    }
    if let Some(datum_y) = request.datum_y {
        parameters.insert("datum_y".to_string(), Value::from(datum_y));
    }
    if let Some(sample_spacing) = request.sample_spacing {
        parameters.insert("sample_spacing".to_string(), Value::from(sample_spacing));
    }
    if !request.boundary.is_empty() {
        parameters.insert(
            "boundary".to_string(),
            Value::Array(
                request
                    .boundary
                    .iter()
                    .map(|point| serde_json::json!([point[0], point[1]]))
                    .collect(),
            ),
        );
    }

    let handler = world
        .resource::<CommandRegistry>()
        .handler_for("terrain.cut_fill_analysis")
        .ok_or_else(|| "Unknown command: terrain.cut_fill_analysis".to_string())?;
    let result: CommandResult = handler(world, &Value::Object(parameters))?;
    flush_model_api_write_pipeline(world);
    Ok(result)
}

#[cfg(feature = "model-api")]
pub fn handle_terrain_elevation_at(
    world: &World,
    request: TerrainElevationAtRequest,
) -> Result<Value, String> {
    let registry = world
        .get_resource::<crate::capability_registry::TerrainProviderRegistry>()
        .ok_or_else(|| "Terrain provider registry is unavailable".to_string())?;
    let provider = registry
        .provider()
        .ok_or_else(|| "No terrain provider is registered".to_string())?;
    let elevation = provider
        .elevation_at(world, request.x, request.z)
        .ok_or_else(|| {
            format!(
                "No terrain elevation found at x={}, z={}",
                request.x, request.z
            )
        })?;
    Ok(serde_json::json!({
        "x": request.x,
        "z": request.z,
        "elevation": elevation,
    }))
}

// --- Semantic Assembly / Relation handlers ---

#[cfg(feature = "model-api")]
fn handle_list_vocabulary(world: &World) -> VocabularyInfo {
    let registry = world.resource::<CapabilityRegistry>();
    let mut assembly_patterns = registry
        .assembly_pattern_descriptors()
        .iter()
        .map(|descriptor| assembly_pattern_to_info(descriptor, false, None, true))
        .collect::<Vec<_>>();
    if let Some(drafts) = world
        .get_resource::<crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftRegistry>()
    {
        assembly_patterns.extend(
            drafts
                .list(
                    None,
                    Some(
                        crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftStatus::Installed,
                    ),
                )
                .iter()
                .map(assembly_pattern_draft_to_pattern_info),
        );
    }
    VocabularyInfo {
        assembly_types: registry.assembly_type_descriptors().to_vec(),
        assembly_patterns,
        relation_types: registry.relation_type_descriptors().to_vec(),
    }
}

#[cfg(feature = "model-api")]
pub fn handle_create_assembly(
    world: &mut World,
    request: CreateAssemblyRequest,
) -> Result<CreateAssemblyResult, String> {
    use crate::plugins::modeling::assembly::{
        AssemblyMemberRef, AssemblySnapshot, RelationSnapshot, SemanticAssembly, SemanticRelation,
    };

    // Validate assembly_type against registered vocabulary.
    {
        let registry = world.resource::<CapabilityRegistry>();
        let valid_types: Vec<&str> = registry
            .assembly_type_descriptors()
            .iter()
            .map(|d| d.assembly_type.as_str())
            .collect();
        if !valid_types.contains(&request.assembly_type.as_str()) {
            return Err(format!(
                "Unknown assembly type '{}'. Registered types: {}",
                request.assembly_type,
                valid_types.join(", ")
            ));
        }
    }

    // Validate member targets exist.
    for m in &request.members {
        ensure_entity_exists(world, ElementId(m.target))?;
    }

    // Validate relation types and endpoints.
    {
        let registry = world.resource::<CapabilityRegistry>();
        let valid_rel_types: Vec<&str> = registry
            .relation_type_descriptors()
            .iter()
            .map(|d| d.relation_type.as_str())
            .collect();
        for rel in &request.relations {
            if !valid_rel_types.contains(&rel.relation_type.as_str()) {
                return Err(format!(
                    "Unknown relation type '{}'. Registered types: {}",
                    rel.relation_type,
                    valid_rel_types.join(", ")
                ));
            }
        }
    }
    for rel in &request.relations {
        ensure_entity_exists(world, ElementId(rel.source))?;
        ensure_entity_exists(world, ElementId(rel.target))?;
    }

    let assembly_id = world.resource::<ElementIdAllocator>().next_id();
    let members: Vec<AssemblyMemberRef> = request
        .members
        .iter()
        .map(|m| AssemblyMemberRef {
            target: ElementId(m.target),
            role: m.role.clone(),
        })
        .collect();

    let assembly_snapshot = AssemblySnapshot {
        element_id: assembly_id,
        assembly: SemanticAssembly {
            assembly_type: request.assembly_type,
            label: request.label,
            members,
            parameters: request.parameters,
            metadata: request.metadata,
        },
        refinement_state: None,
    };

    let mut relation_snapshots: Vec<RelationSnapshot> = Vec::new();
    for rel in &request.relations {
        let rel_id = world.resource::<ElementIdAllocator>().next_id();
        relation_snapshots.push(RelationSnapshot {
            element_id: rel_id,
            relation: SemanticRelation {
                source: ElementId(rel.source),
                target: ElementId(rel.target),
                relation_type: rel.relation_type.clone(),
                parameters: rel.parameters.clone(),
            },
        });
    }

    // Emit all creates as one command group for atomic undo.
    // The command pipeline handles apply_to — no eager world mutation here.
    send_event(
        world,
        BeginCommandGroup {
            label: "Create Assembly",
        },
    );

    send_event(
        world,
        CreateEntityCommand {
            snapshot: assembly_snapshot.into(),
        },
    );

    let mut relation_ids = Vec::new();
    for snapshot in relation_snapshots {
        relation_ids.push(snapshot.element_id.0);
        send_event(
            world,
            CreateEntityCommand {
                snapshot: snapshot.into(),
            },
        );
    }

    send_event(world, EndCommandGroup);
    flush_model_api_write_pipeline(world);

    Ok(CreateAssemblyResult {
        assembly_id: assembly_id.0,
        relation_ids,
    })
}

#[cfg(feature = "model-api")]
pub fn handle_get_assembly(world: &World, element_id: u64) -> Result<AssemblyDetails, String> {
    use crate::plugins::modeling::assembly::SemanticAssembly;

    let eid = ElementId(element_id);
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    let assembly = entity_ref
        .get::<SemanticAssembly>()
        .ok_or_else(|| format!("Entity {element_id} is not a semantic assembly"))?;

    let members = enrich_assembly_members(world, assembly);

    Ok(AssemblyDetails {
        element_id,
        assembly_type: assembly.assembly_type.clone(),
        label: assembly.label.clone(),
        members,
        parameters: assembly.parameters.clone(),
        metadata: assembly.metadata.clone(),
    })
}

#[cfg(feature = "model-api")]
pub fn handle_list_assemblies(world: &World) -> Vec<AssemblyEntry> {
    use crate::plugins::modeling::assembly::SemanticAssembly;

    let mut entries = Vec::new();
    let mut q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in q.iter(world) {
        let (Some(eid), Some(assembly)) = (
            entity_ref.get::<ElementId>(),
            entity_ref.get::<SemanticAssembly>(),
        ) else {
            continue;
        };
        entries.push(AssemblyEntry {
            element_id: eid.0,
            assembly_type: assembly.assembly_type.clone(),
            label: assembly.label.clone(),
            member_count: assembly.members.len(),
        });
    }
    entries.sort_by_key(|e| e.element_id);
    entries
}

#[cfg(feature = "model-api")]
pub fn handle_query_relations(
    world: &World,
    source: Option<u64>,
    target: Option<u64>,
    relation_type: Option<String>,
) -> Vec<RelationEntry> {
    use crate::plugins::modeling::assembly::SemanticRelation;

    let mut entries = Vec::new();
    let mut q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in q.iter(world) {
        let (Some(eid), Some(rel)) = (
            entity_ref.get::<ElementId>(),
            entity_ref.get::<SemanticRelation>(),
        ) else {
            continue;
        };
        if let Some(src) = source {
            if rel.source.0 != src {
                continue;
            }
        }
        if let Some(tgt) = target {
            if rel.target.0 != tgt {
                continue;
            }
        }
        if let Some(ref rt) = relation_type {
            if &rel.relation_type != rt {
                continue;
            }
        }
        entries.push(RelationEntry {
            element_id: eid.0,
            source: rel.source.0,
            target: rel.target.0,
            relation_type: rel.relation_type.clone(),
            parameters: rel.parameters.clone(),
        });
    }
    entries.sort_by_key(|e| e.element_id);
    entries
}

#[cfg(feature = "model-api")]
pub fn handle_list_assembly_members(
    world: &World,
    element_id: u64,
) -> Result<Vec<AssemblyMemberEntry>, String> {
    use crate::plugins::modeling::assembly::SemanticAssembly;

    let eid = ElementId(element_id);
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    let assembly = entity_ref
        .get::<SemanticAssembly>()
        .ok_or_else(|| format!("Entity {element_id} is not a semantic assembly"))?;

    Ok(enrich_assembly_members(world, assembly))
}

#[cfg(feature = "model-api")]
fn enrich_assembly_members(
    world: &World,
    assembly: &crate::plugins::modeling::assembly::SemanticAssembly,
) -> Vec<AssemblyMemberEntry> {
    use crate::plugins::modeling::assembly::SemanticAssembly as SA;

    let registry = world.resource::<CapabilityRegistry>();
    assembly
        .members
        .iter()
        .map(|member| {
            let mut q = world.try_query::<EntityRef>().unwrap();
            let member_entity = q
                .iter(world)
                .find(|e| e.get::<ElementId>().copied() == Some(member.target));

            let (member_kind, member_type, label) = match member_entity {
                Some(ref entity_ref) if entity_ref.get::<SA>().is_some() => {
                    let sub_assembly = entity_ref.get::<SA>().unwrap();
                    (
                        "assembly".to_string(),
                        sub_assembly.assembly_type.clone(),
                        sub_assembly.label.clone(),
                    )
                }
                Some(ref entity_ref) => match registry.capture_snapshot(entity_ref, world) {
                    Some(snapshot) => (
                        "entity".to_string(),
                        snapshot.type_name().to_string(),
                        snapshot.label(),
                    ),
                    None => (
                        "entity".to_string(),
                        "unknown".to_string(),
                        format!("#{}", member.target.0),
                    ),
                },
                None => (
                    "unknown".to_string(),
                    "missing".to_string(),
                    format!("#{} (missing)", member.target.0),
                ),
            };

            AssemblyMemberEntry {
                target: member.target.0,
                role: member.role.clone(),
                member_kind,
                member_type,
                label,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Definition / Occurrence handlers
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn parse_param_type(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::ParamType, String> {
    use crate::plugins::modeling::definition::ParamType;

    match value.and_then(|value| value.as_str()).unwrap_or("Numeric") {
        "Numeric" => Ok(ParamType::Numeric),
        "Boolean" => Ok(ParamType::Boolean),
        "StringVal" => Ok(ParamType::StringVal),
        "AxisRef" => Ok(ParamType::AxisRef),
        "ParameterRef" => Err(
            "ParameterRef parameters must provide param_type as an object with side".to_string(),
        ),
        "Enum" => Err("Enum parameters must provide param_type as an object or array".to_string()),
        other => Err(format!("Unsupported param_type '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_param_type_value(
    value: &Value,
) -> Result<crate::plugins::modeling::definition::ParamType, String> {
    use crate::plugins::modeling::definition::{BindingSide, ParamType};

    if let Some(string) = value.as_str() {
        return parse_param_type(Some(&Value::String(string.to_string())));
    }

    if let Some(object) = value.as_object() {
        let kind = object
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or("Numeric");
        return match kind {
            "Numeric" => Ok(ParamType::Numeric),
            "Boolean" => Ok(ParamType::Boolean),
            "StringVal" => Ok(ParamType::StringVal),
            "AxisRef" => Ok(ParamType::AxisRef),
            "ParameterRef" => {
                let side = object
                    .get("side")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| "ParameterRef param_type requires a 'side'".to_string())?;
                let side = match side {
                    "Host" => BindingSide::Host,
                    "Hosted" => BindingSide::Hosted,
                    other => {
                        return Err(format!(
                            "ParameterRef side must be Host or Hosted, got '{other}'"
                        ));
                    }
                };
                Ok(ParamType::ParameterRef { side })
            }
            "Enum" => {
                let variants = object
                    .get("variants")
                    .and_then(|value| value.as_array())
                    .ok_or_else(|| "Enum param_type requires a 'variants' array".to_string())?
                    .iter()
                    .map(|variant| {
                        variant
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "Enum variants must be strings".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ParamType::Enum(variants))
            }
            other => Err(format!("Unsupported param_type '{other}'")),
        };
    }

    Err("param_type must be a string or object".to_string())
}

#[cfg(feature = "model-api")]
fn parse_override_policy(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::OverridePolicy, String> {
    use crate::plugins::modeling::definition::OverridePolicy;

    match value
        .and_then(|value| value.as_str())
        .unwrap_or("Overridable")
    {
        "Locked" => Ok(OverridePolicy::Locked),
        "Overridable" => Ok(OverridePolicy::Overridable),
        "Required" => Ok(OverridePolicy::Required),
        other => Err(format!("Unsupported override_policy '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_parameter_metadata(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::ParameterMetadata, String> {
    use crate::plugins::modeling::definition::{
        ParameterMetadata, ParameterMutability, ParameterScaleBehavior,
    };

    let Some(value) = value else {
        return Ok(ParameterMetadata::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "parameter metadata must be an object".to_string())?;
    let mutability = match object
        .get("mutability")
        .and_then(|value| value.as_str())
        .unwrap_or("Input")
    {
        "Input" => ParameterMutability::Input,
        "Derived" => ParameterMutability::Derived,
        other => return Err(format!("Unsupported parameter mutability '{other}'")),
    };
    let scale_behavior = object
        .get("scale_behavior")
        .and_then(|value| value.as_str())
        .map(|value| match value {
            "scale_with_occurrence" => Ok(ParameterScaleBehavior::ScaleWithOccurrence),
            "fixed_world" => Ok(ParameterScaleBehavior::FixedWorld),
            "ratio" => Ok(ParameterScaleBehavior::Ratio),
            "semantic" => Ok(ParameterScaleBehavior::Semantic),
            other => Err(format!("Unsupported parameter scale_behavior '{other}'")),
        })
        .transpose()?;

    Ok(ParameterMetadata {
        unit: object
            .get("unit")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        min: object.get("min").cloned(),
        max: object.get("max").cloned(),
        step: object.get("step").cloned(),
        category: object
            .get("category")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        mutability,
        scale_behavior,
    })
}

#[cfg(feature = "model-api")]
fn parse_parameter_schema(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::ParameterSchema, String> {
    use crate::plugins::modeling::definition::{ParameterDef, ParameterSchema};

    let Some(value) = value else {
        return Ok(ParameterSchema::default());
    };

    let parameters = value
        .as_array()
        .ok_or_else(|| "'parameters' must be an array".to_string())?
        .iter()
        .map(|parameter| {
            let object = parameter
                .as_object()
                .ok_or_else(|| "each parameter must be an object".to_string())?;
            let name = object
                .get("name")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "parameter missing 'name'".to_string())?
                .to_string();
            let param_type = object
                .get("param_type")
                .map(parse_param_type_value)
                .transpose()?
                .unwrap_or(crate::plugins::modeling::definition::ParamType::Numeric);
            // Accept `default_value` (canonical) or `default` (alias).
            // `default_value` takes precedence if both are present.
            let default_value = object
                .get("default_value")
                .or_else(|| object.get("default"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let override_policy = parse_override_policy(object.get("override_policy"))?;
            let geometry_affecting = object
                .get("geometry_affecting")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let metadata = parse_parameter_metadata(object.get("metadata"))?;
            Ok(ParameterDef {
                name,
                param_type,
                default_value,
                override_policy,
                geometry_affecting,
                metadata,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(ParameterSchema(parameters))
}

#[cfg(feature = "model-api")]
fn parse_optional_void_declaration(
    value: Option<&Value>,
) -> Result<Option<crate::plugins::modeling::void_declaration::VoidDeclaration>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|e| format!("void_declaration must be a typed VoidDeclaration JSON: {e}"))
}

#[cfg(feature = "model-api")]
fn parse_definition_kind(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::DefinitionKind, String> {
    use crate::plugins::modeling::definition::DefinitionKind;

    match value.and_then(|value| value.as_str()).unwrap_or("Solid") {
        "Solid" => Ok(DefinitionKind::Solid),
        "Annotation" => Ok(DefinitionKind::Annotation),
        other => Err(format!("Unsupported definition_kind '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_representation_kind(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::RepresentationKind, String> {
    parse_representation_kind_label(
        value
            .and_then(|value| value.as_str())
            .unwrap_or("PrimaryGeometry"),
    )
}

#[cfg(feature = "model-api")]
fn parse_representation_kind_label(
    value: &str,
) -> Result<crate::plugins::modeling::definition::RepresentationKind, String> {
    use crate::plugins::modeling::definition::RepresentationKind;

    let label = value.trim();
    if label.is_empty() {
        return Err("representation kind must be non-empty".to_string());
    }
    match label.to_ascii_lowercase().as_str() {
        "primarygeometry" | "primary_geometry" | "primary" => {
            Ok(RepresentationKind::PrimaryGeometry)
        }
        "annotation" => Ok(RepresentationKind::Annotation),
        "reference" => Ok(RepresentationKind::Reference),
        other => Err(format!("Unsupported representation kind '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_representation_role(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::RepresentationRole, String> {
    parse_representation_role_label(value.and_then(|value| value.as_str()).unwrap_or("Body"))
}

#[cfg(feature = "model-api")]
fn parse_representation_role_label(
    value: &str,
) -> Result<crate::plugins::modeling::definition::RepresentationRole, String> {
    use crate::plugins::modeling::definition::RepresentationRole;

    let label = value.trim();
    if label.is_empty() {
        return Err("representation role must be non-empty".to_string());
    }
    Ok(match label.to_ascii_lowercase().as_str() {
        "body" => RepresentationRole::Body,
        "axis" => RepresentationRole::Axis,
        "footprint" | "foot_print" => RepresentationRole::Footprint,
        "boundingbox" | "bounding_box" | "box" => RepresentationRole::BoundingBox,
        "annotation" => RepresentationRole::Annotation,
        "cog" | "centerofgravity" | "center_of_gravity" | "centreofgravity"
        | "centre_of_gravity" => RepresentationRole::CoG,
        _ => RepresentationRole::Custom(label.to_string()),
    })
}

#[cfg(feature = "model-api")]
fn parse_level_of_detail_label(
    value: &str,
) -> Result<crate::plugins::modeling::definition::LevelOfDetail, String> {
    use crate::plugins::modeling::definition::LevelOfDetail;

    match value.trim().to_ascii_lowercase().as_str() {
        "conceptual" => Ok(LevelOfDetail::Conceptual),
        "schematic" => Ok(LevelOfDetail::Schematic),
        "detailed" | "detail" => Ok(LevelOfDetail::Detailed),
        "fabrication" | "fabrication_ready" => Ok(LevelOfDetail::Fabrication),
        other => Err(format!("Unsupported level of detail '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_update_policy_label(
    value: &str,
) -> Result<crate::plugins::modeling::definition::UpdatePolicy, String> {
    use crate::plugins::modeling::definition::UpdatePolicy;

    match value.trim().to_ascii_lowercase().as_str() {
        "always" => Ok(UpdatePolicy::Always),
        "ondemand" | "on_demand" | "on-demand" => Ok(UpdatePolicy::OnDemand),
        "frozen" => Ok(UpdatePolicy::Frozen),
        other => Err(format!("Unsupported update policy '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_representations(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<crate::plugins::modeling::definition::RepresentationDecl>, String> {
    use crate::plugins::modeling::definition::{
        RepresentationDecl, RepresentationKind, RepresentationRole,
    };

    if let Some(value) = object.get("representations") {
        return value
            .as_array()
            .ok_or_else(|| "'representations' must be an array".to_string())?
            .iter()
            .map(|representation| {
                let representation = representation
                    .as_object()
                    .ok_or_else(|| "each representation must be an object".to_string())?;
                let kind = parse_representation_kind(representation.get("kind"))?;
                let role = parse_representation_role(representation.get("role"))?;
                let lod = representation
                    .get("lod")
                    .and_then(|value| value.as_str())
                    .map(parse_level_of_detail_label)
                    .transpose()?;
                let update_policy = representation
                    .get("update_policy")
                    .and_then(|value| value.as_str())
                    .map(parse_update_policy_label)
                    .transpose()?;
                Ok(RepresentationDecl {
                    kind,
                    role,
                    lod,
                    update_policy,
                })
            })
            .collect();
    }

    Ok(vec![RepresentationDecl::new(
        RepresentationKind::PrimaryGeometry,
        RepresentationRole::Body,
    )])
}

#[cfg(feature = "model-api")]
fn parse_evaluators(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<crate::plugins::modeling::definition::EvaluatorDecl>, String> {
    use crate::plugins::modeling::definition::{EvaluatorDecl, RectangularExtrusionEvaluator};

    if let Some(value) = object.get("evaluators") {
        return value
            .as_array()
            .ok_or_else(|| "'evaluators' must be an array".to_string())?
            .iter()
            .map(|evaluator| {
                let evaluator = evaluator
                    .as_object()
                    .ok_or_else(|| "each evaluator must be an object".to_string())?;
                let kind = evaluator
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("RectangularExtrusion");
                match kind {
                    "RectangularExtrusion" => Ok(EvaluatorDecl::RectangularExtrusion(
                        RectangularExtrusionEvaluator {
                            width_param: evaluator
                                .get("width_param")
                                .and_then(|value| value.as_str())
                                .unwrap_or("width")
                                .to_string(),
                            depth_param: evaluator
                                .get("depth_param")
                                .and_then(|value| value.as_str())
                                .unwrap_or("depth")
                                .to_string(),
                            height_param: evaluator
                                .get("height_param")
                                .and_then(|value| value.as_str())
                                .unwrap_or("height")
                                .to_string(),
                        },
                    )),
                    other => Err(format!("Unsupported evaluator kind '{other}'")),
                }
            })
            .collect();
    }

    // Bind the default extrusion evaluator to the caller's ACTUAL parameter
    // names so a leaf can never silently render zero-size geometry from a
    // name mismatch. Resolution per axis: explicit `<axis>_param` override →
    // a param named exactly `<axis>` → a param whose name contains `<axis>`
    // (e.g. `width_mm`) → the param in that positional slot → the literal axis.
    let param_names: Vec<String> = object
        .get("parameters")
        .and_then(|value| value.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    p.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let resolve = |axis: &str, idx: usize| -> String {
        if let Some(explicit) = object
            .get(&format!("{axis}_param"))
            .and_then(|value| value.as_str())
        {
            return explicit.to_string();
        }
        if param_names.iter().any(|n| n == axis) {
            return axis.to_string();
        }
        if let Some(n) = param_names
            .iter()
            .find(|n| n.to_ascii_lowercase().contains(axis))
        {
            return n.clone();
        }
        if let Some(n) = param_names.get(idx) {
            return n.clone();
        }
        axis.to_string()
    };
    Ok(vec![EvaluatorDecl::RectangularExtrusion(
        RectangularExtrusionEvaluator {
            width_param: resolve("width", 0),
            height_param: resolve("height", 1),
            depth_param: resolve("depth", 2),
        },
    )])
}

#[cfg(feature = "model-api")]
fn parse_optional_compound(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<crate::plugins::modeling::definition::CompoundDefinition>, String> {
    object
        .get("compound")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid 'compound': {error}"))
}

#[cfg(feature = "model-api")]
fn parse_optional_base_definition_id(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<crate::plugins::modeling::definition::DefinitionId>, String> {
    object
        .get("base_definition_id")
        .map(|value| {
            if value.is_null() {
                Ok(None)
            } else {
                value
                    .as_str()
                    .map(|id| {
                        Some(crate::plugins::modeling::definition::DefinitionId(
                            id.to_string(),
                        ))
                    })
                    .ok_or_else(|| "'base_definition_id' must be a string or null".to_string())
            }
        })
        .transpose()
        .map(Option::flatten)
}

#[cfg(feature = "model-api")]
fn definition_to_entry(
    def: &crate::plugins::modeling::definition::Definition,
    effective_def: &crate::plugins::modeling::definition::Definition,
) -> DefinitionEntry {
    DefinitionEntry {
        definition_id: def.id.to_string(),
        name: def.name.clone(),
        definition_kind: format!("{:?}", effective_def.definition_kind),
        definition_version: def.definition_version,
        parameter_names: effective_def
            .interface
            .parameters
            .0
            .iter()
            .map(|p| p.name.clone())
            .collect(),
        full: serde_json::to_value(def).unwrap_or(serde_json::Value::Null),
        effective_full: serde_json::to_value(effective_def).unwrap_or(serde_json::Value::Null),
    }
}

#[cfg(feature = "model-api")]
fn definition_library_to_entry(
    library: &crate::plugins::modeling::definition::DefinitionLibrary,
) -> DefinitionLibraryEntry {
    let summary = library.summary();
    DefinitionLibraryEntry {
        library_id: summary.library_id,
        name: summary.name,
        scope: summary.scope,
        definition_count: summary.definition_count,
        source_path: summary.source_path,
    }
}

#[cfg(feature = "model-api")]
fn draft_to_entry(
    definitions: &crate::plugins::modeling::definition::DefinitionRegistry,
    libraries: &crate::plugins::modeling::definition::DefinitionLibraryRegistry,
    draft: &crate::plugins::definition_authoring::DefinitionDraft,
) -> DefinitionDraftEntry {
    let effective_full = crate::plugins::definition_authoring::draft_effective_definition(
        definitions,
        libraries,
        draft,
    )
    .ok()
    .and_then(|effective| serde_json::to_value(effective).ok())
    .unwrap_or(Value::Null);

    DefinitionDraftEntry {
        draft_id: draft.draft_id.to_string(),
        source_definition_id: draft.source_definition_id.as_ref().map(ToString::to_string),
        source_library_id: draft.source_library_id.as_ref().map(ToString::to_string),
        definition_id: draft.working_copy.id.to_string(),
        name: draft.working_copy.name.clone(),
        definition_version: draft.working_copy.definition_version,
        dirty: draft.dirty,
        full: serde_json::to_value(&draft.working_copy).unwrap_or(Value::Null),
        effective_full,
    }
}

#[cfg(feature = "model-api")]
fn compile_summary_to_result(
    effective_full: Value,
    summary: crate::plugins::definition_authoring::DefinitionCompileSummary,
) -> DefinitionCompileResult {
    DefinitionCompileResult {
        target_id: summary.target_id,
        effective_full,
        nodes: summary.nodes,
        edges: summary
            .edges
            .into_iter()
            .map(|edge| DefinitionCompileEdge {
                from: edge.from,
                to: edge.to,
            })
            .collect(),
        child_slot_count: summary.child_slot_count,
        collection_slots: summary
            .collection_slots
            .into_iter()
            .map(|slot| DefinitionCollectionSlotResult {
                slot_id: slot.slot_id,
                count: slot.count,
                layout: slot.layout,
            })
            .collect(),
        derived_parameter_count: summary.derived_parameter_count,
        constraint_count: summary.constraint_count,
        anchor_count: summary.anchor_count,
    }
}

#[cfg(feature = "model-api")]
fn definition_explain_value_to_result(value: Value) -> ApiResult<DefinitionExplainResult> {
    let object = value
        .as_object()
        .ok_or_else(|| "definition.explain result must be a JSON object".to_string())?;
    let raw_full = object.get("raw_full").cloned().unwrap_or(Value::Null);
    let effective_full = object.get("effective_full").cloned().unwrap_or(Value::Null);
    let local_parameter_names = serde_json::from_value::<Vec<String>>(
        object
            .get("local_parameter_names")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| error.to_string())?;
    let inherited_parameter_names = serde_json::from_value::<Vec<String>>(
        object
            .get("inherited_parameter_names")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| error.to_string())?;
    let local_child_slot_ids = serde_json::from_value::<Vec<String>>(
        object
            .get("local_child_slot_ids")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| error.to_string())?;
    let inherited_child_slot_ids = serde_json::from_value::<Vec<String>>(
        object
            .get("inherited_child_slot_ids")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| error.to_string())?;
    let resolved_collection_slots = serde_json::from_value::<Vec<Value>>(
        object
            .get("resolved_collection_slots")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| error.to_string())?;
    let compile_summary =
        serde_json::from_value::<crate::plugins::definition_authoring::DefinitionCompileSummary>(
            object
                .get("compile")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )
        .map_err(|error| error.to_string())?;
    let compile = compile_summary_to_result(effective_full.clone(), compile_summary);

    Ok(DefinitionExplainResult {
        target_id: compile.target_id.clone(),
        raw_full,
        effective_full,
        local_parameter_names,
        inherited_parameter_names,
        local_child_slot_ids,
        inherited_child_slot_ids,
        resolved_collection_slots,
        compile,
    })
}

#[cfg(feature = "model-api")]
fn build_definition_from_object(
    object: &serde_json::Map<String, Value>,
) -> Result<crate::plugins::modeling::definition::Definition, String> {
    use crate::plugins::modeling::definition::{Definition, DefinitionId, Interface};

    let name = object
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'name'".to_string())?
        .to_string();

    Ok(Definition {
        id: DefinitionId::new(),
        base_definition_id: parse_optional_base_definition_id(object)?,
        name,
        definition_kind: parse_definition_kind(object.get("definition_kind"))?,
        definition_version: 1,
        interface: Interface {
            parameters: parse_parameter_schema(object.get("parameters"))?,
            void_declaration: parse_optional_void_declaration(object.get("void_declaration"))?,
            external_context_requirements: Vec::new(),
        },
        evaluators: parse_evaluators(object)?,
        representations: parse_representations(object)?,
        compound: parse_optional_compound(object)?,
        material_assignment: None,
        visibility: crate::plugins::modeling::definition::DefinitionVisibility::PublicRoot,
        domain_data: object.get("domain_data").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(feature = "model-api")]
fn resolve_definition_analysis_target(
    world: &World,
    object: &serde_json::Map<String, Value>,
) -> ApiResult<(
    crate::plugins::modeling::definition::DefinitionRegistry,
    crate::plugins::modeling::definition::Definition,
)> {
    let definitions = world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
    let libraries =
        world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();

    if let Some(draft_id) = object.get("draft_id").and_then(Value::as_str) {
        let drafts =
            world.resource::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
        let draft = drafts
            .get(&crate::plugins::definition_authoring::DefinitionDraftId(
                draft_id.to_string(),
            ))
            .ok_or_else(|| format!("Definition draft '{}' not found", draft_id))?;
        let preview = crate::plugins::definition_authoring::preview_registry_for_draft(
            definitions,
            libraries,
            draft,
        )?;
        Ok((preview, draft.working_copy.clone()))
    } else {
        let definition_id = object
            .get("definition_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Provide either 'draft_id' or 'definition_id'".to_string())?;
        let library_id = object.get("library_id").and_then(Value::as_str);
        let (definition, _, _, _) =
            crate::plugins::definition_authoring::resolve_definition_for_authoring(
                definitions,
                libraries,
                definition_id,
                library_id,
            )?;
        let mut preview = definitions.clone();
        if let Some(library_id) = library_id {
            let library = libraries
                .get(&crate::plugins::modeling::definition::DefinitionLibraryId(
                    library_id.to_string(),
                ))
                .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
            for library_definition in library.definitions.values() {
                preview.insert(library_definition.clone());
            }
        }
        Ok((preview, definition))
    }
}

#[cfg(feature = "model-api")]
/// List definitions in the registry.
///
/// When `include_internal` is `false` (the default for MCP exposure and the
/// browser), `InternalPart` definitions are excluded so agents and users see
/// only user-facing families.  Pass `true` to retrieve the full set for
/// debugging, parent navigation, or migration work.
pub fn handle_list_definitions(world: &World) -> Vec<DefinitionEntry> {
    handle_list_definitions_filtered(world, false)
}

#[cfg(feature = "model-api")]
pub fn handle_list_definitions_filtered(
    world: &World,
    include_internal: bool,
) -> Vec<DefinitionEntry> {
    use crate::plugins::modeling::definition::{DefinitionRegistry, DefinitionVisibility};
    let registry = world.resource::<DefinitionRegistry>();
    registry
        .list()
        .into_iter()
        .filter(|definition| {
            include_internal || definition.visibility != DefinitionVisibility::InternalPart
        })
        .filter_map(|definition| {
            registry
                .effective_definition(&definition.id)
                .ok()
                .map(|effective| definition_to_entry(definition, &effective))
        })
        .collect()
}

#[cfg(feature = "model-api")]
pub fn handle_get_definition(world: &World, definition_id: String) -> ApiResult<DefinitionEntry> {
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};
    let id = DefinitionId(definition_id.clone());
    let registry = world.resource::<DefinitionRegistry>();
    let definition = registry
        .get(&id)
        .ok_or_else(|| format!("Definition '{definition_id}' not found"))?;
    let effective = registry.effective_definition(&id)?;
    Ok(definition_to_entry(definition, &effective))
}

#[cfg(feature = "model-api")]
pub fn handle_list_definition_libraries(world: &World) -> Vec<DefinitionLibraryEntry> {
    use crate::plugins::modeling::definition::DefinitionLibraryRegistry;

    world
        .resource::<DefinitionLibraryRegistry>()
        .list()
        .into_iter()
        .map(definition_library_to_entry)
        .collect()
}

#[cfg(feature = "model-api")]
pub fn handle_get_definition_library(world: &World, library_id: String) -> ApiResult<Value> {
    use crate::plugins::modeling::definition::{DefinitionLibraryId, DefinitionLibraryRegistry};

    let id = DefinitionLibraryId(library_id.clone());
    let library = world
        .resource::<DefinitionLibraryRegistry>()
        .get(&id)
        .ok_or_else(|| format!("Definition library '{library_id}' not found"))?;

    serde_json::to_value(library).map_err(|error| error.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_create_definition(world: &mut World, request: Value) -> ApiResult<DefinitionEntry> {
    use crate::plugins::commands::enqueue_create_definition;
    use crate::plugins::modeling::definition::DefinitionRegistry;

    let obj = request
        .as_object()
        .ok_or_else(|| "definition.create expects a JSON object".to_string())?;

    let definition = build_definition_from_object(obj)?;
    let entry = {
        let registry = world.resource::<DefinitionRegistry>();
        registry.validate_definition(&definition)?;
        let mut preview = registry.clone();
        preview.insert(definition.clone());
        let effective = preview.effective_definition(&definition.id)?;
        definition_to_entry(&definition, &effective)
    };
    enqueue_create_definition(world, definition);
    flush_model_api_write_pipeline(world);
    Ok(entry)
}

#[cfg(feature = "model-api")]
pub fn handle_update_definition(world: &mut World, request: Value) -> ApiResult<DefinitionEntry> {
    use crate::plugins::commands::enqueue_update_definition;
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};

    let obj = request
        .as_object()
        .ok_or_else(|| "definition.update expects a JSON object".to_string())?;

    let id_str = obj
        .get("definition_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'definition_id'".to_string())?;
    let id = DefinitionId(id_str.to_string());

    let before = world
        .resource::<DefinitionRegistry>()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("Definition '{id_str}' not found"))?;

    let mut after = before.clone();
    after.definition_version += 1;

    if let Some(name_val) = obj.get("name") {
        if let Some(n) = name_val.as_str() {
            after.name = n.to_string();
        }
    }

    if obj.contains_key("definition_kind") {
        after.definition_kind = parse_definition_kind(obj.get("definition_kind"))?;
    }

    if obj.contains_key("base_definition_id") {
        after.base_definition_id = parse_optional_base_definition_id(obj)?;
    }

    if obj.contains_key("parameters") {
        after.interface.parameters = parse_parameter_schema(obj.get("parameters"))?;
    }

    if obj.contains_key("void_declaration") {
        after.interface.void_declaration =
            parse_optional_void_declaration(obj.get("void_declaration"))?;
    }

    if obj.contains_key("evaluators")
        || obj.contains_key("width_param")
        || obj.contains_key("depth_param")
        || obj.contains_key("height_param")
    {
        after.evaluators = parse_evaluators(obj)?;
    }

    if obj.contains_key("representations") {
        after.representations = parse_representations(obj)?;
    }

    if obj.contains_key("compound") {
        after.compound = parse_optional_compound(obj)?;
    }

    if obj.contains_key("domain_data") {
        after.domain_data = obj.get("domain_data").cloned().unwrap_or(Value::Null);
    }

    let entry = {
        let registry = world.resource::<DefinitionRegistry>();
        registry.validate_definition(&after)?;
        let mut preview = registry.clone();
        preview.insert(after.clone());
        let effective = preview.effective_definition(&after.id)?;
        definition_to_entry(&after, &effective)
    };
    enqueue_update_definition(world, before, after);
    flush_model_api_write_pipeline(world);
    Ok(entry)
}

#[cfg(feature = "model-api")]
fn mutate_definition_representations<F>(
    world: &mut World,
    definition_id: &str,
    mutate: F,
) -> ApiResult<DefinitionEntry>
where
    F: FnOnce(&mut crate::plugins::modeling::definition::Definition) -> Result<(), String>,
{
    use crate::plugins::commands::enqueue_update_definition;
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};

    let id_str = definition_id.trim();
    if id_str.is_empty() {
        return Err("definition_id must be non-empty".to_string());
    }
    let id = DefinitionId(id_str.to_string());
    let before = world
        .resource::<DefinitionRegistry>()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("Definition '{id_str}' not found"))?;

    let mut after = before.clone();
    mutate(&mut after)?;
    after.definition_version += 1;

    let entry = {
        let registry = world.resource::<DefinitionRegistry>();
        registry.validate_definition(&after)?;
        let mut preview = registry.clone();
        preview.insert(after.clone());
        let effective = preview.effective_definition(&after.id)?;
        definition_to_entry(&after, &effective)
    };
    enqueue_update_definition(world, before, after);
    flush_model_api_write_pipeline(world);
    Ok(entry)
}

#[cfg(feature = "model-api")]
fn find_representation_index(
    definition_name: &str,
    representations: &[crate::plugins::modeling::definition::RepresentationDecl],
    kind: &crate::plugins::modeling::definition::RepresentationKind,
    role: Option<&crate::plugins::modeling::definition::RepresentationRole>,
) -> Result<usize, String> {
    let matches: Vec<usize> = representations
        .iter()
        .enumerate()
        .filter_map(|(index, representation)| {
            if &representation.kind == kind && role.is_none_or(|role| &representation.role == role)
            {
                Some(index)
            } else {
                None
            }
        })
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "Definition '{definition_name}' has no representation matching kind {:?}",
            kind
        )),
        [index] => Ok(*index),
        _ => Err(format!(
            "Definition '{definition_name}' has multiple representations matching kind {:?}; provide role",
            kind
        )),
    }
}

#[cfg(feature = "model-api")]
pub fn handle_representation_declare(
    world: &mut World,
    request: RepresentationDeclareRequest,
) -> ApiResult<DefinitionEntry> {
    use crate::plugins::modeling::definition::RepresentationDecl;

    let kind = parse_representation_kind_label(&request.kind)?;
    let role = request
        .role
        .as_deref()
        .map(parse_representation_role_label)
        .transpose()?
        .unwrap_or(crate::plugins::modeling::definition::RepresentationRole::Body);
    let lod = request
        .lod
        .as_deref()
        .map(parse_level_of_detail_label)
        .transpose()?;
    let update_policy = request
        .update_policy
        .as_deref()
        .map(parse_update_policy_label)
        .transpose()?;

    mutate_definition_representations(world, &request.definition_id, move |definition| {
        let declaration = RepresentationDecl {
            kind: kind.clone(),
            role: role.clone(),
            lod,
            update_policy,
        };
        if let Some(existing) = definition
            .representations
            .iter_mut()
            .find(|representation| representation.kind == kind && representation.role == role)
        {
            *existing = declaration;
        } else {
            definition.representations.push(declaration);
        }
        Ok(())
    })
}

#[cfg(feature = "model-api")]
pub fn handle_representation_set_lod(
    world: &mut World,
    request: RepresentationSetLodRequest,
) -> ApiResult<DefinitionEntry> {
    let kind = parse_representation_kind_label(&request.kind)?;
    let role = request
        .role
        .as_deref()
        .map(parse_representation_role_label)
        .transpose()?;
    let lod = parse_level_of_detail_label(&request.lod)?;

    mutate_definition_representations(world, &request.definition_id, move |definition| {
        let index = find_representation_index(
            &definition.name,
            &definition.representations,
            &kind,
            role.as_ref(),
        )?;
        definition.representations[index].lod = Some(lod);
        Ok(())
    })
}

#[cfg(feature = "model-api")]
pub fn handle_representation_set_update_policy(
    world: &mut World,
    request: RepresentationSetUpdatePolicyRequest,
) -> ApiResult<DefinitionEntry> {
    let kind = parse_representation_kind_label(&request.kind)?;
    let role = request
        .role
        .as_deref()
        .map(parse_representation_role_label)
        .transpose()?;
    let update_policy = parse_update_policy_label(&request.update_policy)?;

    mutate_definition_representations(world, &request.definition_id, move |definition| {
        let index = find_representation_index(
            &definition.name,
            &definition.representations,
            &kind,
            role.as_ref(),
        )?;
        definition.representations[index].update_policy = Some(update_policy);
        Ok(())
    })
}

#[cfg(feature = "model-api")]
pub fn handle_list_definition_drafts(world: &World) -> Vec<DefinitionDraftEntry> {
    let definitions = world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
    let libraries =
        world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();
    let drafts = world.resource::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
    drafts
        .list()
        .into_iter()
        .map(|draft| draft_to_entry(definitions, libraries, draft))
        .collect()
}

#[cfg(feature = "model-api")]
pub fn handle_get_definition_draft(
    world: &World,
    draft_id: String,
) -> ApiResult<DefinitionDraftEntry> {
    let draft_id = crate::plugins::definition_authoring::DefinitionDraftId(draft_id);
    let definitions = world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
    let libraries =
        world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();
    let drafts = world.resource::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
    let draft = drafts
        .get(&draft_id)
        .ok_or_else(|| format!("Definition draft '{}' not found", draft_id))?;
    Ok(draft_to_entry(definitions, libraries, draft))
}

#[cfg(feature = "model-api")]
pub fn handle_open_definition_draft(
    world: &mut World,
    request: Value,
) -> ApiResult<DefinitionDraftEntry> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.draft.open expects a JSON object".to_string())?;
    let definition_id = object
        .get("definition_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'definition_id'".to_string())?;
    let library_id = object.get("library_id").and_then(Value::as_str);

    let (definition, source_definition_id, source_library_id, _) = {
        let definitions =
            world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
        let libraries =
            world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();
        crate::plugins::definition_authoring::resolve_definition_for_authoring(
            definitions,
            libraries,
            definition_id,
            library_id,
        )?
    };

    let draft_id = {
        let mut drafts =
            world.resource_mut::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
        drafts.insert(crate::plugins::definition_authoring::DefinitionDraft {
            draft_id: crate::plugins::definition_authoring::DefinitionDraftId::new(),
            source_definition_id,
            source_library_id,
            working_copy: definition,
            dirty: false,
        })
    };

    handle_get_definition_draft(world, draft_id.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_create_definition_draft(
    world: &mut World,
    request: Value,
) -> ApiResult<DefinitionDraftEntry> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.draft.create expects a JSON object".to_string())?;
    let definition = build_definition_from_object(object)?;

    {
        let registry = world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
        let mut preview = registry.clone();
        preview.insert(definition.clone());
        let _ = preview.effective_definition(&definition.id);
    }

    let draft_id = {
        let mut drafts =
            world.resource_mut::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
        drafts.insert(crate::plugins::definition_authoring::DefinitionDraft {
            draft_id: crate::plugins::definition_authoring::DefinitionDraftId::new(),
            source_definition_id: None,
            source_library_id: None,
            working_copy: definition,
            dirty: true,
        })
    };

    handle_get_definition_draft(world, draft_id.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_derive_definition_draft(
    world: &mut World,
    request: Value,
) -> ApiResult<DefinitionDraftEntry> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.draft.derive expects a JSON object".to_string())?;
    let definition_id = object
        .get("definition_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'definition_id'".to_string())?;
    let library_id = object.get("library_id").and_then(Value::as_str);

    let (base_definition, _, source_library_id, effective_base) = {
        let definitions =
            world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
        let libraries =
            world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();
        crate::plugins::definition_authoring::resolve_definition_for_authoring(
            definitions,
            libraries,
            definition_id,
            library_id,
        )?
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} Variant", base_definition.name));
    let definition = crate::plugins::definition_authoring::derive_definition_from_base(
        &base_definition,
        &effective_base,
        name,
    );

    let draft_id = {
        let mut drafts =
            world.resource_mut::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
        drafts.insert(crate::plugins::definition_authoring::DefinitionDraft {
            draft_id: crate::plugins::definition_authoring::DefinitionDraftId::new(),
            source_definition_id: None,
            source_library_id,
            working_copy: definition,
            dirty: true,
        })
    };

    handle_get_definition_draft(world, draft_id.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_patch_definition_draft(
    world: &mut World,
    request: Value,
) -> ApiResult<DefinitionDraftEntry> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.draft.patch expects a JSON object".to_string())?;
    let draft_id = crate::plugins::definition_authoring::DefinitionDraftId(
        object
            .get("draft_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing 'draft_id'".to_string())?
            .to_string(),
    );

    let patches = if let Some(patches_value) = object.get("patches") {
        serde_json::from_value::<Vec<crate::plugins::definition_authoring::DefinitionPatch>>(
            patches_value.clone(),
        )
        .map_err(|error| error.to_string())?
    } else if let Some(patch_value) = object.get("patch") {
        vec![
            serde_json::from_value::<crate::plugins::definition_authoring::DefinitionPatch>(
                patch_value.clone(),
            )
            .map_err(|error| error.to_string())?,
        ]
    } else {
        return Err("Provide either 'patch' or 'patches'".to_string());
    };

    let definitions = world
        .resource::<crate::plugins::modeling::definition::DefinitionRegistry>()
        .clone();
    let libraries = world
        .resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>()
        .clone();
    {
        let mut drafts =
            world.resource_mut::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
        for patch in patches {
            crate::plugins::definition_authoring::apply_patch_to_draft(
                &definitions,
                &libraries,
                &mut drafts,
                &draft_id,
                patch,
            )?;
        }
    }

    handle_get_definition_draft(world, draft_id.to_string())
}

#[cfg(feature = "model-api")]
pub fn handle_publish_definition_draft(
    world: &mut World,
    draft_id: String,
) -> ApiResult<DefinitionEntry> {
    let draft_id = crate::plugins::definition_authoring::DefinitionDraftId(draft_id);
    let definition = crate::plugins::definition_authoring::publish_draft(world, &draft_id)?;
    let registry = world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
    let effective = registry.effective_definition(&definition.id)?;
    Ok(definition_to_entry(&definition, &effective))
}

#[cfg(feature = "model-api")]
pub fn handle_validate_definition(
    world: &World,
    request: Value,
) -> ApiResult<DefinitionValidationResult> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.validate expects a JSON object".to_string())?;
    if let Some(draft_id) = object.get("draft_id").and_then(Value::as_str) {
        let definitions =
            world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
        let libraries =
            world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();
        let drafts =
            world.resource::<crate::plugins::definition_authoring::DefinitionDraftRegistry>();
        let draft = drafts
            .get(&crate::plugins::definition_authoring::DefinitionDraftId(
                draft_id.to_string(),
            ))
            .ok_or_else(|| format!("Definition draft '{}' not found", draft_id))?;
        match crate::plugins::definition_authoring::validate_draft(definitions, libraries, draft) {
            Ok(effective) => Ok(DefinitionValidationResult {
                ok: true,
                errors: Vec::new(),
                effective_full: Some(serde_json::to_value(effective).unwrap_or(Value::Null)),
            }),
            Err(error) => Ok(DefinitionValidationResult {
                ok: false,
                errors: vec![error],
                effective_full: None,
            }),
        }
    } else {
        let definition_id = object
            .get("definition_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Provide either 'draft_id' or 'definition_id'".to_string())?;
        let library_id = object.get("library_id").and_then(Value::as_str);
        let definitions =
            world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
        let libraries =
            world.resource::<crate::plugins::modeling::definition::DefinitionLibraryRegistry>();
        let (definition, _, _, _) =
            crate::plugins::definition_authoring::resolve_definition_for_authoring(
                definitions,
                libraries,
                definition_id,
                library_id,
            )?;
        let mut preview = definitions.clone();
        if let Some(library_id) = library_id {
            let library = libraries
                .get(&crate::plugins::modeling::definition::DefinitionLibraryId(
                    library_id.to_string(),
                ))
                .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
            for library_definition in library.definitions.values() {
                preview.insert(library_definition.clone());
            }
        }
        match preview
            .validate_definition(&definition)
            .and_then(|_| preview.effective_definition(&definition.id))
        {
            Ok(effective) => Ok(DefinitionValidationResult {
                ok: true,
                errors: Vec::new(),
                effective_full: Some(serde_json::to_value(effective).unwrap_or(Value::Null)),
            }),
            Err(error) => Ok(DefinitionValidationResult {
                ok: false,
                errors: vec![error],
                effective_full: None,
            }),
        }
    }
}

#[cfg(feature = "model-api")]
pub fn handle_compile_definition(
    world: &World,
    request: Value,
) -> ApiResult<DefinitionCompileResult> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.compile expects a JSON object".to_string())?;
    let (preview, definition) = resolve_definition_analysis_target(world, object)?;
    let effective = preview.effective_definition(&definition.id)?;
    let summary =
        crate::plugins::definition_authoring::compile_definition_summary(&preview, &definition)?;
    Ok(compile_summary_to_result(
        serde_json::to_value(effective).unwrap_or(Value::Null),
        summary,
    ))
}

#[cfg(feature = "model-api")]
pub fn handle_explain_definition(
    world: &World,
    request: Value,
) -> ApiResult<DefinitionExplainResult> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.explain expects a JSON object".to_string())?;
    let (preview, definition) = resolve_definition_analysis_target(world, object)?;
    let explained =
        crate::plugins::definition_authoring::explain_definition(&preview, &definition)?;
    definition_explain_value_to_result(explained)
}

#[cfg(feature = "model-api")]
pub fn handle_create_definition_library(
    world: &mut World,
    request: Value,
) -> ApiResult<DefinitionLibraryEntry> {
    use crate::plugins::modeling::definition::{DefinitionLibraryRegistry, DefinitionLibraryScope};

    let object = request
        .as_object()
        .ok_or_else(|| "definition.library.create expects a JSON object".to_string())?;
    let name = object
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Missing 'name'".to_string())?;
    let scope = match object
        .get("scope")
        .and_then(|value| value.as_str())
        .unwrap_or("DocumentLocal")
    {
        "DocumentLocal" => DefinitionLibraryScope::DocumentLocal,
        "ExternalFile" => DefinitionLibraryScope::ExternalFile,
        other => return Err(format!("Unsupported library scope '{other}'")),
    };
    let source_path = object
        .get("source_path")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let library_id = world
        .resource_mut::<DefinitionLibraryRegistry>()
        .create_library(name.to_string(), scope, source_path);
    let library = world
        .resource::<DefinitionLibraryRegistry>()
        .get(&library_id)
        .cloned()
        .ok_or_else(|| {
            format!(
                "Definition library '{}' not found after creation",
                library_id
            )
        })?;

    Ok(definition_library_to_entry(&library))
}

#[cfg(feature = "model-api")]
pub fn handle_add_definition_to_library(
    world: &mut World,
    request: Value,
) -> ApiResult<DefinitionLibraryEntry> {
    use crate::plugins::modeling::definition::{
        DefinitionId, DefinitionLibraryId, DefinitionLibraryRegistry, DefinitionRegistry,
    };

    let object = request
        .as_object()
        .ok_or_else(|| "definition.library.add_definition expects a JSON object".to_string())?;
    let library_id = DefinitionLibraryId(
        object
            .get("library_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing 'library_id'".to_string())?
            .to_string(),
    );
    let definition_id = DefinitionId(
        object
            .get("definition_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing 'definition_id'".to_string())?
            .to_string(),
    );

    let definitions_to_add = {
        let registry = world.resource::<DefinitionRegistry>();
        let root_definition = registry
            .get(&definition_id)
            .cloned()
            .ok_or_else(|| format!("Definition '{}' not found", definition_id))?;
        let mut definitions = Vec::new();
        let mut stack = vec![root_definition];
        let mut seen = std::collections::HashSet::new();
        while let Some(definition) = stack.pop() {
            if !seen.insert(definition.id.clone()) {
                continue;
            }
            if let Some(base_definition_id) = &definition.base_definition_id {
                if let Some(base_definition) = registry.get(base_definition_id).cloned() {
                    stack.push(base_definition);
                }
            }
            if let Some(compound) = &definition.compound {
                for slot in &compound.child_slots {
                    if let Some(child_definition) = registry.get(&slot.definition_id).cloned() {
                        stack.push(child_definition);
                    }
                }
            }
            definitions.push(definition);
        }
        definitions
    };

    let mut libraries = world.resource_mut::<DefinitionLibraryRegistry>();
    for definition in definitions_to_add {
        libraries.add_definition(&library_id, definition)?;
    }

    let library = world
        .resource::<DefinitionLibraryRegistry>()
        .get(&library_id)
        .cloned()
        .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;

    Ok(definition_library_to_entry(&library))
}

#[cfg(feature = "model-api")]
pub fn handle_import_definition_library(
    world: &mut World,
    path: &str,
) -> ApiResult<DefinitionLibraryEntry> {
    use crate::plugins::modeling::definition::{
        DefinitionLibraryFile, DefinitionLibraryRegistry, DefinitionLibraryScope,
    };

    let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut file: DefinitionLibraryFile =
        serde_json::from_str(&contents).map_err(|error| error.to_string())?;
    if file.version != DefinitionLibraryFile::VERSION {
        return Err(format!(
            "Unsupported definition library version {} (expected {})",
            file.version,
            DefinitionLibraryFile::VERSION
        ));
    }
    file.library.scope = DefinitionLibraryScope::ExternalFile;
    file.library.source_path = Some(path.to_string());

    world
        .resource_mut::<DefinitionLibraryRegistry>()
        .insert(file.library.clone());

    Ok(definition_library_to_entry(&file.library))
}

#[cfg(feature = "model-api")]
pub fn handle_export_definition_library(
    world: &World,
    library_id: &str,
    path: &str,
) -> ApiResult<String> {
    use crate::plugins::modeling::definition::{
        DefinitionLibraryFile, DefinitionLibraryId, DefinitionLibraryRegistry,
    };

    let id = DefinitionLibraryId(library_id.to_string());
    let library = world
        .resource::<DefinitionLibraryRegistry>()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;

    let file = DefinitionLibraryFile {
        version: DefinitionLibraryFile::VERSION,
        library,
    };
    let json = serde_json::to_string_pretty(&file).map_err(|error| error.to_string())?;
    std::fs::write(path, json).map_err(|error| error.to_string())?;
    Ok(path.to_string())
}

#[cfg(feature = "model-api")]
fn ensure_definition_available_for_request(
    world: &mut World,
    object: &serde_json::Map<String, Value>,
) -> ApiResult<(String, Vec<String>)> {
    use crate::plugins::commands::enqueue_create_definition;
    use crate::plugins::modeling::definition::{
        DefinitionId, DefinitionLibraryId, DefinitionLibraryRegistry, DefinitionRegistry,
    };

    let definition_id = object
        .get("definition_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Missing 'definition_id'".to_string())?
        .to_string();

    let mut imported_definition_ids = Vec::new();
    let needs_import = {
        let registry = world.resource::<DefinitionRegistry>();
        registry.get(&DefinitionId(definition_id.clone())).is_none()
    };

    if needs_import {
        let library_id = object
            .get("library_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                format!(
                    "Definition '{}' is not present in the document; provide 'library_id' to import it first",
                    definition_id
                )
            })?;
        let library_id = DefinitionLibraryId(library_id.to_string());
        let library = world
            .resource::<DefinitionLibraryRegistry>()
            .get(&library_id)
            .cloned()
            .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
        let root_definition = library
            .get(&DefinitionId(definition_id.clone()))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Definition '{}' not found in library '{}'",
                    definition_id, library_id
                )
            })?;

        let mut to_import = vec![root_definition];
        let mut seen = std::collections::HashSet::new();
        while let Some(definition) = to_import.pop() {
            if !seen.insert(definition.id.clone()) {
                continue;
            }
            if let Some(base_definition_id) = &definition.base_definition_id {
                if let Some(base_definition) = library.get(base_definition_id).cloned() {
                    to_import.push(base_definition);
                }
            }
            if let Some(compound) = &definition.compound {
                for slot in &compound.child_slots {
                    if let Some(child) = library.get(&slot.definition_id).cloned() {
                        to_import.push(child);
                    }
                }
            }

            let already_present = {
                let registry = world.resource::<DefinitionRegistry>();
                registry.get(&definition.id).is_some()
            };
            if !already_present {
                imported_definition_ids.push(definition.id.to_string());
                enqueue_create_definition(world, definition);
            }
        }
        flush_model_api_write_pipeline(world);
    }

    Ok((definition_id, imported_definition_ids))
}

#[cfg(feature = "model-api")]
pub fn handle_instantiate_definition(
    world: &mut World,
    request: Value,
) -> ApiResult<InstantiateDefinitionResult> {
    let object = request
        .as_object()
        .ok_or_else(|| "definition.instantiate expects a JSON object".to_string())?;
    let (definition_id, imported_definition_ids) =
        ensure_definition_available_for_request(world, object)?;

    let element_id = handle_place_occurrence(world, request)?;
    Ok(InstantiateDefinitionResult {
        element_id,
        definition_id,
        imported_definition_ids,
        relation_ids: Vec::new(),
    })
}

#[cfg(feature = "model-api")]
fn value_vec3(value: &Value, context: &str) -> Result<Vec3, String> {
    let [x, y, z] = serde_json::from_value::<[f32; 3]>(value.clone())
        .map_err(|_| format!("{context} must be a [x, y, z] array"))?;
    Ok(Vec3::new(x, y, z))
}

#[cfg(feature = "model-api")]
fn vec3_value(vector: Vec3) -> Value {
    serde_json::json!([vector.x, vector.y, vector.z])
}

#[cfg(feature = "model-api")]
fn quat_value(rotation: Quat) -> Value {
    serde_json::json!([rotation.x, rotation.y, rotation.z, rotation.w])
}

#[cfg(feature = "model-api")]
fn infer_wall_thickness_from_snapshot(snapshot: &BoxedEntity) -> Option<f32> {
    let bounds = snapshot.bounds()?;
    let extents = bounds.max - bounds.min;
    Some(extents.x.min(extents.y).min(extents.z))
}

#[cfg(feature = "model-api")]
fn extract_wall_axis_from_snapshot(snapshot: &BoxedEntity) -> Option<Vec3> {
    if snapshot.type_name() != "wall" {
        return None;
    }

    let json = snapshot.to_json();
    let wall = json.get("Wall")?;
    let start = value_vec3(
        &serde_json::json!([
            wall.get("wall")?.get("start")?.get(0)?.as_f64()? as f32,
            0.0,
            wall.get("wall")?.get("start")?.get(1)?.as_f64()? as f32
        ]),
        "wall.start",
    )
    .ok()?;
    let end = value_vec3(
        &serde_json::json!([
            wall.get("wall")?.get("end")?.get(0)?.as_f64()? as f32,
            0.0,
            wall.get("wall")?.get("end")?.get(1)?.as_f64()? as f32
        ]),
        "wall.end",
    )
    .ok()?;
    (end - start).try_normalize()
}

#[cfg(feature = "model-api")]
fn infer_face_anchors(
    snapshot: &BoxedEntity,
    opening_center: Vec3,
) -> Option<(HostedAnchor, HostedAnchor, f32)> {
    let thickness = infer_wall_thickness_from_snapshot(snapshot)?;
    let axis = extract_wall_axis_from_snapshot(snapshot).unwrap_or_else(|| {
        let bounds = snapshot
            .bounds()
            .unwrap_or(crate::authored_entity::EntityBounds {
                min: opening_center,
                max: opening_center,
            });
        let extents = bounds.max - bounds.min;
        if extents.x <= extents.y && extents.x <= extents.z {
            Vec3::X
        } else if extents.y <= extents.x && extents.y <= extents.z {
            Vec3::Y
        } else {
            Vec3::Z
        }
    });
    let normal = Vec3::new(-axis.z, 0.0, axis.x)
        .try_normalize()
        .unwrap_or(Vec3::Z);
    let half = thickness * 0.5;
    Some((
        HostedAnchor {
            id: "opening.exterior_face".to_string(),
            kind: Some("host_exterior_face".to_string()),
            position: (opening_center - normal * half).to_array(),
        },
        HostedAnchor {
            id: "opening.interior_face".to_string(),
            kind: Some("host_interior_face".to_string()),
            position: (opening_center + normal * half).to_array(),
        },
        thickness,
    ))
}

#[cfg(feature = "model-api")]
fn infer_position_along_wall(snapshot: &BoxedEntity, point: Vec3) -> Option<f64> {
    if snapshot.type_name() != "wall" {
        return None;
    }
    let json = snapshot.to_json();
    let wall = json.get("Wall")?.get("wall")?;
    let start = Vec2::new(
        wall.get("start")?.get(0)?.as_f64()? as f32,
        wall.get("start")?.get(1)?.as_f64()? as f32,
    );
    let end = Vec2::new(
        wall.get("end")?.get(0)?.as_f64()? as f32,
        wall.get("end")?.get(1)?.as_f64()? as f32,
    );
    let direction = (end - start).try_normalize()?;
    let length = start.distance(end);
    if length <= f32::EPSILON {
        return None;
    }
    Some(((point.xz() - start).dot(direction) / length).clamp(0.0, 1.0) as f64)
}

#[cfg(feature = "model-api")]
fn infer_wall_rotation(snapshot: &BoxedEntity) -> Option<Quat> {
    let axis = extract_wall_axis_from_snapshot(snapshot)?;
    let planar = Vec2::new(axis.x, axis.z).try_normalize()?;
    let angle = planar.y.atan2(planar.x);
    Some(Quat::from_rotation_y(-angle))
}

#[cfg(feature = "model-api")]
const WALL_OPENING_HOSTING_CONTRACT_KIND: &str = "architecture::wall_opening";
#[cfg(feature = "model-api")]
const HOSTED_OPENING_MIN_REMAINING_WALL: f32 = 0.05;
#[cfg(feature = "model-api")]
const HOSTED_OPENING_GEOMETRY_TOLERANCE: f32 = 0.01;
#[cfg(feature = "model-api")]
const HOSTED_OPENING_PLACEMENT_TOLERANCE: f32 = 0.02;

#[cfg(feature = "model-api")]
#[derive(Debug, Clone, Copy)]
struct HostedOpeningAxes {
    normal: SpatialAxis,
    in_plane: [SpatialAxis; 2],
}

#[cfg(feature = "model-api")]
fn axis_extent(bounds: crate::authored_entity::EntityBounds, axis: SpatialAxis) -> f32 {
    axis.bounds_max(bounds) - axis.bounds_min(bounds)
}

#[cfg(feature = "model-api")]
fn shortest_positive_axis(bounds: crate::authored_entity::EntityBounds) -> Option<SpatialAxis> {
    [SpatialAxis::X, SpatialAxis::Y, SpatialAxis::Z]
        .into_iter()
        .filter(|axis| axis_extent(bounds, *axis) > f32::EPSILON)
        .min_by(|a, b| axis_extent(bounds, *a).total_cmp(&axis_extent(bounds, *b)))
}

#[cfg(feature = "model-api")]
fn hosted_opening_axes(
    host_bounds: crate::authored_entity::EntityBounds,
) -> Option<HostedOpeningAxes> {
    let normal = shortest_positive_axis(host_bounds)?;
    let in_plane = match normal {
        SpatialAxis::X => [SpatialAxis::Y, SpatialAxis::Z],
        SpatialAxis::Y => [SpatialAxis::X, SpatialAxis::Z],
        SpatialAxis::Z => [SpatialAxis::X, SpatialAxis::Y],
    };
    Some(HostedOpeningAxes { normal, in_plane })
}

#[cfg(feature = "model-api")]
fn axis_name(axis: SpatialAxis) -> &'static str {
    match axis {
        SpatialAxis::X => "x",
        SpatialAxis::Y => "y",
        SpatialAxis::Z => "z",
    }
}

#[cfg(feature = "model-api")]
fn validation_measured(value: impl Into<Value>, unit: &str) -> MeasuredValue {
    MeasuredValue {
        value: value.into(),
        unit: unit.to_string(),
    }
}

#[cfg(feature = "model-api")]
fn hosted_opening_check(
    id: &str,
    label: &str,
    status: HostingCheckStatus,
    message: impl Into<String>,
    measured_value: Option<MeasuredValue>,
    expected_value: Option<MeasuredValue>,
    affected_region: Option<&str>,
) -> HostingValidationCheck {
    use crate::plugins::modeling::definition::ConstraintSeverity;

    HostingValidationCheck {
        id: HostingCheckId(id.to_string()),
        label: label.to_string(),
        severity: if matches!(status, HostingCheckStatus::Warning) {
            ConstraintSeverity::Warning
        } else {
            ConstraintSeverity::Error
        },
        status,
        message: message.into(),
        measured_value,
        expected_value,
        affected_region: affected_region.map(|region| HostAffectedRegion(region.to_string())),
    }
}

#[cfg(feature = "model-api")]
fn status_from_hosting_checks(checks: &[HostingValidationCheck]) -> HostingValidationStatus {
    if checks
        .iter()
        .any(|check| matches!(check.status, HostingCheckStatus::Failed))
    {
        HostingValidationStatus::Blocked
    } else if checks
        .iter()
        .any(|check| matches!(check.status, HostingCheckStatus::Warning))
    {
        HostingValidationStatus::Warning
    } else {
        HostingValidationStatus::Passed
    }
}

#[cfg(feature = "model-api")]
fn opening_id_from_contract_parameters(parameters: &Value) -> Option<ElementId> {
    parameters
        .get("opening_element_id")
        .and_then(Value::as_u64)
        .map(ElementId)
}

#[cfg(feature = "model-api")]
fn opening_id_from_occurrence(world: &World, occurrence_id: ElementId) -> Option<ElementId> {
    use crate::plugins::modeling::occurrence::OccurrenceIdentity;

    let mut query = world.try_query::<EntityRef>()?;
    query
        .iter(world)
        .find(|entity_ref| entity_ref.get::<ElementId>().copied() == Some(occurrence_id))
        .and_then(|entity_ref| entity_ref.get::<OccurrenceIdentity>())
        .and_then(|identity| identity.hosting.as_ref())
        .and_then(|hosting| hosting.opening_element_id)
}

#[cfg(feature = "model-api")]
fn hosted_occurrence_placement_center(world: &World, occurrence_id: ElementId) -> Option<Vec3> {
    use crate::plugins::modeling::occurrence::OccurrenceIdentity;

    let mut query = world.try_query::<EntityRef>()?;
    let entity_ref = query
        .iter(world)
        .find(|entity_ref| entity_ref.get::<ElementId>().copied() == Some(occurrence_id))?;
    entity_ref
        .get::<Transform>()
        .map(|transform| transform.translation)
        .or_else(|| {
            entity_ref
                .get::<OccurrenceIdentity>()
                .and_then(|identity| identity.hosting.as_ref())
                .and_then(|hosting| hosting.anchor_position("opening.center"))
        })
}

#[cfg(feature = "model-api")]
fn wall_opening_validation_result(
    contract_kind: HostingContractKindId,
    host_element_id: ElementId,
    hosted_element_id: ElementId,
    host_snapshot: Option<BoxedEntity>,
    opening_snapshot: Option<BoxedEntity>,
    hosted_center: Option<Vec3>,
) -> HostingValidationResult {
    let mut checks = Vec::new();

    let Some(host_snapshot) = host_snapshot else {
        checks.push(hosted_opening_check(
            "host.exists",
            "Host Exists",
            HostingCheckStatus::Failed,
            format!("Host entity {} does not exist.", host_element_id.0),
            None,
            None,
            Some("host"),
        ));
        return HostingValidationResult {
            contract_kind,
            host_element_id,
            hosted_element_id,
            status: status_from_hosting_checks(&checks),
            checks,
        };
    };
    let Some(opening_snapshot) = opening_snapshot else {
        checks.push(hosted_opening_check(
            "opening.exists",
            "Opening Exists",
            HostingCheckStatus::Failed,
            "Hosted wall openings require an explicit opening entity.",
            None,
            None,
            Some("opening"),
        ));
        return HostingValidationResult {
            contract_kind,
            host_element_id,
            hosted_element_id,
            status: status_from_hosting_checks(&checks),
            checks,
        };
    };

    let host_bounds = alignment_bounds(&host_snapshot);
    let opening_bounds = alignment_bounds(&opening_snapshot);
    let Some(axes) = hosted_opening_axes(host_bounds) else {
        checks.push(hosted_opening_check(
            "host.thickness_axis",
            "Host Thickness Axis",
            HostingCheckStatus::Failed,
            "Host wall must have a measurable thickness axis.",
            None,
            None,
            Some("host"),
        ));
        return HostingValidationResult {
            contract_kind,
            host_element_id,
            hosted_element_id,
            status: status_from_hosting_checks(&checks),
            checks,
        };
    };

    let opening_center = opening_bounds.center();
    let host_thickness = axis_extent(host_bounds, axes.normal);
    let opening_thickness = axis_extent(opening_bounds, axes.normal);

    let axis_alignment_status =
        if (opening_thickness - host_thickness).abs() <= HOSTED_OPENING_GEOMETRY_TOLERANCE {
            HostingCheckStatus::Passed
        } else {
            HostingCheckStatus::Failed
        };
    checks.push(hosted_opening_check(
        "opening.normal_alignment",
        "Opening Normal Alignment",
        axis_alignment_status,
        if axis_alignment_status == HostingCheckStatus::Passed {
            format!(
                "Opening cuts through the host thickness axis ({}).",
                axis_name(axes.normal)
            )
        } else {
            format!(
                "Opening must span the host wall thickness axis ({}) without rotating out of plane.",
                axis_name(axes.normal)
            )
        },
        Some(validation_measured(opening_thickness as f64, "model_units")),
        Some(validation_measured(host_thickness as f64, "model_units")),
        Some("opening"),
    ));

    let normal_min_delta = opening_bounds.min_axis(axes.normal) - host_bounds.min_axis(axes.normal);
    let normal_max_delta = host_bounds.max_axis(axes.normal) - opening_bounds.max_axis(axes.normal);
    let thickness_status = if normal_min_delta >= -HOSTED_OPENING_GEOMETRY_TOLERANCE
        && normal_max_delta >= -HOSTED_OPENING_GEOMETRY_TOLERANCE
    {
        HostingCheckStatus::Passed
    } else {
        HostingCheckStatus::Failed
    };
    checks.push(hosted_opening_check(
        "opening.thickness_containment",
        "Opening Thickness Containment",
        thickness_status,
        if thickness_status == HostingCheckStatus::Passed {
            "Opening depth is contained by the host wall thickness.".to_string()
        } else {
            "Opening cannot extend outside the host wall thickness.".to_string()
        },
        Some(validation_measured(opening_thickness as f64, "model_units")),
        Some(validation_measured(host_thickness as f64, "model_units")),
        Some("opening"),
    ));

    for axis in axes.in_plane {
        let remaining_min = opening_bounds.min_axis(axis) - host_bounds.min_axis(axis);
        let remaining_max = host_bounds.max_axis(axis) - opening_bounds.max_axis(axis);
        let remaining = remaining_min.min(remaining_max);
        let status =
            if remaining + HOSTED_OPENING_GEOMETRY_TOLERANCE >= HOSTED_OPENING_MIN_REMAINING_WALL {
                HostingCheckStatus::Passed
            } else {
                HostingCheckStatus::Failed
            };
        checks.push(hosted_opening_check(
            &format!("opening.remaining_wall.{}", axis_name(axis)),
            &format!("Remaining Wall {}", axis_name(axis).to_uppercase()),
            status,
            if status == HostingCheckStatus::Passed {
                format!(
                    "Opening leaves wall material on both sides along {}.",
                    axis_name(axis)
                )
            } else {
                format!(
                    "Opening must leave at least {:.2} model units of wall on both sides along {}.",
                    HOSTED_OPENING_MIN_REMAINING_WALL,
                    axis_name(axis)
                )
            },
            Some(validation_measured(remaining as f64, "model_units")),
            Some(validation_measured(
                HOSTED_OPENING_MIN_REMAINING_WALL as f64,
                "model_units",
            )),
            Some("host"),
        ));
    }

    if let Some(hosted_center) = hosted_center {
        let mut worst_in_plane_delta = 0.0f32;
        for axis in axes.in_plane {
            worst_in_plane_delta = worst_in_plane_delta
                .max((axis.component(hosted_center) - axis.component(opening_center)).abs());
        }
        let out_of_plane_delta =
            (axes.normal.component(hosted_center) - axes.normal.component(opening_center)).abs();
        let in_plane_status = if worst_in_plane_delta <= HOSTED_OPENING_PLACEMENT_TOLERANCE {
            HostingCheckStatus::Passed
        } else {
            HostingCheckStatus::Failed
        };
        checks.push(hosted_opening_check(
            "hosted.center_in_opening",
            "Hosted Center In Opening",
            in_plane_status,
            if in_plane_status == HostingCheckStatus::Passed {
                "Hosted occurrence is centered in the opening plane.".to_string()
            } else {
                "Hosted occurrence center must stay in the authored opening.".to_string()
            },
            Some(validation_measured(
                worst_in_plane_delta as f64,
                "model_units",
            )),
            Some(validation_measured(
                HOSTED_OPENING_PLACEMENT_TOLERANCE as f64,
                "model_units",
            )),
            Some("opening"),
        ));

        let allowed_normal_offset = host_thickness * 0.5 + HOSTED_OPENING_PLACEMENT_TOLERANCE;
        let normal_status = if out_of_plane_delta <= allowed_normal_offset {
            HostingCheckStatus::Passed
        } else {
            HostingCheckStatus::Failed
        };
        checks.push(hosted_opening_check(
            "hosted.center_on_wall_depth",
            "Hosted Center On Wall Depth",
            normal_status,
            if normal_status == HostingCheckStatus::Passed {
                "Hosted occurrence remains within the wall depth.".to_string()
            } else {
                "Hosted occurrence cannot be displaced clear of the wall depth.".to_string()
            },
            Some(validation_measured(
                out_of_plane_delta as f64,
                "model_units",
            )),
            Some(validation_measured(
                allowed_normal_offset as f64,
                "model_units",
            )),
            Some("opening"),
        ));
    }

    HostingValidationResult {
        contract_kind,
        host_element_id,
        hosted_element_id,
        status: status_from_hosting_checks(&checks),
        checks,
    }
}

#[cfg(feature = "model-api")]
trait EntityBoundsAxisExt {
    fn min_axis(&self, axis: SpatialAxis) -> f32;
    fn max_axis(&self, axis: SpatialAxis) -> f32;
}

#[cfg(feature = "model-api")]
impl EntityBoundsAxisExt for crate::authored_entity::EntityBounds {
    fn min_axis(&self, axis: SpatialAxis) -> f32 {
        axis.component(self.min)
    }

    fn max_axis(&self, axis: SpatialAxis) -> f32 {
        axis.component(self.max)
    }
}

#[cfg(feature = "model-api")]
fn validate_preflight_hosted_wall_opening(
    world: &World,
    hosted_request: &Value,
    hosted_context: &crate::plugins::modeling::occurrence::HostedOccurrenceContext,
) -> ApiResult<()> {
    let Some(host_id) = hosted_context.host_element_id else {
        return Ok(());
    };
    let Some(opening_id) = hosted_context.opening_element_id else {
        return Ok(());
    };

    let hosted_center = hosted_request
        .get("offset")
        .and_then(|value| serde_json::from_value::<[f32; 3]>(value.clone()).ok())
        .map(|[x, y, z]| Vec3::new(x, y, z));
    let result = wall_opening_validation_result(
        HostingContractKindId(WALL_OPENING_HOSTING_CONTRACT_KIND.to_string()),
        host_id,
        ElementId(0),
        capture_entity_snapshot(world, host_id),
        capture_entity_snapshot(world, opening_id),
        hosted_center,
    );

    if result.status == HostingValidationStatus::Blocked {
        let failed = result
            .checks
            .iter()
            .filter(|check| check.status == HostingCheckStatus::Failed)
            .map(|check| format!("{}: {}", check.label, check.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "Hosted wall opening failed building sanity checks: {failed}"
        ));
    }

    Ok(())
}

#[cfg(feature = "model-api")]
fn validate_relation_descriptor(
    world: &World,
    relation_type: &str,
    source_type: &str,
    target_snapshot: &BoxedEntity,
) -> ApiResult<()> {
    let descriptor = world
        .resource::<CapabilityRegistry>()
        .relation_type_descriptors()
        .iter()
        .find(|descriptor| descriptor.relation_type == relation_type)
        .ok_or_else(|| format!("Unknown relation type '{relation_type}'"))?;

    if !descriptor.valid_source_types.is_empty()
        && !descriptor
            .valid_source_types
            .iter()
            .any(|allowed| allowed == source_type)
    {
        return Err(format!(
            "Relation '{}' does not allow source type '{}'",
            relation_type, source_type
        ));
    }
    if !descriptor.valid_target_types.is_empty()
        && !descriptor
            .valid_target_types
            .iter()
            .any(|allowed| allowed == target_snapshot.type_name())
    {
        return Err(format!(
            "Relation '{}' does not allow target type '{}'",
            relation_type,
            target_snapshot.type_name()
        ));
    }

    Ok(())
}

#[cfg(feature = "model-api")]
fn build_relation_snapshot(
    world: &mut World,
    source: ElementId,
    target: ElementId,
    relation_type: String,
    parameters: Value,
) -> (
    ElementId,
    crate::plugins::modeling::assembly::RelationSnapshot,
) {
    use crate::plugins::modeling::assembly::{RelationSnapshot, SemanticRelation};

    let relation_id = world.resource_mut::<ElementIdAllocator>().next_id();
    (
        relation_id,
        RelationSnapshot {
            element_id: relation_id,
            relation: SemanticRelation {
                source,
                target,
                relation_type,
                parameters,
            },
        },
    )
}

#[cfg(feature = "model-api")]
fn prepare_hosted_occurrence_request(
    world: &World,
    object: &serde_json::Map<String, Value>,
) -> ApiResult<(Value, Option<String>, Option<ElementId>, Value)> {
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry, OverridePolicy};
    use crate::plugins::modeling::occurrence::{HostedAnchor, HostedOccurrenceContext};

    let hosting = object
        .get("hosting")
        .and_then(Value::as_object)
        .ok_or_else(|| "Hosted instantiation requires a 'hosting' object".to_string())?;

    let host_element_id = hosting
        .get("host_element_id")
        .and_then(Value::as_u64)
        .map(ElementId);
    let opening_element_id = hosting
        .get("opening_element_id")
        .and_then(Value::as_u64)
        .map(ElementId);

    let mut anchors_by_id: HashMap<String, HostedAnchor> = HashMap::new();
    if let Some(anchor_object) = hosting.get("anchors").and_then(Value::as_object) {
        for (id, position) in anchor_object {
            anchors_by_id.insert(
                id.clone(),
                HostedAnchor {
                    id: id.clone(),
                    kind: None,
                    position: serde_json::from_value::<[f32; 3]>(position.clone()).map_err(
                        |_| format!("hosting.anchors['{id}'] must be a [x, y, z] array"),
                    )?,
                },
            );
        }
    }

    if let Some(opening_id) = opening_element_id {
        let opening_snapshot = capture_entity_snapshot(world, opening_id)
            .ok_or_else(|| format!("Opening entity {} not found", opening_id.0))?;
        anchors_by_id
            .entry("opening.center".to_string())
            .or_insert_with(|| HostedAnchor {
                id: "opening.center".to_string(),
                kind: Some("opening_center".to_string()),
                position: opening_snapshot.center().to_array(),
            });
    }

    let mut inferred_wall_thickness = hosting
        .get("wall_thickness")
        .and_then(Value::as_f64)
        .map(|value| value as f32);

    if let Some(host_id) = host_element_id {
        let host_snapshot = capture_entity_snapshot(world, host_id)
            .ok_or_else(|| format!("Host entity {} not found", host_id.0))?;
        let opening_center = anchors_by_id
            .get("opening.center")
            .map(HostedAnchor::vec3)
            .unwrap_or_else(|| host_snapshot.center());
        if let Some((exterior, interior, thickness)) =
            infer_face_anchors(&host_snapshot, opening_center)
        {
            anchors_by_id.entry(exterior.id.clone()).or_insert(exterior);
            anchors_by_id.entry(interior.id.clone()).or_insert(interior);
            if inferred_wall_thickness.is_none() {
                inferred_wall_thickness = Some(thickness);
            }
        }
    }

    let local_offset = object
        .get("offset")
        .map(|value| value_vec3(value, "offset"))
        .transpose()?
        .unwrap_or(Vec3::ZERO);
    let placement_origin = anchors_by_id
        .get("opening.center")
        .map(HostedAnchor::vec3)
        .or_else(|| {
            host_element_id
                .and_then(|host_id| capture_entity_snapshot(world, host_id))
                .map(|snapshot| snapshot.center())
        })
        .ok_or_else(|| {
            "Hosted instantiation requires either hosting.anchors['opening.center'], opening_element_id, or host_element_id"
                .to_string()
        })?;

    let definition_id = object
        .get("definition_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'definition_id'".to_string())?;
    let definition = world
        .resource::<DefinitionRegistry>()
        .get(&DefinitionId(definition_id.to_string()))
        .ok_or_else(|| format!("Definition '{}' not found", definition_id))?;

    let mut overrides = object
        .get("overrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !overrides.contains_key("wall_thickness") {
        if let Some(parameter) = definition.interface.parameters.get("wall_thickness") {
            if parameter.override_policy != OverridePolicy::Locked {
                if let Some(thickness) = inferred_wall_thickness {
                    overrides.insert("wall_thickness".to_string(), Value::from(thickness as f64));
                }
            }
        }
    }

    let mut request_object = object.clone();
    if let Some(host_id) = host_element_id {
        if let Some(host_snapshot) = capture_entity_snapshot(world, host_id) {
            if let Some(rotation) = infer_wall_rotation(&host_snapshot) {
                request_object
                    .entry("rotation".to_string())
                    .or_insert_with(|| quat_value(rotation));
            }
        }
    }
    request_object.insert(
        "offset".to_string(),
        vec3_value(placement_origin + local_offset),
    );
    if !overrides.is_empty() {
        request_object.insert("overrides".to_string(), Value::Object(overrides));
    }

    let hosted_context = HostedOccurrenceContext {
        host_element_id,
        opening_element_id,
        anchors: anchors_by_id.into_values().collect(),
    };
    request_object.insert(
        "hosting".to_string(),
        serde_json::to_value(&hosted_context).map_err(|error| error.to_string())?,
    );

    let relation_type = hosting
        .get("relation_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| host_element_id.map(|_| "hosted_on".to_string()));
    let mut relation_parameters = hosting
        .get("relation_parameters")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if let (Some(host_id), Some(relation_type)) = (host_element_id, relation_type.as_ref()) {
        if relation_parameters.is_null() {
            relation_parameters = Value::Object(Default::default());
        }
        if let Some(object) = relation_parameters.as_object_mut() {
            if let Some(opening_id) = opening_element_id {
                object
                    .entry("opening_element_id".to_string())
                    .or_insert(Value::from(opening_id.0));
            }
            if let Some(host_snapshot) = capture_entity_snapshot(world, host_id) {
                if let Some(opening_center) = hosted_context.anchor_position("opening.center") {
                    if let Some(position_along_wall) =
                        infer_position_along_wall(&host_snapshot, opening_center)
                    {
                        object
                            .entry("position_along_wall".to_string())
                            .or_insert(Value::from(position_along_wall));
                    }
                }
                validate_relation_descriptor(world, relation_type, "occurrence", &host_snapshot)?;
            }
        }
    }

    Ok((
        Value::Object(request_object),
        relation_type,
        host_element_id,
        relation_parameters,
    ))
}

#[cfg(feature = "model-api")]
pub fn handle_instantiate_hosted_definition(
    world: &mut World,
    request: Value,
) -> ApiResult<InstantiateDefinitionResult> {
    use crate::plugins::commands::{enqueue_apply_void_placement, enqueue_create_boxed_entity};
    use crate::plugins::history::PendingCommandQueue;
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};
    use crate::plugins::modeling::occurrence::HostedOccurrenceContext;
    use crate::plugins::modeling::void_declaration::plan_void_placement;

    let object = request
        .as_object()
        .ok_or_else(|| "definition.instantiate_hosted expects a JSON object".to_string())?;
    let (definition_id, imported_definition_ids) =
        ensure_definition_available_for_request(world, object)?;
    let (hosted_request, relation_type, host_element_id, relation_parameters) =
        prepare_hosted_occurrence_request(world, object)?;

    let hosted_context: HostedOccurrenceContext = hosted_request
        .get("hosting")
        .cloned()
        .ok_or_else(|| "Hosted request missing resolved hosting context".to_string())
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|error| format!("Invalid hosting context: {error}"))
        })?;
    validate_preflight_hosted_wall_opening(world, &hosted_request, &hosted_context)?;
    let (element_id, occurrence_snapshot) =
        build_occurrence_snapshot_for_place(world, hosted_request)?;

    let relation_snapshots =
        if let (Some(relation_type), Some(host_element_id)) = (relation_type, host_element_id) {
            vec![build_relation_snapshot(
                world,
                ElementId(element_id),
                host_element_id,
                relation_type,
                relation_parameters,
            )]
        } else {
            Vec::new()
        };
    let relation_ids = relation_snapshots
        .iter()
        .map(|(relation_id, _)| relation_id.0)
        .collect();

    let void_outcome = match (
        hosted_context.host_element_id,
        hosted_context.opening_element_id,
    ) {
        (Some(host), Some(opening)) => {
            let definition_key = DefinitionId(definition_id.clone());
            let definition = world
                .resource::<DefinitionRegistry>()
                .effective_definition(&definition_key)?;
            if definition.interface.void_declaration.is_some() {
                Some(
                    plan_void_placement(
                        definition.interface.void_declaration.as_ref(),
                        &definition_key,
                        host,
                        ElementId(element_id),
                        opening,
                    )
                    .map_err(|error| error.to_string())?,
                )
            } else {
                None
            }
        }
        _ => None,
    };

    world
        .resource_mut::<PendingCommandQueue>()
        .begin_group("Instantiate hosted definition");
    enqueue_create_boxed_entity(world, occurrence_snapshot);
    for (_, relation_snapshot) in relation_snapshots {
        enqueue_create_boxed_entity(world, relation_snapshot.into());
    }
    if let Some(outcome) = void_outcome {
        enqueue_apply_void_placement(world, outcome);
    }
    world.resource_mut::<PendingCommandQueue>().end_group();
    flush_model_api_write_pipeline(world);

    Ok(InstantiateDefinitionResult {
        element_id,
        definition_id,
        imported_definition_ids,
        relation_ids,
    })
}

#[cfg(feature = "model-api")]
pub fn handle_place_occurrence(world: &mut World, request: Value) -> ApiResult<u64> {
    use crate::plugins::commands::enqueue_create_boxed_entity;

    let (result_id, snapshot) = build_occurrence_snapshot_for_place(world, request)?;
    enqueue_create_boxed_entity(world, snapshot);
    flush_model_api_write_pipeline(world);
    Ok(result_id)
}

#[cfg(feature = "model-api")]
fn build_occurrence_snapshot_for_place(
    world: &mut World,
    request: Value,
) -> ApiResult<(u64, BoxedEntity)> {
    use crate::plugins::identity::ElementIdAllocator;
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};
    use crate::plugins::modeling::occurrence::{
        HostedOccurrenceContext, OccurrenceIdentity, OccurrenceSnapshot,
    };

    let obj = request
        .as_object()
        .ok_or_else(|| "occurrence.place expects a JSON object".to_string())?;

    let def_id_str = obj
        .get("definition_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'definition_id'".to_string())?;
    let def_id = DefinitionId(def_id_str.to_string());

    let def_version = world
        .resource::<DefinitionRegistry>()
        .get(&def_id)
        .ok_or_else(|| format!("Definition '{def_id_str}' not found"))?
        .definition_version;

    let mut identity = OccurrenceIdentity::new(def_id, def_version);

    if let Some(overrides_val) = obj.get("overrides") {
        if let Some(map) = overrides_val.as_object() {
            for (k, v) in map {
                identity.overrides.set(k.clone(), v.clone());
            }
        }
    }
    if obj.contains_key("domain_data") {
        identity.domain_data = obj.get("domain_data").cloned().unwrap_or(Value::Null);
    }
    if let Some(hosting) = obj.get("hosting") {
        identity.hosting = Some(
            serde_json::from_value::<HostedOccurrenceContext>(hosting.clone())
                .map_err(|error| format!("Invalid hosting context: {error}"))?,
        );
    }
    {
        let registry = world.resource::<DefinitionRegistry>();
        registry.validate_overrides(&identity.definition_id, &identity.overrides)?;
    }

    let label = obj
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("Occurrence")
        .to_string();

    let offset = obj
        .get("offset")
        .and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok())
        .map(|[x, y, z]| bevy::prelude::Vec3::new(x, y, z))
        .unwrap_or(bevy::prelude::Vec3::ZERO);
    let rotation = obj
        .get("rotation")
        .and_then(|v| serde_json::from_value::<[f32; 4]>(v.clone()).ok())
        .map(|[x, y, z, w]| Quat::from_xyzw(x, y, z, w))
        .unwrap_or(Quat::IDENTITY);

    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    let mut snapshot = OccurrenceSnapshot::new(element_id, identity, label);
    snapshot.offset = offset;
    snapshot.rotation = rotation;

    let result_id = element_id.0;
    Ok((result_id, snapshot.into()))
}

#[cfg(feature = "model-api")]
pub fn handle_update_occurrence_overrides(
    world: &mut World,
    element_id: u64,
    overrides: Value,
) -> ApiResult<Value> {
    use crate::plugins::commands::enqueue_apply_entity_changes;
    use crate::plugins::modeling::occurrence::OccurrenceIdentity;

    let eid = ElementId(element_id);

    // Capture before snapshot
    let before = capture_entity_snapshot(world, eid)
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    // Verify it is an occurrence
    let mut q = world.try_query::<EntityRef>().unwrap();
    let has_identity = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .map(|e| e.get::<OccurrenceIdentity>().is_some())
        .unwrap_or(false);
    drop(q);

    if !has_identity {
        return Err(format!("Entity {element_id} is not an occurrence"));
    }

    // Apply overrides through the AuthoredEntity set_property_json pathway for each key.
    let mut after = before.clone();
    if let Some(map) = overrides.as_object() {
        for (k, v) in map {
            after = after
                .set_property_json(k, v)
                .map_err(|e| format!("Failed to set '{k}': {e}"))?;
        }
    }

    if let Some(snapshot) = after
        .0
        .as_any()
        .downcast_ref::<crate::plugins::modeling::occurrence::OccurrenceSnapshot>()
    {
        let registry = world.resource::<crate::plugins::modeling::definition::DefinitionRegistry>();
        registry.validate_overrides(
            &snapshot.identity.definition_id,
            &snapshot.identity.overrides,
        )?;
    }

    let after_json = after.to_json();
    let mut before_snapshots = vec![before];
    let mut after_snapshots = vec![after];
    let occurrence_after = after_snapshots
        .first()
        .ok_or_else(|| "Updated occurrence snapshot missing".to_string())?
        .clone();
    append_host_opening_sync_snapshots(
        world,
        eid,
        &occurrence_after,
        &mut before_snapshots,
        &mut after_snapshots,
    )?;

    enqueue_apply_entity_changes(
        world,
        ApplyEntityChangesCommand {
            label: "Update occurrence overrides",
            before: before_snapshots,
            after: after_snapshots,
        },
    );

    flush_model_api_write_pipeline(world);
    Ok(after_json)
}

#[cfg(feature = "model-api")]
fn occurrence_snapshot(
    snapshot: &BoxedEntity,
) -> Option<&crate::plugins::modeling::occurrence::OccurrenceSnapshot> {
    snapshot
        .0
        .as_any()
        .downcast_ref::<crate::plugins::modeling::occurrence::OccurrenceSnapshot>()
}

#[cfg(feature = "model-api")]
fn relation_dimension_axes(
    host_snapshot: Option<&BoxedEntity>,
    opening_snapshot: &BoxedEntity,
) -> Option<(SpatialAxis, SpatialAxis, SpatialAxis)> {
    let host_bounds = host_snapshot
        .and_then(BoxedEntity::bounds)
        .unwrap_or_else(|| alignment_bounds(opening_snapshot));
    let axes = hosted_opening_axes(host_bounds)?;
    let height_axis = if axes.in_plane.contains(&SpatialAxis::Y) {
        SpatialAxis::Y
    } else {
        axes.in_plane[1]
    };
    let width_axis = axes
        .in_plane
        .into_iter()
        .find(|axis| *axis != height_axis)
        .unwrap_or(axes.in_plane[0]);
    Some((width_axis, height_axis, axes.normal))
}

#[cfg(feature = "model-api")]
fn set_axis_value(values: &mut [f32; 3], axis: SpatialAxis, value: f32) {
    match axis {
        SpatialAxis::X => values[0] = value,
        SpatialAxis::Y => values[1] = value,
        SpatialAxis::Z => values[2] = value,
    }
}

#[cfg(feature = "model-api")]
fn ensure_json_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(Default::default());
    }
}

#[cfg(feature = "model-api")]
fn opening_size_from_occurrence(
    world: &World,
    identity: &crate::plugins::modeling::occurrence::OccurrenceIdentity,
) -> ApiResult<Option<(f32, f32)>> {
    use crate::plugins::modeling::definition::DefinitionRegistry;
    use crate::plugins::modeling::void_declaration::VoidShape;

    let registry = world.resource::<DefinitionRegistry>();
    let definition = registry.effective_definition(&identity.definition_id)?;
    let Some(void_declaration) = definition.interface.void_declaration.as_ref() else {
        return Ok(None);
    };
    let VoidShape::Rectangular {
        width_param,
        height_param,
    } = &void_declaration.shape
    else {
        return Ok(None);
    };
    let resolved = registry.resolve_params_checked(&identity.definition_id, &identity.overrides)?;
    let width = resolved
        .get(width_param)
        .and_then(|param| param.value.as_f64())
        .ok_or_else(|| {
            format!(
                "Void width parameter '{}' must resolve to a number",
                width_param
            )
        })? as f32;
    let height = resolved
        .get(height_param)
        .and_then(|param| param.value.as_f64())
        .ok_or_else(|| {
            format!(
                "Void height parameter '{}' must resolve to a number",
                height_param
            )
        })? as f32;
    if width <= 0.0 || height <= 0.0 {
        return Err("Hosted occurrence void dimensions must be greater than zero".to_string());
    }
    Ok(Some((width, height)))
}

#[cfg(feature = "model-api")]
fn append_host_opening_sync_snapshots(
    world: &World,
    occurrence_id: ElementId,
    occurrence_after: &BoxedEntity,
    before_snapshots: &mut Vec<BoxedEntity>,
    after_snapshots: &mut Vec<BoxedEntity>,
) -> ApiResult<()> {
    use crate::plugins::modeling::assembly::{RelationSnapshot, SemanticRelation};

    let Some(snapshot) = occurrence_snapshot(occurrence_after) else {
        return Ok(());
    };
    let Some(hosting) = snapshot.identity.hosting.as_ref() else {
        return Ok(());
    };
    let Some((opening_width, opening_height)) =
        opening_size_from_occurrence(world, &snapshot.identity)?
    else {
        return Ok(());
    };

    if let Some(opening_id) = hosting.opening_element_id {
        if let Some(opening_before) = capture_entity_snapshot(world, opening_id) {
            if opening_before.type_name() == "box" {
                let host_before = hosting
                    .host_element_id
                    .and_then(|id| capture_entity_snapshot(world, id));
                if let Some((width_axis, height_axis, _normal_axis)) =
                    relation_dimension_axes(host_before.as_ref(), &opening_before)
                {
                    let json = opening_before.to_json();
                    if let Some(raw_half_extents) = json.get("half_extents") {
                        let mut half_extents: [f32; 3] =
                            serde_json::from_value(raw_half_extents.clone()).map_err(|_| {
                                "Opening proxy half_extents must be a [x, y, z] array".to_string()
                            })?;
                        set_axis_value(&mut half_extents, width_axis, opening_width * 0.5);
                        set_axis_value(&mut half_extents, height_axis, opening_height * 0.5);
                        let opening_after = opening_before
                            .set_property_json("half_extents", &json!(half_extents))?;
                        before_snapshots.push(opening_before);
                        after_snapshots.push(opening_after);
                    }
                }
            }
        }
    }

    let Some(mut query) = world.try_query::<(&ElementId, &SemanticRelation)>() else {
        return Ok(());
    };
    for (relation_id, relation) in query.iter(world) {
        if relation.source != occurrence_id || relation.relation_type != "hosted_on" {
            continue;
        }
        let mut parameters = relation.parameters.clone();
        ensure_json_object(&mut parameters);
        if let Some(object) = parameters.as_object_mut() {
            object.insert("window_width_m".to_string(), json!(opening_width));
            object.insert("opening_width_m".to_string(), json!(opening_width));
            object.insert("window_height_m".to_string(), json!(opening_height));
            object.insert("opening_height_m".to_string(), json!(opening_height));
        }
        if parameters == relation.parameters {
            continue;
        }
        before_snapshots.push(
            RelationSnapshot {
                element_id: *relation_id,
                relation: relation.clone(),
            }
            .into(),
        );
        after_snapshots.push(
            RelationSnapshot {
                element_id: *relation_id,
                relation: SemanticRelation {
                    parameters,
                    ..relation.clone()
                },
            }
            .into(),
        );
    }
    Ok(())
}

#[cfg(feature = "model-api")]
fn copy_effective_definition_tree(
    registry: &crate::plugins::modeling::definition::DefinitionRegistry,
    source_id: &crate::plugins::modeling::definition::DefinitionId,
    root_name: Option<&str>,
    copy_dependencies: bool,
    copied: &mut HashMap<
        crate::plugins::modeling::definition::DefinitionId,
        crate::plugins::modeling::definition::DefinitionId,
    >,
    definitions: &mut Vec<crate::plugins::modeling::definition::Definition>,
) -> ApiResult<crate::plugins::modeling::definition::DefinitionId> {
    use crate::plugins::modeling::definition::DefinitionId;

    if let Some(existing) = copied.get(source_id) {
        return Ok(existing.clone());
    }

    let source = registry.effective_definition(source_id)?;
    let new_id = DefinitionId::new();
    copied.insert(source_id.clone(), new_id.clone());

    let mut copy = source.clone();
    copy.id = new_id.clone();
    copy.base_definition_id = None;
    copy.definition_version = 1;
    copy.name = root_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} Copy", source.name));

    if copy_dependencies {
        if let Some(compound) = copy.compound.as_mut() {
            for slot in &mut compound.child_slots {
                let child_new_id = copy_effective_definition_tree(
                    registry,
                    &slot.definition_id,
                    None,
                    true,
                    copied,
                    definitions,
                )?;
                slot.definition_id = child_new_id;
            }
        }
    }

    definitions.push(copy);
    Ok(new_id)
}

#[cfg(feature = "model-api")]
pub fn handle_make_occurrence_unique(
    world: &mut World,
    request: OccurrenceMakeUniqueRequest,
) -> ApiResult<MakeOccurrenceUniqueResult> {
    use crate::plugins::commands::{enqueue_apply_entity_changes, enqueue_create_definition};
    use crate::plugins::history::PendingCommandQueue;
    use crate::plugins::modeling::definition::DefinitionRegistry;

    let occurrence_id = ElementId(request.element_id);
    let before = capture_entity_snapshot(world, occurrence_id)
        .ok_or_else(|| format!("Entity {} not found", request.element_id))?;
    let occurrence = occurrence_snapshot(&before)
        .ok_or_else(|| format!("Entity {} is not an occurrence", request.element_id))?;
    let previous_definition_id = occurrence.identity.definition_id.clone();

    let mut copied = HashMap::new();
    let mut copied_definitions = Vec::new();
    let new_definition_id = {
        let registry = world.resource::<DefinitionRegistry>();
        copy_effective_definition_tree(
            registry,
            &previous_definition_id,
            request.name.as_deref(),
            request.copy_dependencies,
            &mut copied,
            &mut copied_definitions,
        )?
    };

    let mut preview = world.resource::<DefinitionRegistry>().clone();
    for definition in &copied_definitions {
        preview.validate_definition(definition)?;
        preview.insert(definition.clone());
    }

    let new_definition = copied_definitions
        .iter()
        .find(|definition| definition.id == new_definition_id)
        .ok_or_else(|| "Copied root definition was not generated".to_string())?;

    let mut after_occurrence = occurrence.clone();
    after_occurrence.identity.definition_id = new_definition_id.clone();
    after_occurrence.identity.definition_version = new_definition.definition_version;
    preview.validate_overrides(
        &after_occurrence.identity.definition_id,
        &after_occurrence.identity.overrides,
    )?;

    let copied_definition_ids = copied_definitions
        .iter()
        .map(|definition| definition.id.to_string())
        .collect::<Vec<_>>();

    world
        .resource_mut::<PendingCommandQueue>()
        .begin_group("Make occurrence unique");
    for definition in copied_definitions {
        enqueue_create_definition(world, definition);
    }
    enqueue_apply_entity_changes(
        world,
        ApplyEntityChangesCommand {
            label: "Make occurrence unique",
            before: vec![before],
            after: vec![BoxedEntity(Box::new(after_occurrence))],
        },
    );
    world.resource_mut::<PendingCommandQueue>().end_group();
    flush_model_api_write_pipeline(world);

    Ok(MakeOccurrenceUniqueResult {
        element_id: request.element_id,
        previous_definition_id: previous_definition_id.to_string(),
        new_definition_id: new_definition_id.to_string(),
        copied_definition_ids,
    })
}

#[cfg(feature = "model-api")]
pub fn handle_occurrence_validate_host_fit(
    world: &World,
    request: ValidateHostFitRequest,
) -> ApiResult<HostingValidationResult> {
    let contract_kind = HostingContractKindId(request.contract_kind);
    let registry = world
        .get_resource::<CapabilityRegistry>()
        .ok_or_else(|| "CapabilityRegistry is not available".to_string())?;
    let validation_request = HostingValidationRequest {
        contract_kind: contract_kind.clone(),
        host_element_id: ElementId(request.host_element_id),
        hosted_element_id: ElementId(request.hosted_element_id),
        contract_parameters: request.contract_parameters,
    };
    if let Some(descriptor) = registry.hosting_contract_descriptor(&contract_kind) {
        return Ok((descriptor.validator)(validation_request, world));
    }

    if contract_kind.0 == WALL_OPENING_HOSTING_CONTRACT_KIND {
        let opening_id = opening_id_from_contract_parameters(
            &validation_request.contract_parameters,
        )
        .or_else(|| opening_id_from_occurrence(world, validation_request.hosted_element_id));
        return Ok(wall_opening_validation_result(
            validation_request.contract_kind,
            validation_request.host_element_id,
            validation_request.hosted_element_id,
            capture_entity_snapshot(world, validation_request.host_element_id),
            opening_id.and_then(|id| capture_entity_snapshot(world, id)),
            hosted_occurrence_placement_center(world, validation_request.hosted_element_id)
                .or_else(|| {
                    capture_entity_snapshot(world, validation_request.hosted_element_id)
                        .map(|snapshot| snapshot.center())
                }),
        ));
    }

    Err(format!(
        "Hosting contract '{}' is not registered",
        contract_kind.0
    ))
}

#[cfg(feature = "model-api")]
pub fn handle_definition_validate_host_contract(
    world: &World,
    request: ValidateDefinitionHostContractRequest,
) -> ApiResult<HostingValidationResult> {
    let definition_id = crate::plugins::modeling::definition::DefinitionId(request.definition_id);
    let definitions = world
        .get_resource::<crate::plugins::modeling::definition::DefinitionRegistry>()
        .ok_or_else(|| "DefinitionRegistry is not available".to_string())?;
    definitions
        .get(&definition_id)
        .ok_or_else(|| format!("Definition '{}' not found", definition_id.0))?;

    handle_occurrence_validate_host_fit(
        world,
        ValidateHostFitRequest {
            contract_kind: request.contract_kind,
            host_element_id: request.host_element_id,
            hosted_element_id: request.hosted_element_id,
            contract_parameters: request.contract_parameters,
        },
    )
}

#[cfg(feature = "model-api")]
pub fn handle_resolve_occurrence(world: &World, element_id: u64) -> ApiResult<Value> {
    use crate::plugins::modeling::definition::DefinitionRegistry;
    use crate::plugins::modeling::occurrence::OccurrenceIdentity;

    let eid = ElementId(element_id);
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    let identity = entity_ref
        .get::<OccurrenceIdentity>()
        .ok_or_else(|| format!("Entity {element_id} is not an occurrence"))?
        .clone();
    drop(q);

    let registry = world.resource::<DefinitionRegistry>();
    let resolved = registry.resolve_params_checked(&identity.definition_id, &identity.overrides)?;

    Ok(serde_json::to_value(resolved).unwrap_or(serde_json::Value::Null))
}

#[cfg(feature = "model-api")]
pub fn handle_explain_occurrence(
    world: &World,
    element_id: u64,
) -> ApiResult<OccurrenceExplainResult> {
    use crate::plugins::modeling::{
        definition::DefinitionRegistry,
        occurrence::{GeneratedOccurrencePart, OccurrenceIdentity},
        primitives::ShapeRotation,
        profile::ProfileExtrusion,
    };

    let eid = ElementId(element_id);
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    let identity = entity_ref
        .get::<OccurrenceIdentity>()
        .ok_or_else(|| format!("Entity {element_id} is not an occurrence"))?
        .clone();
    let transform = entity_ref.get::<Transform>().copied().unwrap_or_default();
    drop(q);

    let registry = world.resource::<DefinitionRegistry>();
    let definition = registry
        .get(&identity.definition_id)
        .ok_or_else(|| format!("Definition '{}' not found", identity.definition_id))?;
    let resolved = registry.resolve_params_checked(&identity.definition_id, &identity.overrides)?;
    let anchors = definition
        .compound
        .as_ref()
        .map(|compound| {
            compound
                .anchors
                .iter()
                .map(|anchor| serde_json::to_value(anchor).unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_default();

    let label = get_entity_snapshot(world, eid)
        .and_then(|value| {
            value
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Occurrence".to_string());

    let mut generated_query = world
        .try_query::<(
            &GeneratedOccurrencePart,
            &ProfileExtrusion,
            Option<&ShapeRotation>,
        )>()
        .ok_or_else(|| "Failed to create generated part query".to_string())?;
    let generated_parts = generated_query
        .iter(world)
        .filter(|(generated, _, _)| generated.owner == eid)
        .map(|(generated, extrusion, _rotation)| {
            let (profile_min, profile_max) = extrusion.profile.bounds_2d();
            GeneratedOccurrencePartEntry {
                slot_path: generated.slot_path.clone(),
                definition_id: generated.definition_id.to_string(),
                center: [extrusion.centre.x, extrusion.centre.y, extrusion.centre.z],
                profile_min: [profile_min.x, profile_min.y],
                profile_max: [profile_max.x, profile_max.y],
                height: extrusion.height,
            }
        })
        .collect();

    Ok(OccurrenceExplainResult {
        element_id,
        label,
        definition_id: identity.definition_id.to_string(),
        definition_version: identity.definition_version,
        domain_data: identity.domain_data.clone(),
        hosting: serde_json::to_value(&identity.hosting).unwrap_or(Value::Null),
        transform: serde_json::json!({
            "translation": [transform.translation.x, transform.translation.y, transform.translation.z],
            "rotation": [transform.rotation.x, transform.rotation.y, transform.rotation.z, transform.rotation.w],
            "scale": [transform.scale.x, transform.scale.y, transform.scale.z],
        }),
        resolved_parameters: serde_json::to_value(resolved).unwrap_or(Value::Null),
        anchors,
        generated_parts,
    })
}

// ---------------------------------------------------------------------------
// Array handlers
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_array_create_linear(
    world: &mut World,
    source_id: u64,
    count: u32,
    spacing: [f32; 3],
) -> ApiResult<u64> {
    use crate::plugins::modeling::array::{LinearArrayNode, LinearArraySnapshot};

    ensure_entity_exists(world, ElementId(source_id))?;

    let array_id = world
        .resource::<crate::plugins::identity::ElementIdAllocator>()
        .next_id();

    let snapshot = LinearArraySnapshot {
        element_id: array_id,
        node: LinearArrayNode {
            source: ElementId(source_id),
            count: count.max(2),
            spacing: bevy::math::Vec3::from(spacing),
        },
    };

    send_event(
        world,
        crate::plugins::commands::CreateEntityCommand {
            snapshot: snapshot.into(),
        },
    );
    flush_model_api_write_pipeline(world);

    get_entity_snapshot(world, array_id)
        .map(|_| array_id.0)
        .ok_or_else(|| "Failed to create linear array entity".to_string())
}

#[cfg(feature = "model-api")]
fn handle_array_create_polar(
    world: &mut World,
    source_id: u64,
    count: u32,
    axis: [f32; 3],
    total_angle_degrees: f32,
    center: [f32; 3],
) -> ApiResult<u64> {
    use crate::plugins::modeling::array::{PolarArrayNode, PolarArraySnapshot};

    ensure_entity_exists(world, ElementId(source_id))?;

    let array_id = world
        .resource::<crate::plugins::identity::ElementIdAllocator>()
        .next_id();

    let snapshot = PolarArraySnapshot {
        element_id: array_id,
        node: PolarArrayNode {
            source: ElementId(source_id),
            count: count.max(2),
            axis: bevy::math::Vec3::from(axis),
            total_angle_degrees,
            center: bevy::math::Vec3::from(center),
        },
    };

    send_event(
        world,
        crate::plugins::commands::CreateEntityCommand {
            snapshot: snapshot.into(),
        },
    );
    flush_model_api_write_pipeline(world);

    get_entity_snapshot(world, array_id)
        .map(|_| array_id.0)
        .ok_or_else(|| "Failed to create polar array entity".to_string())
}

#[cfg(feature = "model-api")]
fn handle_array_update(
    world: &mut World,
    element_id: u64,
    count: Option<u32>,
    spacing: Option<[f32; 3]>,
    axis: Option<[f32; 3]>,
    total_angle_degrees: Option<f32>,
    center: Option<[f32; 3]>,
) -> ApiResult<Value> {
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::commands::ApplyEntityChangesCommand;
    use crate::plugins::modeling::array::{LinearArraySnapshot, PolarArraySnapshot};

    let eid = ElementId(element_id);
    let before = capture_snapshot_by_id(world, eid)?;

    // Attempt linear array first, then polar.
    if let Some(linear_snap) = before.0.as_any().downcast_ref::<LinearArraySnapshot>() {
        let mut updated = linear_snap.clone();
        if let Some(c) = count {
            updated.node.count = c.max(2);
        }
        if let Some(s) = spacing {
            updated.node.spacing = bevy::math::Vec3::from(s);
        }
        let after_json = updated.to_json();
        let after: crate::authored_entity::BoxedEntity = updated.into();
        send_event(
            world,
            ApplyEntityChangesCommand {
                label: "Update linear array",
                before: vec![before],
                after: vec![after],
            },
        );
        flush_model_api_write_pipeline(world);
        return Ok(after_json);
    }

    if let Some(polar_snap) = before.0.as_any().downcast_ref::<PolarArraySnapshot>() {
        let mut updated = polar_snap.clone();
        if let Some(c) = count {
            updated.node.count = c.max(2);
        }
        if let Some(a) = axis {
            updated.node.axis = bevy::math::Vec3::from(a);
        }
        if let Some(angle) = total_angle_degrees {
            updated.node.total_angle_degrees = angle;
        }
        if let Some(ctr) = center {
            updated.node.center = bevy::math::Vec3::from(ctr);
        }
        let after_json = updated.to_json();
        let after: crate::authored_entity::BoxedEntity = updated.into();
        send_event(
            world,
            ApplyEntityChangesCommand {
                label: "Update polar array",
                before: vec![before],
                after: vec![after],
            },
        );
        flush_model_api_write_pipeline(world);
        return Ok(after_json);
    }

    Err(format!(
        "Entity {element_id} is not a linear or polar array node"
    ))
}

#[cfg(feature = "model-api")]
fn handle_array_dissolve(world: &mut World, element_id: u64) -> ApiResult<u64> {
    use crate::plugins::commands::{CreateEntityCommand, ResolvedDeleteEntitiesCommand};
    use crate::plugins::identity::ElementIdAllocator;
    use crate::plugins::modeling::array::EvaluatedArray;
    use crate::plugins::modeling::primitives::TriangleMesh;
    use crate::plugins::modeling::snapshots::TriangleMeshSnapshot;

    let eid = ElementId(element_id);

    let evaluated = {
        let mut q = world
            .try_query::<(Entity, &ElementId, &EvaluatedArray)>()
            .unwrap();
        q.iter(world)
            .find(|(_, id, _)| **id == eid)
            .map(|(_, _, ev)| ev.clone())
            .ok_or_else(|| {
                format!(
                    "Entity {element_id} is not an evaluated array node (has it been evaluated yet?)"
                )
            })?
    };

    send_event(
        world,
        ResolvedDeleteEntitiesCommand {
            element_ids: vec![eid],
        },
    );

    let faces: Vec<[u32; 3]> = evaluated
        .indices
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();

    let new_id = world.resource::<ElementIdAllocator>().next_id();
    let tri_mesh = TriangleMesh {
        vertices: evaluated.vertices.clone(),
        faces,
        normals: Some(evaluated.normals.clone()),
        name: None,
    };
    let snapshot = TriangleMeshSnapshot {
        element_id: new_id,
        primitive: tri_mesh,
        layer: None,
        material_assignment: None,
    };

    send_event(
        world,
        CreateEntityCommand {
            snapshot: snapshot.into(),
        },
    );
    flush_model_api_write_pipeline(world);
    Ok(new_id.0)
}

#[cfg(feature = "model-api")]
fn handle_array_get(world: &World, element_id: u64) -> ApiResult<Value> {
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::modeling::array::{LinearArrayNode, PolarArrayNode};

    let eid = ElementId(element_id);
    let mut q = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    if let Some(node) = entity_ref.get::<LinearArrayNode>() {
        use crate::plugins::modeling::array::LinearArraySnapshot;
        let snap = LinearArraySnapshot {
            element_id: eid,
            node: node.clone(),
        };
        return Ok(snap.to_json());
    }
    if let Some(node) = entity_ref.get::<PolarArrayNode>() {
        use crate::plugins::modeling::array::PolarArraySnapshot;
        let snap = PolarArraySnapshot {
            element_id: eid,
            node: node.clone(),
        };
        return Ok(snap.to_json());
    }

    Err(format!(
        "Entity {element_id} is not a linear or polar array node"
    ))
}

// ---------------------------------------------------------------------------
// Mirror handlers
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_mirror_create(
    world: &mut World,
    source_id: u64,
    plane_str: Option<String>,
    plane_origin: Option<[f32; 3]>,
    plane_normal: Option<[f32; 3]>,
    merge: Option<bool>,
) -> ApiResult<u64> {
    use crate::plugins::modeling::mirror::{MirrorNode, MirrorSnapshot};

    // Verify source exists.
    ensure_entity_exists(world, ElementId(source_id))?;

    let plane = build_mirror_plane(plane_str, plane_origin, plane_normal)?;
    let mirror_id = world
        .resource::<crate::plugins::identity::ElementIdAllocator>()
        .next_id();

    let snapshot = MirrorSnapshot {
        element_id: mirror_id,
        mirror_node: MirrorNode {
            source: ElementId(source_id),
            plane,
            merge: merge.unwrap_or(false),
        },
    };

    send_event(
        world,
        crate::plugins::commands::CreateEntityCommand {
            snapshot: snapshot.into(),
        },
    );
    flush_model_api_write_pipeline(world);

    get_entity_snapshot(world, mirror_id)
        .map(|_| mirror_id.0)
        .ok_or_else(|| "Failed to create mirror entity".to_string())
}

#[cfg(feature = "model-api")]
fn handle_mirror_update(
    world: &mut World,
    element_id: u64,
    plane_str: Option<String>,
    plane_origin: Option<[f32; 3]>,
    plane_normal: Option<[f32; 3]>,
    merge: Option<bool>,
) -> ApiResult<Value> {
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::commands::ApplyEntityChangesCommand;
    use crate::plugins::modeling::mirror::MirrorSnapshot;

    let eid = ElementId(element_id);
    let before = capture_snapshot_by_id(world, eid)?;
    let mirror_snap = before
        .0
        .as_any()
        .downcast_ref::<MirrorSnapshot>()
        .ok_or_else(|| format!("Entity {element_id} is not a mirror node"))?
        .clone();

    let mut updated = mirror_snap;

    // Only replace the plane when the caller provided plane parameters.
    if plane_str.is_some() || plane_origin.is_some() || plane_normal.is_some() {
        updated.mirror_node.plane = build_mirror_plane(plane_str, plane_origin, plane_normal)?;
    }
    if let Some(m) = merge {
        updated.mirror_node.merge = m;
    }

    let after_json = updated.to_json();
    let after: crate::authored_entity::BoxedEntity = updated.into();

    send_event(
        world,
        ApplyEntityChangesCommand {
            label: "Update mirror",
            before: vec![before],
            after: vec![after],
        },
    );
    flush_model_api_write_pipeline(world);
    Ok(after_json)
}

#[cfg(feature = "model-api")]
fn handle_mirror_dissolve(world: &mut World, element_id: u64) -> ApiResult<u64> {
    use crate::plugins::commands::{CreateEntityCommand, ResolvedDeleteEntitiesCommand};
    use crate::plugins::identity::ElementIdAllocator;
    use crate::plugins::modeling::mirror::EvaluatedMirror;
    use crate::plugins::modeling::primitives::TriangleMesh;
    use crate::plugins::modeling::snapshots::TriangleMeshSnapshot;

    let eid = ElementId(element_id);

    // Capture the evaluated geometry before deletion.
    let evaluated = {
        let mut q = world
            .try_query::<(Entity, &ElementId, &EvaluatedMirror)>()
            .unwrap();
        q.iter(world)
            .find(|(_, id, _)| **id == eid)
            .map(|(_, _, ev)| ev.clone())
            .ok_or_else(|| {
                format!(
                    "Entity {element_id} is not an evaluated mirror node (has it been evaluated yet?)"
                )
            })?
    };

    // Delete the mirror entity.
    send_event(
        world,
        ResolvedDeleteEntitiesCommand {
            element_ids: vec![eid],
        },
    );

    // Convert the flat index buffer to [u32; 3] face triples.
    let faces: Vec<[u32; 3]> = evaluated
        .indices
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();

    // Create an independent TriangleMesh with the reflected geometry.
    let new_id = world.resource::<ElementIdAllocator>().next_id();
    let tri_mesh = TriangleMesh {
        vertices: evaluated.vertices.clone(),
        faces,
        normals: Some(evaluated.normals.clone()),
        name: None,
    };
    let snapshot = TriangleMeshSnapshot {
        element_id: new_id,
        primitive: tri_mesh,
        layer: None,
        material_assignment: None,
    };

    send_event(
        world,
        CreateEntityCommand {
            snapshot: snapshot.into(),
        },
    );
    flush_model_api_write_pipeline(world);
    Ok(new_id.0)
}

#[cfg(feature = "model-api")]
fn handle_mirror_get(world: &World, element_id: u64) -> ApiResult<Value> {
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::modeling::mirror::{MirrorNode, MirrorSnapshot};

    let eid = ElementId(element_id);
    let mut q = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(eid))
        .ok_or_else(|| format!("Entity {element_id} not found"))?;

    let mirror_node = entity_ref
        .get::<MirrorNode>()
        .ok_or_else(|| format!("Entity {element_id} is not a mirror node"))?
        .clone();
    let snap = MirrorSnapshot {
        element_id: eid,
        mirror_node,
    };
    Ok(snap.to_json())
}

/// Build a `MirrorPlane` from the optional API parameters.
///
/// Priority: `plane_str` shortcut → `plane_origin` + `plane_normal` → XZ default.
#[cfg(feature = "model-api")]
fn build_mirror_plane(
    plane_str: Option<String>,
    plane_origin: Option<[f32; 3]>,
    plane_normal: Option<[f32; 3]>,
) -> ApiResult<crate::plugins::modeling::mirror::MirrorPlane> {
    use crate::plugins::modeling::mirror::MirrorPlane;

    if let Some(s) = plane_str {
        return MirrorPlane::try_from(s.as_str());
    }
    if let (Some(origin), Some(normal)) = (plane_origin, plane_normal) {
        return Ok(MirrorPlane::new(
            bevy::math::Vec3::from(origin),
            bevy::math::Vec3::from(normal),
        ));
    }
    Ok(MirrorPlane::xz())
}

#[cfg(feature = "model-api")]
fn flush_model_api_write_pipeline(world: &mut World) {
    queue_command_events(world);
    apply_pending_history_commands(world);
}

/// `SessionStepExecutor` that routes committed Semantic Procedural Session
/// steps to the existing Model API world handlers, so a session commit
/// produces real geometry / authored entities (ADR-051, PP-SPS-3/4).
///
/// Supported step tools in v1: `create_box`, `create_entity`,
/// `set_property`. Each returns a JSON object containing the affected
/// `element_id` so later steps can bind to it.
#[cfg(feature = "model-api")]
pub struct ModelApiStepExecutor;

#[cfg(feature = "model-api")]
impl crate::plugins::procedural_session_mcp::SessionStepExecutor for ModelApiStepExecutor {
    fn execute(
        &mut self,
        world: &mut World,
        tool: &crate::curation::McpToolId,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Value, crate::curation::ToolDispatchError> {
        use crate::curation::ToolDispatchError;
        let to_err = |code: &str, e: String| ToolDispatchError::new(code, e);
        match tool.as_str() {
            "create_box" => {
                let request: CreateBoxRequest = serde_json::from_value(Value::Object(args.clone()))
                    .map_err(|e| to_err("invalid_args", e.to_string()))?;
                let id = handle_create_box(world, request)
                    .map_err(|e| to_err("create_box_failed", e))?;
                Ok(serde_json::json!({ "element_id": id }))
            }
            "create_entity" => {
                let id = handle_create_entity(world, Value::Object(args.clone()))
                    .map_err(|e| to_err("create_entity_failed", e))?;
                Ok(serde_json::json!({ "element_id": id }))
            }
            "set_property" => {
                let element_id = args
                    .get("element_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| to_err("invalid_args", "missing element_id".into()))?;
                let property_name = args
                    .get("property_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| to_err("invalid_args", "missing property_name".into()))?;
                let value = args.get("value").cloned().unwrap_or(Value::Null);
                let result = handle_set_property(world, element_id, property_name, value)
                    .map_err(|e| to_err("set_property_failed", e))?;
                Ok(result)
            }
            other => Err(to_err(
                "unsupported_tool",
                format!("ModelApiStepExecutor does not support tool '{other}'"),
            )),
        }
    }
}

/// Register `SessionToolDescriptor`s for the Model API tools a procedural
/// session may commit. Called from `ModelApiPlugin::build` so `eval`
/// accepts these tools and dry-run projections have a sensible stub shape.
#[cfg(feature = "model-api")]
pub fn register_model_api_session_tools(
    registry: &mut crate::curation::procedural_session::SessionToolRegistry,
) {
    use crate::curation::procedural_session::SessionToolDescriptor;
    use crate::curation::McpToolId;
    registry.register(SessionToolDescriptor {
        tool: McpToolId::new("create_box"),
        mutates: true,
        default_stub: Some(serde_json::json!({ "element_id": 0 })),
        creates_obligations: Vec::new(),
        satisfies_obligation_ids: Vec::new(),
    });
    registry.register(SessionToolDescriptor {
        tool: McpToolId::new("create_entity"),
        mutates: true,
        default_stub: Some(serde_json::json!({ "element_id": 0 })),
        creates_obligations: Vec::new(),
        satisfies_obligation_ids: Vec::new(),
    });
    registry.register(SessionToolDescriptor {
        tool: McpToolId::new("set_property"),
        mutates: true,
        default_stub: Some(serde_json::json!({ "ok": true })),
        creates_obligations: Vec::new(),
        satisfies_obligation_ids: Vec::new(),
    });
}

#[cfg(feature = "model-api")]
fn execute_model_api_create_entity_command(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let element_id = handle_create_entity(world, parameters.clone())?;
    Ok(CommandResult {
        created: vec![element_id],
        output: Some(json!({ "element_id": element_id })),
        ..CommandResult::default()
    })
}

#[cfg(feature = "model-api")]
fn execute_model_api_create_box_command(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let request: CreateBoxRequest =
        serde_json::from_value(parameters.clone()).map_err(|e| e.to_string())?;
    let element_id = handle_create_box(world, request)?;
    Ok(CommandResult {
        created: vec![element_id],
        output: Some(json!({ "element_id": element_id })),
        ..CommandResult::default()
    })
}

#[cfg(feature = "model-api")]
fn execute_model_api_delete_entities_command(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let request: DeleteEntitiesRequest =
        serde_json::from_value(parameters.clone()).map_err(|e| e.to_string())?;
    let requested_ids = request.element_ids.clone();
    let deleted_count = handle_delete_entities(world, request.element_ids)?;
    Ok(CommandResult {
        deleted: requested_ids,
        output: Some(json!({ "deleted_count": deleted_count })),
        ..CommandResult::default()
    })
}

#[cfg(feature = "model-api")]
fn execute_model_api_transform_entities_command(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let request: TransformToolRequest =
        serde_json::from_value(parameters.clone()).map_err(|e| e.to_string())?;
    let modified = request.element_ids.clone();
    let snapshots = handle_transform(world, request)?;
    Ok(CommandResult {
        modified,
        output: Some(Value::Array(snapshots)),
        ..CommandResult::default()
    })
}

#[cfg(feature = "model-api")]
fn execute_model_api_set_entity_property_command(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let request: SetPropertyRequest =
        serde_json::from_value(parameters.clone()).map_err(|e| e.to_string())?;
    let updated = handle_set_property(
        world,
        request.element_id,
        &request.property_name,
        request.value,
    )?;
    Ok(CommandResult {
        modified: vec![request.element_id],
        output: Some(updated),
        ..CommandResult::default()
    })
}

#[cfg(feature = "model-api")]
pub fn handle_create_entity(world: &mut World, json: Value) -> Result<u64, String> {
    let mut request_json = json;
    let object = request_json
        .as_object_mut()
        .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
    let semantic_annotation = object
        .remove("semantic")
        .map(serde_json::from_value::<SemanticEntityAnnotationRequest>)
        .transpose()
        .map_err(|error| format!("Invalid semantic annotation: {error}"))?;
    let entity_type = required_string(object, "type")?.to_ascii_lowercase();
    let registry = world.resource::<CapabilityRegistry>();
    let factory = registry.factory_for(&entity_type).ok_or_else(|| {
        let valid_types: Vec<&str> = registry.factories().iter().map(|f| f.type_name()).collect();
        format!(
            "Invalid entity type '{entity_type}'. Valid types: {}",
            valid_types.join(", ")
        )
    })?;
    let snapshot = factory.from_create_request(world, &request_json)?;
    let element_id = snapshot.element_id();
    send_event(
        world,
        crate::plugins::commands::CreateEntityCommand { snapshot },
    );

    flush_model_api_write_pipeline(world);
    if let Some(annotation) = semantic_annotation {
        apply_semantic_annotation(world, element_id, annotation)?;
    }

    get_entity_snapshot(world, element_id)
        .map(|_| element_id.0)
        .ok_or_else(|| format!("Failed to create entity of type '{entity_type}'"))
}

#[cfg(feature = "model-api")]
fn apply_semantic_annotation(
    world: &mut World,
    element_id: ElementId,
    annotation: SemanticEntityAnnotationRequest,
) -> Result<(), String> {
    use crate::capability_registry::{ElementClassAssignment, ElementClassId};
    use crate::plugins::refinement::{
        AuthoringMode, AuthoringProvenance, RefinementState, RefinementStateComponent,
        SemanticIntent,
    };

    let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
    let entity = q
        .iter(world)
        .find_map(|(entity, id)| (*id == element_id).then_some(entity))
        .ok_or_else(|| format!("Entity {} not found after creation", element_id.0))?;

    let element_class = annotation
        .element_class
        .as_deref()
        .map(|id| ElementClassId(id.to_string()));
    let refinement_state = annotation
        .refinement_state
        .as_deref()
        .map(|state| {
            RefinementState::from_str(state).ok_or_else(|| {
                format!(
                    "Invalid refinement_state '{state}'. Valid states: Conceptual, Schematic, Constructible, Detailed, FabricationReady"
                )
            })
        })
        .transpose()?;

    if let Some(element_class) = &element_class {
        let registry = world
            .get_resource::<CapabilityRegistry>()
            .ok_or_else(|| "CapabilityRegistry is not available".to_string())?;
        if registry.element_class_descriptor(element_class).is_none() {
            return Err(format!(
                "Unknown element_class '{}'; register the domain vocabulary before creating semantically typed entities",
                element_class.0
            ));
        }
    }

    let mut entity_mut = world.entity_mut(entity);
    if let Some(element_class) = element_class {
        entity_mut.insert(ElementClassAssignment {
            element_class,
            active_recipe: None,
        });
    }
    if annotation.refinement_state.is_some() || annotation.element_class.is_some() {
        entity_mut.insert(RefinementStateComponent {
            state: refinement_state.unwrap_or(RefinementState::Conceptual),
        });
    }
    if !annotation.parameters.is_null()
        || !annotation.unresolved_decisions.is_empty()
        || !annotation.source_refs.is_empty()
    {
        entity_mut.insert(SemanticIntent {
            parameters: annotation.parameters,
            unresolved_decisions: annotation.unresolved_decisions,
            source_refs: annotation.source_refs,
        });
    }
    if annotation.rationale.is_some() {
        entity_mut.insert(AuthoringProvenance {
            mode: AuthoringMode::Freeform,
            rationale: annotation.rationale,
        });
    }

    Ok(())
}

#[cfg(feature = "model-api")]
pub fn handle_create_box(world: &mut World, request: CreateBoxRequest) -> Result<u64, String> {
    let json = create_box_request_json(&request)?;
    handle_create_entity(world, json)
}

#[cfg(feature = "model-api")]
pub fn handle_import_file(
    world: &mut World,
    path: &str,
    format_hint: Option<&str>,
) -> Result<Vec<u64>, String> {
    let element_ids = import_file_now(world, std::path::Path::new(path), format_hint)?;
    flush_model_api_write_pipeline(world);
    Ok(element_ids)
}

#[cfg(feature = "model-api")]
pub fn handle_delete_entities(world: &mut World, element_ids: Vec<u64>) -> Result<usize, String> {
    if element_ids.is_empty() {
        return Err("No entities found for the given IDs".to_string());
    }

    let ids: Vec<ElementId> = element_ids.into_iter().map(ElementId).collect();
    for element_id in &ids {
        ensure_user_editable_entity(world, *element_id, "deleted")?;
    }

    let expanded_ids = world
        .resource::<CapabilityRegistry>()
        .expand_delete_ids(world, &ids);
    let deleted_count = expanded_ids.len();
    send_event(
        world,
        ResolvedDeleteEntitiesCommand {
            element_ids: expanded_ids,
        },
    );
    flush_model_api_write_pipeline(world);
    Ok(deleted_count)
}

#[cfg(feature = "model-api")]
pub fn handle_transform(
    world: &mut World,
    request: TransformToolRequest,
) -> Result<Vec<Value>, String> {
    for element_id in &request.element_ids {
        ensure_user_editable_entity(world, ElementId(*element_id), "transformed")?;
    }
    let snapshots = capture_snapshots_by_ids(world, &request.element_ids)?;
    if snapshots.is_empty() {
        return Err("No entities found for the given IDs".to_string());
    }

    let after = apply_transform_request(&snapshots, &request)?;
    let before = snapshots
        .iter()
        .map(|(_, snapshot)| snapshot.clone())
        .collect();
    send_event(
        world,
        ApplyEntityChangesCommand {
            label: "AI transform",
            before,
            after: after.clone(),
        },
    );
    flush_model_api_write_pipeline(world);

    after
        .into_iter()
        .map(|snapshot| Ok(snapshot.to_json()))
        .collect()
}

#[cfg(feature = "model-api")]
pub fn handle_set_property(
    world: &mut World,
    element_id: u64,
    property_name: &str,
    value: Value,
) -> Result<Value, String> {
    ensure_user_editable_entity(world, ElementId(element_id), "edited")?;
    let snapshot = capture_snapshot_by_id(world, ElementId(element_id))?;
    let updated = snapshot.set_property_json(property_name, &value)?;
    send_event(
        world,
        ApplyEntityChangesCommand {
            label: "AI set property",
            before: vec![snapshot],
            after: vec![updated.clone()],
        },
    );
    flush_model_api_write_pipeline(world);
    Ok(updated.to_json())
}

#[cfg(feature = "model-api")]
pub fn handle_list_handles(world: &World, element_id: u64) -> Result<Vec<HandleInfo>, String> {
    let snapshot = capture_snapshot_by_id(world, ElementId(element_id))?;
    Ok(snapshot
        .handles()
        .into_iter()
        .map(|handle| HandleInfo {
            id: handle.id,
            position: handle.position.into(),
            kind: handle.kind.as_str().to_string(),
            label: handle.label,
        })
        .collect())
}

/// ADR-026 Phase 6a MCP handler: read a single BIM property-set
/// value. Returns the typed `PropertyValue` JSON, or `Value::Null`
/// when the entity has no `PropertySetMap` component or the
/// requested property is not authored.
#[cfg(feature = "model-api")]
pub fn handle_bim_property_set_get(
    world: &mut World,
    element_id: u64,
    set_name: &str,
    property_name: &str,
) -> Result<Value, String> {
    use crate::plugins::modeling::property_sets::PropertySetMap;
    let entity = find_entity_by_element_id(world, ElementId(element_id))
        .ok_or_else(|| format!("element {element_id} not found"))?;
    let Some(map) = world.get::<PropertySetMap>(entity) else {
        return Ok(Value::Null);
    };
    Ok(map
        .get(set_name, property_name)
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .unwrap_or(Value::Null))
}

/// ADR-026 Phase 6a MCP handler: write a single BIM property-set
/// value. Validates the value against the
/// `PropertySetSchemaRegistry` for the given definition id and
/// emits a `PropertySetChanged` message on success. Returns the
/// prior value as JSON (or `Value::Null`).
///
/// Per ADR-026 §1 this handler does NOT invalidate the mesh cache:
/// the data lives in `PropertySetMap`, a sibling component to
/// `OccurrenceIdentity`; the geometry-evaluation pipeline never
/// queries it.
#[cfg(feature = "model-api")]
pub fn handle_bim_property_set_set(
    world: &mut World,
    element_id: u64,
    definition_id: &str,
    set_name: &str,
    property_name: &str,
    value: Value,
) -> Result<Value, String> {
    use crate::plugins::modeling::definition::DefinitionId;
    use crate::plugins::modeling::property_sets::{
        set_property_validated, PropertySetChangeKind, PropertySetChanged, PropertySetMap,
        PropertySetSchemaRegistry, PropertyValue,
    };
    use bevy::ecs::message::Messages;

    let entity = find_entity_by_element_id(world, ElementId(element_id))
        .ok_or_else(|| format!("element {element_id} not found"))?;
    let parsed_value: PropertyValue = serde_json::from_value(value).map_err(|e| {
        format!(
            "value must be a typed PropertyValue JSON ({{\"text\": ...}} / \
             {{\"number\": ...}} / etc.); got error: {e}"
        )
    })?;
    let def_id = DefinitionId(definition_id.to_string());

    // Ensure the entity has a PropertySetMap; insert empty if not.
    if world.get::<PropertySetMap>(entity).is_none() {
        world.entity_mut(entity).insert(PropertySetMap::default());
    }

    // Validate against the registered schema, then mutate.
    let prior = {
        let registry_clone: PropertySetSchemaRegistry = world
            .get_resource::<PropertySetSchemaRegistry>()
            .cloned()
            .unwrap_or_default();
        let mut map_view = world
            .get_mut::<PropertySetMap>(entity)
            .ok_or_else(|| "PropertySetMap component missing after insert".to_string())?;
        set_property_validated(
            &mut map_view,
            &registry_clone,
            &def_id,
            set_name,
            property_name,
            parsed_value,
        )?
    };

    // Emit PropertySetChanged message. The geometry pipeline does
    // not consume this message — only validation, export, and AI
    // inspection surfaces do.
    let kind = match &prior {
        Some(p) => PropertySetChangeKind::Updated { prior: p.clone() },
        None => PropertySetChangeKind::Created,
    };
    if let Some(mut messages) = world.get_resource_mut::<Messages<PropertySetChanged>>() {
        messages.write(PropertySetChanged {
            element_id: ElementId(element_id),
            set_name: set_name.to_string(),
            property_name: property_name.to_string(),
            kind,
        });
    }

    // No flush_model_api_write_pipeline call: per ADR-026 §1 the
    // write must NOT invalidate the mesh cache, and there is no
    // dirty marker to flush.

    Ok(prior
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .unwrap_or(Value::Null))
}

#[cfg(feature = "model-api")]
fn parse_exchange_system(
    system: &str,
) -> Result<crate::plugins::modeling::exchange_identity::ExchangeSystem, String> {
    use crate::plugins::modeling::exchange_identity::ExchangeSystem;
    let label = system.trim();
    if label.is_empty() {
        return Err("exchange system must be non-empty".to_string());
    }
    Ok(match label.to_ascii_lowercase().as_str() {
        "ifc" => ExchangeSystem::Ifc,
        "revit" => ExchangeSystem::Revit,
        "dwg" => ExchangeSystem::Dwg,
        "cobie" => ExchangeSystem::Cobie,
        _ => ExchangeSystem::Custom(label.to_string()),
    })
}

/// ADR-026 Phase 6b MCP handler: assign a stable BIM exchange id to an
/// entity if no id exists for that exchange system. Returns
/// `Value::Null` on success.
#[cfg(feature = "model-api")]
pub fn handle_bim_exchange_identity_assign(
    world: &mut World,
    element_id: u64,
    system: &str,
    exchange_id: &str,
) -> Result<Value, String> {
    use crate::plugins::modeling::exchange_identity::{ExchangeId, ExchangeIdentityMap};

    let exchange_id = exchange_id.trim();
    if exchange_id.is_empty() {
        return Err("exchange_id must be non-empty".to_string());
    }
    let system = parse_exchange_system(system)?;
    let entity = find_entity_by_element_id(world, ElementId(element_id))
        .ok_or_else(|| format!("element {element_id} not found"))?;
    if world.get::<ExchangeIdentityMap>(entity).is_none() {
        world
            .entity_mut(entity)
            .insert(ExchangeIdentityMap::empty());
    }
    let mut map = world
        .get_mut::<ExchangeIdentityMap>(entity)
        .ok_or_else(|| "ExchangeIdentityMap component missing after insert".to_string())?;
    map.assign_if_absent(system, ExchangeId::new(exchange_id))
        .map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

/// ADR-026 Phase 6b MCP handler: read one BIM exchange id from an
/// entity. Returns the id string, or `Value::Null` if that system has
/// not been assigned.
#[cfg(feature = "model-api")]
pub fn handle_bim_exchange_identity_get(
    world: &mut World,
    element_id: u64,
    system: &str,
) -> Result<Value, String> {
    use crate::plugins::modeling::exchange_identity::ExchangeIdentityMap;

    let system = parse_exchange_system(system)?;
    let entity = find_entity_by_element_id(world, ElementId(element_id))
        .ok_or_else(|| format!("element {element_id} not found"))?;
    let Some(map) = world.get::<ExchangeIdentityMap>(entity) else {
        return Ok(Value::Null);
    };
    Ok(map
        .get(&system)
        .map(|id| Value::String(id.as_str().to_string()))
        .unwrap_or(Value::Null))
}

/// ADR-026 Phase 6b MCP handler: list all BIM exchange identities
/// assigned to an entity as a JSON object keyed by exchange system
/// label.
#[cfg(feature = "model-api")]
pub fn handle_bim_exchange_identity_list(
    world: &mut World,
    element_id: u64,
) -> Result<Value, String> {
    use crate::plugins::modeling::exchange_identity::ExchangeIdentityMap;

    let entity = find_entity_by_element_id(world, ElementId(element_id))
        .ok_or_else(|| format!("element {element_id} not found"))?;
    let Some(map) = world.get::<ExchangeIdentityMap>(entity) else {
        return Ok(serde_json::json!({}));
    };
    let mut entries: Vec<(&str, &str)> = map
        .iter()
        .map(|(system, exchange_id)| (system.as_label(), exchange_id.as_str()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = serde_json::Map::new();
    for (system, exchange_id) in entries {
        out.insert(system.to_string(), Value::String(exchange_id.to_string()));
    }
    Ok(Value::Object(out))
}

/// ADR-026 Phase 6f MCP handler: write a `VoidDeclaration` into a
/// Definition interface. Returns the prior declaration as JSON, or
/// `Value::Null` if no prior was declared.
#[cfg(feature = "model-api")]
pub fn handle_bim_void_declare_for_definition(
    world: &mut World,
    definition_id: &str,
    declaration: Value,
) -> Result<Value, String> {
    use crate::plugins::commands::enqueue_update_definition;
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};
    use crate::plugins::modeling::void_declaration::VoidDeclaration;

    let parsed: VoidDeclaration = serde_json::from_value(declaration)
        .map_err(|e| format!("declaration must be a typed VoidDeclaration JSON: {e}"))?;
    let id = DefinitionId(definition_id.to_string());
    let before = world
        .resource::<DefinitionRegistry>()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("Definition '{definition_id}' not found"))?;
    let prior = before.interface.void_declaration.clone();
    let mut after = before.clone();
    after.definition_version += 1;
    after.interface.void_declaration = Some(parsed);
    {
        let registry = world.resource::<DefinitionRegistry>();
        registry.validate_definition(&after)?;
    }
    enqueue_update_definition(world, before, after);
    flush_model_api_write_pipeline(world);
    Ok(prior
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .unwrap_or(Value::Null))
}

/// ADR-026 Phase 6f MCP handler: plan an atomic void placement.
/// Returns a JSON object `{ opening_element, filling_link, opening_context }`.
#[cfg(feature = "model-api")]
pub fn handle_bim_void_plan_placement(
    world: &mut World,
    filling_definition: &str,
    host_element_id: u64,
    filling_element_id: u64,
) -> Result<Value, String> {
    use crate::plugins::identity::ElementIdAllocator;
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};
    use crate::plugins::modeling::void_declaration::plan_void_placement;

    let definition_id = DefinitionId(filling_definition.to_string());
    let definition = world
        .resource::<DefinitionRegistry>()
        .effective_definition(&definition_id)?;
    let host = ElementId(host_element_id);
    let filling = ElementId(filling_element_id);
    ensure_entity_exists(world, host)?;
    ensure_entity_exists(world, filling)?;
    let next_opening_id = world.resource::<ElementIdAllocator>().next_id();
    let outcome = plan_void_placement(
        definition.interface.void_declaration.as_ref(),
        &definition_id,
        host,
        filling,
        next_opening_id,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "opening_element": outcome.opening_element.0,
        "filling_link": {
            "opening": outcome.filling_link.opening.0,
        },
        "opening_context": {
            "host": outcome.opening_context.host.0,
            "filling": outcome.opening_context.filling.map(|f| f.0),
        },
    }))
}

/// ADR-026 Phase 6g MCP handler: validate + insert a
/// `SpatialMembership` component on the child entity. Returns
/// `Value::Null` on success.
#[cfg(feature = "model-api")]
pub fn handle_bim_spatial_assign(
    world: &mut World,
    child_element_id: u64,
    container_element_id: u64,
    container_kind: &str,
) -> Result<Value, String> {
    use crate::plugins::modeling::spatial_container::{
        validate_assignment, SpatialContainerKind, SpatialContainerKindRegistry,
        SpatialContainmentGraph, SpatialMembership,
    };

    let child = ElementId(child_element_id);
    let container = ElementId(container_element_id);
    let child_entity = find_entity_by_element_id(world, child)
        .ok_or_else(|| format!("child {child_element_id} not found"))?;
    ensure_entity_exists(world, container)?;

    // Build the current containment graph from existing SpatialMembership
    // components.
    let mut graph = SpatialContainmentGraph::new();
    if let Some(mut q) = world.try_query::<(&ElementId, &SpatialMembership)>() {
        for (id, m) in q.iter(world) {
            graph.parent_of.insert(*id, m.container);
        }
    }
    let kinds = world
        .get_resource::<SpatialContainerKindRegistry>()
        .cloned()
        .unwrap_or_default();
    let kind = SpatialContainerKind::new(container_kind);
    validate_assignment(&graph, &kinds, &kind, child, container).map_err(|e| e.to_string())?;
    world
        .entity_mut(child_entity)
        .insert(SpatialMembership::in_container(container));
    Ok(Value::Null)
}

/// ADR-026 Phase 6g MCP handler: list the registered spatial
/// container kinds.
#[cfg(feature = "model-api")]
pub fn handle_bim_spatial_list_kind_registry(world: &mut World) -> Result<Value, String> {
    use crate::plugins::modeling::spatial_container::SpatialContainerKindRegistry;
    let kinds = world
        .get_resource::<SpatialContainerKindRegistry>()
        .map(|r| {
            let mut v: Vec<String> = r.kinds.iter().map(|k| k.as_str().to_string()).collect();
            v.sort();
            v
        })
        .unwrap_or_default();
    Ok(Value::Array(kinds.into_iter().map(Value::String).collect()))
}

#[cfg(feature = "model-api")]
fn handle_set_toolbar_layout(
    world: &mut World,
    updates: Vec<ToolbarLayoutUpdate>,
) -> Result<Vec<ToolbarDetails>, String> {
    let Some(registry) = world.get_resource::<ToolbarRegistry>().cloned() else {
        return Err("Toolbar registry is unavailable".to_string());
    };
    if world.get_resource::<ToolbarLayoutState>().is_none() {
        return Err("Toolbar layout state is unavailable".to_string());
    }

    world.resource_scope(|world, mut layout_state: Mut<ToolbarLayoutState>| {
        let mut doc_props = world.resource_mut::<DocumentProperties>();
        for update in &updates {
            if !registry
                .toolbars()
                .any(|descriptor| descriptor.id == update.toolbar_id)
            {
                return Err(format!("Unknown toolbar: {}", update.toolbar_id));
            }
            if update.toolbar_id == "core" && update.visible == Some(false) {
                return Err("The core toolbar cannot be hidden".to_string());
            }
            let dock = update.dock.as_deref().map(parse_toolbar_dock).transpose()?;
            update_toolbar_layout_entry(
                &mut layout_state,
                &mut doc_props,
                &update.toolbar_id,
                dock,
                update.order,
                update.visible,
            )?;
        }
        Ok::<(), String>(())
    })?;

    let layout_state = world.resource::<ToolbarLayoutState>();
    Ok(toolbar_details_from_resources(&registry, layout_state))
}

#[cfg(feature = "model-api")]
fn handle_set_document_properties(
    world: &mut World,
    partial: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let current_json = serde_json::to_value(world.resource::<DocumentProperties>().clone())
        .map_err(|e| e.to_string())?;
    let merged = merge_json(current_json, partial);
    let updated: DocumentProperties = serde_json::from_value(merged).map_err(|e| e.to_string())?;
    world.insert_resource(updated.clone());
    serde_json::to_value(updated).map_err(|e| e.to_string())
}

#[cfg(feature = "model-api")]
fn parse_toolbar_dock(value: &str) -> Result<ToolbarDock, String> {
    ToolbarDock::from_str(value).ok_or_else(|| {
        format!("Invalid toolbar dock: {value}. Expected one of top, bottom, left, right, floating")
    })
}

#[cfg(feature = "model-api")]
fn merge_json(base: serde_json::Value, patch: serde_json::Value) -> serde_json::Value {
    match (base, patch) {
        (serde_json::Value::Object(mut base_map), serde_json::Value::Object(patch_map)) => {
            for (key, patch_value) in patch_map {
                let base_value = base_map.remove(&key).unwrap_or(serde_json::Value::Null);
                base_map.insert(key, merge_json(base_value, patch_value));
            }
            serde_json::Value::Object(base_map)
        }
        (_, patch) => patch,
    }
}

#[cfg(feature = "model-api")]
fn capture_snapshots_by_ids(
    world: &World,
    element_ids: &[u64],
) -> ApiResult<Vec<(ElementId, BoxedEntity)>> {
    if element_ids.is_empty() {
        return Err("No entities found for the given IDs".to_string());
    }

    let selected_ids = element_ids
        .iter()
        .copied()
        .map(ElementId)
        .collect::<std::collections::HashSet<_>>();

    let snapshots = element_ids
        .iter()
        .map(|element_id| {
            let element_id = ElementId(*element_id);
            let snapshot = capture_snapshot_by_id(world, element_id)?;
            Ok((element_id, snapshot))
        })
        .collect::<ApiResult<Vec<_>>>()?;

    Ok(snapshots
        .into_iter()
        .filter(|(_, snapshot)| {
            snapshot
                .transform_parent()
                .map(|parent_id| !selected_ids.contains(&parent_id))
                .unwrap_or(true)
        })
        .collect())
}

#[cfg(feature = "model-api")]
fn capture_snapshot_by_id(world: &World, element_id: ElementId) -> ApiResult<BoxedEntity> {
    capture_entity_snapshot(world, element_id)
        .ok_or_else(|| format!("Entity not found: {}", element_id.0))
}

#[cfg(feature = "model-api")]
fn ensure_entity_exists(world: &World, element_id: ElementId) -> ApiResult<()> {
    if capture_entity_snapshot(world, element_id).is_some() {
        Ok(())
    } else {
        Err(format!("Entity not found: {}", element_id.0))
    }
}

#[cfg(feature = "model-api")]
fn ensure_user_editable_entity(
    world: &World,
    element_id: ElementId,
    operation: &str,
) -> ApiResult<()> {
    ensure_entity_exists(world, element_id)?;
    let Some(entity) = find_entity_by_element_id_readonly(world, element_id) else {
        return Err(format!("Entity not found: {}", element_id.0));
    };
    if world
        .resource::<CapabilityRegistry>()
        .is_user_facing_entity(world, entity)
    {
        Ok(())
    } else {
        Err(format!(
            "Entity {} is an internal wall opening proxy and cannot be {operation}; edit the host wall or owning occurrence instead",
            element_id.0
        ))
    }
}

// PP70/PP71/PP72 refinement handlers operate on any entity carrying an
// `ElementId`, whether or not a factory is registered for it (e.g. foundation
// entities registered as descriptor-driven recipes without a bespoke factory).
#[cfg(feature = "model-api")]
fn ensure_refinable_entity_exists(world: &World, element_id: ElementId) -> ApiResult<()> {
    let mut q = world.try_query::<&ElementId>().unwrap();
    if q.iter(world).any(|id| *id == element_id) {
        Ok(())
    } else {
        Err(format!("Entity not found: {}", element_id.0))
    }
}

#[cfg(feature = "model-api")]
fn apply_transform_request(
    snapshots: &[(ElementId, BoxedEntity)],
    request: &TransformToolRequest,
) -> ApiResult<Vec<BoxedEntity>> {
    let axis = parse_axis(request.axis.as_deref())?;
    match request.operation.to_ascii_lowercase().as_str() {
        "move" => {
            let delta = if let Some(axis) = axis {
                axis.unit_vector() * scalar_from_value(&request.value)?
            } else {
                vec3_from_value(&request.value)?
            };
            Ok(snapshots
                .iter()
                .map(|(_, snapshot)| snapshot.translate_by(delta))
                .collect())
        }
        "rotate" => {
            let delta_radians = scalar_from_value(&request.value)?.to_radians();
            let rotation = match axis {
                Some(AxisName::X) => Quat::from_rotation_x(delta_radians),
                Some(AxisName::Z) => Quat::from_rotation_z(delta_radians),
                _ => Quat::from_rotation_y(delta_radians),
            };
            Ok(snapshots
                .iter()
                .map(|(_, snapshot)| snapshot.rotate_by(rotation))
                .collect())
        }
        "scale" => {
            let center = snapshots
                .iter()
                .map(|(_, snapshot)| snapshot.center())
                .fold(Vec3::ZERO, |sum, center| sum + center)
                / snapshots.len() as f32;
            let factor_value = scalar_from_value(&request.value)?;
            let factor = match axis {
                Some(AxisName::X) => Vec3::new(factor_value, 1.0, 1.0),
                Some(AxisName::Y) => Vec3::new(1.0, factor_value, 1.0),
                Some(AxisName::Z) => Vec3::new(1.0, 1.0, factor_value),
                None => Vec3::splat(factor_value),
            };
            Ok(snapshots
                .iter()
                .map(|(_, snapshot)| snapshot.scale_by(factor, center))
                .collect())
        }
        operation => Err(format!(
            "Invalid transform operation '{operation}'. Valid operations: move, rotate, scale"
        )),
    }
}

#[cfg(feature = "model-api")]
#[derive(Clone, Copy)]
enum AxisName {
    X,
    Y,
    Z,
}

#[cfg(feature = "model-api")]
impl AxisName {
    fn unit_vector(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }
}

#[cfg(feature = "model-api")]
fn parse_axis(axis: Option<&str>) -> ApiResult<Option<AxisName>> {
    match axis.map(|axis| axis.to_ascii_uppercase()) {
        None => Ok(None),
        Some(axis) if axis == "X" => Ok(Some(AxisName::X)),
        Some(axis) if axis == "Y" => Ok(Some(AxisName::Y)),
        Some(axis) if axis == "Z" => Ok(Some(AxisName::Z)),
        Some(axis) => Err(format!("Invalid axis '{axis}'. Valid axes: X, Y, Z")),
    }
}

#[cfg(feature = "model-api")]
fn scalar_from_value(value: &Value) -> ApiResult<f32> {
    value
        .as_f64()
        .map(|value| value as f32)
        .ok_or_else(|| "Expected a numeric value".to_string())
}

#[cfg(feature = "model-api")]
fn vec3_from_value(value: &Value) -> ApiResult<Vec3> {
    if let Some(array) = value.as_array() {
        if array.len() == 3 {
            return Ok(Vec3::new(
                scalar_from_value(&array[0])?,
                scalar_from_value(&array[1])?,
                scalar_from_value(&array[2])?,
            ));
        }
    }
    if let Some(object) = value.as_object() {
        return Ok(Vec3::new(
            required_f32(object, "x")?,
            required_f32(object, "y")?,
            required_f32(object, "z")?,
        ));
    }
    Err("Expected a Vec3 as [x, y, z] or {\"x\": ..., \"y\": ..., \"z\": ...}".to_string())
}

#[cfg(feature = "model-api")]
fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> ApiResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing or invalid string field '{field}'"))
}

#[cfg(feature = "model-api")]
fn required_f32(object: &serde_json::Map<String, Value>, field: &str) -> ApiResult<f32> {
    object
        .get(field)
        .map(scalar_from_value)
        .transpose()?
        .ok_or_else(|| format!("Missing or invalid numeric field '{field}'"))
}

#[cfg(feature = "model-api")]
fn send_event<E: Message>(world: &mut World, event: E) {
    world.resource_mut::<Messages<E>>().write(event);
}

// ---------------------------------------------------------------------------
// Refinement handlers (PP70)
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_get_refinement_state(world: &World, element_id: u64) -> ApiResult<RefinementStateInfo> {
    use crate::plugins::refinement::RefinementStateComponent;

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;

    let state = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .find_map(|(entity_ref,)| {
                if entity_ref.get::<ElementId>().copied() != Some(eid) {
                    return None;
                }
                Some(
                    entity_ref
                        .get::<RefinementStateComponent>()
                        .map(|c| c.state)
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    };

    Ok(RefinementStateInfo {
        element_id,
        state: state.as_str().to_string(),
    })
}

#[cfg(feature = "model-api")]
fn handle_get_obligations(world: &World, element_id: u64) -> ApiResult<Vec<ObligationInfo>> {
    use crate::plugins::refinement::{ObligationSet, ObligationStatus};

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;

    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    let entry = q.iter(world).find_map(|(entity_ref,)| {
        if entity_ref.get::<ElementId>().copied() != Some(eid) {
            return None;
        }
        let set = entity_ref.get::<ObligationSet>()?;
        Some(
            set.entries
                .iter()
                .map(|o| ObligationInfo {
                    id: o.id.0.clone(),
                    role: o.role.0.clone(),
                    required_by_state: o.required_by_state.as_str().to_string(),
                    status: match &o.status {
                        ObligationStatus::Unresolved => "Unresolved".to_string(),
                        ObligationStatus::SatisfiedBy(id) => format!("SatisfiedBy:{id}"),
                        ObligationStatus::Deferred(reason) => format!("Deferred:{reason}"),
                        ObligationStatus::Waived(rationale) => format!("Waived:{rationale}"),
                    },
                })
                .collect::<Vec<_>>(),
        )
    });

    Ok(entry.unwrap_or_default())
}

#[cfg(feature = "model-api")]
fn handle_get_authoring_provenance(
    world: &World,
    element_id: u64,
) -> ApiResult<AuthoringProvenanceInfo> {
    use crate::plugins::refinement::{AuthoringMode, AuthoringProvenance};

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;

    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    let entry = q
        .iter(world)
        .find_map(|(entity_ref,)| {
            if entity_ref.get::<ElementId>().copied() != Some(eid) {
                return None;
            }
            let prov = entity_ref.get::<AuthoringProvenance>()?;
            let mode_str = match &prov.mode {
                AuthoringMode::Freeform => "Freeform".to_string(),
                AuthoringMode::ViaRecipe(id) => format!("ViaRecipe:{}", id.0),
                AuthoringMode::Imported(src) => format!("Imported:{}", src.0),
                AuthoringMode::Refined(parent_id) => format!("Refined:{parent_id}"),
            };
            Some(AuthoringProvenanceInfo {
                element_id,
                mode: mode_str,
                rationale: prov.rationale.clone(),
            })
        })
        .unwrap_or_else(|| AuthoringProvenanceInfo {
            element_id,
            mode: "Freeform".to_string(),
            rationale: None,
        });

    Ok(entry)
}

#[cfg(feature = "model-api")]
pub fn handle_get_claim_grounding(
    world: &World,
    element_id: u64,
    path_filter: Option<String>,
) -> ApiResult<Vec<ClaimGroundingEntry>> {
    use crate::capability_registry::{effective_promotion_critical_paths, ElementClassAssignment};
    use crate::plugins::refinement::{ClaimGrounding, RefinementStateComponent};

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;

    // Compute the effective promotion-critical paths for this entity (PP71).
    // If no class assignment exists, falls back to an empty set (PP70 behaviour).
    let critical_paths: std::collections::HashSet<String> = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .find_map(|(entity_ref,)| {
                if entity_ref.get::<ElementId>().copied() != Some(eid) {
                    return None;
                }
                let assignment = entity_ref.get::<ElementClassAssignment>()?;
                let state = entity_ref
                    .get::<RefinementStateComponent>()
                    .map(|c| c.state)
                    .unwrap_or_default();
                let registry = world.get_resource::<CapabilityRegistry>()?;
                let class_desc = registry.element_class_descriptor(&assignment.element_class)?;
                let recipe_desc = assignment
                    .active_recipe
                    .as_ref()
                    .and_then(|rid| registry.recipe_family_descriptor(rid));
                let paths = effective_promotion_critical_paths(class_desc, recipe_desc, state);
                Some(paths.into_iter().map(|p| p.0).collect())
            })
            .unwrap_or_default()
    };

    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    let entries = q
        .iter(world)
        .find_map(|(entity_ref,)| {
            if entity_ref.get::<ElementId>().copied() != Some(eid) {
                return None;
            }
            let grounding = entity_ref.get::<ClaimGrounding>()?;
            let iter = grounding.claims.iter().filter_map(|(path, record)| {
                if let Some(ref filter) = path_filter {
                    if &path.0 != filter {
                        return None;
                    }
                }
                let grounding_json = serde_json::to_value(&record.grounding).unwrap_or_default();
                let is_promotion_critical = critical_paths.contains(&path.0);
                Some(ClaimGroundingEntry {
                    path: path.0.clone(),
                    grounding: grounding_json,
                    set_at: record.set_at,
                    set_by: record.set_by.as_ref().map(|a| a.0.clone()),
                    is_promotion_critical,
                })
            });
            Some(iter.collect::<Vec<_>>())
        })
        .unwrap_or_default();

    Ok(entries)
}

#[cfg(feature = "model-api")]
pub fn handle_promote_refinement(
    world: &mut World,
    element_id: u64,
    target_state_str: String,
    recipe_id: Option<String>,
    overrides: serde_json::Value,
) -> ApiResult<PromoteRefinementResult> {
    use crate::plugins::refinement::{
        apply_promote_refinement, ClaimPath, PromoteRefinementRequest, RecipeId, RefinementState,
        RefinementStateComponent,
    };

    let target_state = RefinementState::from_str(&target_state_str)
        .ok_or_else(|| format!("Unknown refinement state: '{target_state_str}'"))?;

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;

    if let Some(recipe_id) = recipe_id.as_deref() {
        if world
            .get_resource::<crate::plugins::recipe_drafts::RecipeDraftRegistry>()
            .and_then(|registry| registry.get(recipe_id))
            .is_some()
        {
            return Err(format!(
                "recipe draft '{recipe_id}' is consultable but not executable yet; use list_recipe_drafts/get_recipe_draft to inspect it"
            ));
        }
    }

    // Capture current state before promote.
    let previous_state = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .find_map(|(entity_ref,)| {
                if entity_ref.get::<ElementId>().copied() != Some(eid) {
                    return None;
                }
                Some(
                    entity_ref
                        .get::<RefinementStateComponent>()
                        .map(|c| c.state)
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    };

    let overrides_map: std::collections::HashMap<ClaimPath, serde_json::Value> = overrides
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (ClaimPath(k.clone()), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    let request = PromoteRefinementRequest {
        entity_element_id: element_id,
        target_state,
        recipe_id: recipe_id.map(RecipeId),
        overrides: overrides_map,
    };

    let new_state = apply_promote_refinement(world, request)?;
    flush_model_api_write_pipeline(world);

    Ok(PromoteRefinementResult {
        element_id,
        previous_state: previous_state.as_str().to_string(),
        new_state: new_state.as_str().to_string(),
    })
}

/// `instantiate_recipe`: one-call path from a discovered recipe family to placed
/// geometry. Creates a coarse semantic root of `target_class` (the identity
/// anchor) carrying the recipe parameters, then runs the recipe's `generate`
/// function via the same promote path, returning the root plus every element the
/// recipe created. Thin wrapper — no generation logic is duplicated here.
#[cfg(feature = "model-api")]
fn handle_instantiate_recipe(
    world: &mut World,
    request: InstantiateRecipeRequest,
) -> ApiResult<InstantiateRecipeResult> {
    let target_state = request
        .target_state
        .clone()
        .unwrap_or_else(|| "Constructible".to_string());
    let placement = request
        .placement
        .clone()
        .unwrap_or(InstantiateRecipePlacement {
            translate: [0.0, 0.0, 0.0],
            rotate_euler_deg: [0.0, 0.0, 0.0],
        });

    let collect_ids = |world: &mut World| -> std::collections::HashSet<u64> {
        let mut q = world.try_query::<(&ElementId,)>().unwrap();
        q.iter(world).map(|(id,)| id.0).collect()
    };
    let before = collect_ids(world);

    // 1. Create the coarse identity-anchor root carrying the element class +
    //    recipe parameters, at the requested placement (metres). The recipe
    //    generates the real sub-element geometry on promotion.
    // Size the coarse identity-anchor box so the recipe generates geometry at
    // the intended scale (recipes derive sub-element geometry from the root's
    // extent). Use length_mm/height_mm/thickness_mm when present (mm→m), else a
    // footprint_polygon's bounding box, else a 1 m fallback cube.
    let mm = |key: &str| request.parameters.get(key).and_then(|v| v.as_f64());
    let half_extents: [f64; 3] = if let (Some(l), Some(h)) = (mm("length_mm"), mm("height_mm")) {
        let t = mm("thickness_mm").unwrap_or(200.0);
        [l / 2000.0, h / 2000.0, t / 2000.0]
    } else if let Some(poly) = request
        .parameters
        .get("footprint_polygon")
        .and_then(|v| v.as_array())
    {
        let pts: Vec<(f64, f64)> = poly
            .iter()
            .filter_map(|p| p.as_array())
            .filter_map(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)))
            .collect();
        if pts.len() >= 3 {
            let (mut xmin, mut xmax, mut zmin, mut zmax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
            for (x, z) in &pts {
                xmin = xmin.min(*x);
                xmax = xmax.max(*x);
                zmin = zmin.min(*z);
                zmax = zmax.max(*z);
            }
            // Footprint coordinates are world units (metres).
            [
                ((xmax - xmin) / 2.0).max(0.1),
                0.1,
                ((zmax - zmin) / 2.0).max(0.1),
            ]
        } else {
            [0.5, 0.5, 0.5]
        }
    } else {
        [0.5, 0.5, 0.5]
    };
    // Orientation: the recipe lays out its sub-elements in the host's local
    // frame, so rotating the host box rotates the whole generated assembly
    // coherently. Convert the requested Euler angles (degrees, XYZ) to the
    // box rotation quaternion `[x, y, z, w]`.
    let r = &placement.rotate_euler_deg;
    let rotation = Quat::from_euler(
        EulerRot::XYZ,
        (r[0] as f32).to_radians(),
        (r[1] as f32).to_radians(),
        (r[2] as f32).to_radians(),
    )
    .to_array();
    // The box factory's raw create request uses `centre` + `half_extents`
    // (the create_box tool normalises `center`/`size`, but handle_create_entity
    // bypasses that normalisation).
    let create_json = serde_json::json!({
        "type": "box",
        "centre": placement.translate,
        "half_extents": half_extents,
        "rotation": rotation,
        "semantic": {
            "element_class": request.target_class,
            "refinement_state": "Schematic",
            "parameters": request.parameters,
        }
    });
    let root = handle_create_entity(world, create_json)?;

    // 2. Run the curated recipe via the existing promote path; recipe parameters
    //    are passed as overrides so the generate fn can read them.
    let promote = handle_promote_refinement(
        world,
        root,
        target_state,
        Some(request.family_id.clone()),
        request.parameters.clone(),
    )?;

    // 3. Created ids = everything new since the snapshot, excluding the root.
    let after = collect_ids(world);
    let mut created_element_ids: Vec<u64> = after
        .difference(&before)
        .copied()
        .filter(|id| *id != root)
        .collect();
    created_element_ids.sort_unstable();

    Ok(InstantiateRecipeResult {
        root_element_id: root,
        created_element_ids,
        state: promote.new_state,
    })
}

#[cfg(feature = "model-api")]
fn handle_demote_refinement(
    world: &mut World,
    element_id: u64,
    target_state_str: String,
) -> ApiResult<DemoteRefinementResult> {
    use crate::plugins::refinement::{
        apply_demote_refinement, DemoteRefinementRequest, RefinementState, RefinementStateComponent,
    };

    let target_state = RefinementState::from_str(&target_state_str)
        .ok_or_else(|| format!("Unknown refinement state: '{target_state_str}'"))?;

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;

    let previous_state = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .find_map(|(entity_ref,)| {
                if entity_ref.get::<ElementId>().copied() != Some(eid) {
                    return None;
                }
                Some(
                    entity_ref
                        .get::<RefinementStateComponent>()
                        .map(|c| c.state)
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    };

    let request = DemoteRefinementRequest {
        entity_element_id: element_id,
        target_state,
    };

    let new_state = apply_demote_refinement(world, request)?;
    flush_model_api_write_pipeline(world);

    Ok(DemoteRefinementResult {
        element_id,
        previous_state: previous_state.as_str().to_string(),
        new_state: new_state.as_str().to_string(),
    })
}

#[cfg(feature = "model-api")]
fn handle_inspect_refinement_branches(
    world: &World,
    element_id: u64,
) -> ApiResult<Vec<RefinementBranchApiInfo>> {
    use crate::plugins::refinement::list_refinement_branches;

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;
    Ok(list_refinement_branches(world, eid)
        .into_iter()
        .map(|branch| RefinementBranchApiInfo {
            root_element_id: branch.root_element_id,
            parent_element_id: branch.parent_element_id,
            child_element_id: branch.child_element_id,
            target_state: branch.target_state.as_str().to_string(),
            recipe_id: branch.recipe_id.map(|recipe_id| recipe_id.0),
            status: branch.status.as_str().to_string(),
        })
        .collect())
}

#[cfg(feature = "model-api")]
fn handle_discard_refinement_branch(
    world: &mut World,
    parent_element_id: u64,
    child_element_id: u64,
) -> ApiResult<DiscardRefinementBranchResult> {
    use crate::plugins::refinement::discard_refinement_branch;

    ensure_refinable_entity_exists(world, ElementId(parent_element_id))?;
    ensure_refinable_entity_exists(world, ElementId(child_element_id))?;
    let discarded_element_ids = discard_refinement_branch(
        world,
        ElementId(parent_element_id),
        ElementId(child_element_id),
    )?;
    flush_model_api_write_pipeline(world);
    Ok(DiscardRefinementBranchResult {
        parent_element_id,
        child_element_id,
        discarded_element_ids,
    })
}

#[cfg(feature = "model-api")]
pub fn handle_run_validation(
    world: &World,
    element_id: u64,
) -> ApiResult<Vec<ValidationFindingInfo>> {
    use crate::plugins::refinement::{
        validate_declared_state_obligations, ObligationSet, RefinementStateComponent,
    };

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;
    if crate::plugins::refinement::is_parked_refinement_entity(world, eid) {
        return Ok(Vec::new());
    }

    let (state, obligations) = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .find_map(|(entity_ref,)| {
                if entity_ref.get::<ElementId>().copied() != Some(eid) {
                    return None;
                }
                let state = entity_ref
                    .get::<RefinementStateComponent>()
                    .map(|c| c.state)
                    .unwrap_or_default();
                let obligations = entity_ref
                    .get::<ObligationSet>()
                    .cloned()
                    .unwrap_or_default();
                Some((state, obligations))
            })
            .unwrap_or_default()
    };

    let mut infos: Vec<ValidationFindingInfo> =
        validate_declared_state_obligations(element_id, state, &obligations)
            .into_iter()
            .map(|f| ValidationFindingInfo {
                finding_id: f.finding_id,
                entity_element_id: f.entity_element_id,
                validator: f.validator,
                severity: f.severity.as_str().to_string(),
                message: f.message,
                rationale: f.rationale,
                obligation_id: f.obligation_id.map(|id| id.0),
            })
            .collect();

    // PP74: also dispatch to every registered ConstraintDescriptor whose
    // applicability matches this entity. No dirty-propagation cache yet —
    // validators are invoked directly each call. A system-scheduled cache
    // with change-detection lives behind `validation_sweep_system` and is
    // a follow-on from here.
    use crate::capability_registry::{CapabilityRegistry, ElementClassAssignment};
    if let Some(registry) = world.get_resource::<CapabilityRegistry>() {
        let mut q = world
            .try_query::<(bevy::prelude::Entity, &ElementId)>()
            .unwrap();
        if let Some(entity) = q
            .iter(world)
            .find_map(|(e, id)| if *id == eid { Some(e) } else { None })
        {
            let entity_class = world
                .get::<ElementClassAssignment>(entity)
                .map(|a| a.element_class.clone());
            let entity_state = world
                .get::<RefinementStateComponent>(entity)
                .map(|c| c.state)
                .unwrap_or_default();

            for descriptor in registry.constraint_descriptors() {
                if !descriptor.applicability.element_classes.is_empty() {
                    let matches = entity_class
                        .as_ref()
                        .map(|c| descriptor.applicability.element_classes.contains(c))
                        .unwrap_or(false);
                    if !matches {
                        continue;
                    }
                }
                if let Some(required) = descriptor.applicability.required_state {
                    if entity_state < required {
                        continue;
                    }
                }
                let raw_findings = (descriptor.validator)(entity, world);
                for f in raw_findings {
                    infos.push(ValidationFindingInfo {
                        finding_id: f.id.0,
                        entity_element_id: f.subject,
                        validator: f.constraint_id.0,
                        severity: f.severity.as_str().to_string(),
                        message: f.message,
                        rationale: f.rationale,
                        obligation_id: None,
                    });
                }
            }
        }
    }

    Ok(infos)
}

#[cfg(feature = "model-api")]
fn handle_explain_finding(_world: &World, finding_id: String) -> ApiResult<serde_json::Value> {
    // PP70 scaffold: derive rationale from the finding_id prefix.
    // The richer per-finding lookup with caching lands in PP74.
    if finding_id.starts_with("declared_state_obligations:") {
        Ok(serde_json::json!({
            "finding_id": finding_id,
            "validator": "DeclaredStateRequiresResolvedObligations",
            "rationale": "Entities at Schematic state must have primary-structure obligations \
                          resolved or at least flagged. At Constructible or higher, all \
                          obligations must be in a terminal status (SatisfiedBy, Deferred, \
                          or Waived). This ensures that design intent is captured before \
                          geometry generation proceeds.",
            "source": "ADR-038 §11 / PP70 acceptance criteria"
        }))
    } else {
        Err(format!(
            "Unknown finding_id '{finding_id}'. Only findings produced by run_validation are known to this endpoint."
        ))
    }
}

// ---------------------------------------------------------------------------
// Descriptor discovery handlers (PP71)
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
pub fn handle_list_element_classes(world: &World) -> Vec<ElementClassInfo> {
    use crate::capability_registry::CapabilityRegistry;
    use crate::plugins::refinement::RefinementState;
    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return Vec::new();
    };
    // Iterate refinement states in monotonic order so the served ladder is
    // ordered and deterministic (the descriptor stores them in HashMaps).
    const STATES: [RefinementState; 5] = [
        RefinementState::Conceptual,
        RefinementState::Schematic,
        RefinementState::Constructible,
        RefinementState::Detailed,
        RefinementState::FabricationReady,
    ];
    registry
        .element_class_descriptors()
        .iter()
        .map(|d| {
            let mut obligations_by_state = Vec::new();
            for state in STATES {
                let obligations: Vec<ClassObligationTemplateInfo> = d
                    .class_min_obligations
                    .get(&state)
                    .map(|templates| {
                        templates
                            .iter()
                            .map(|t| ClassObligationTemplateInfo {
                                id: t.id.0.clone(),
                                role: t.role.0.clone(),
                                required_by_state: t.required_by_state.as_str().to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let promotion_critical_paths: Vec<String> = d
                    .class_min_promotion_critical_paths
                    .get(&state)
                    .map(|paths| paths.iter().map(|p| p.0.clone()).collect())
                    .unwrap_or_default();
                if obligations.is_empty() && promotion_critical_paths.is_empty() {
                    continue;
                }
                obligations_by_state.push(ClassStateObligationsInfo {
                    refinement_state: state.as_str().to_string(),
                    obligations,
                    promotion_critical_paths,
                });
            }
            ElementClassInfo {
                id: d.id.0.clone(),
                label: d.label.clone(),
                description: d.description.clone(),
                semantic_roles: d.semantic_roles.iter().map(|r| r.0.clone()).collect(),
                obligations_by_state,
            }
        })
        .collect()
}

#[cfg(feature = "model-api")]
pub fn handle_get_capability_snapshot(world: &World, expanded: bool) -> CapabilitySnapshotInfo {
    use crate::capability_registry::CapabilityRegistry;
    use crate::curation::{CuratedManifestRegistry, MaterialSpecRegistry, SourceRegistry};
    use crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftRegistry;
    use crate::plugins::corpus_gap::CorpusGapQueue;
    use crate::plugins::recipe_drafts::RecipeDraftRegistry;
    use crate::relational::registry::ParametricRegistry;

    let id_limit = if expanded { usize::MAX } else { 12 };
    let gap_limit = if expanded { usize::MAX } else { 8 };
    let guidance_limit: usize = 5;

    let take_ids = |mut ids: Vec<String>| {
        ids.sort();
        ids.truncate(id_limit);
        ids
    };

    let element_class_ids = world
        .get_resource::<CapabilityRegistry>()
        .map(|registry| {
            registry
                .element_class_descriptors()
                .iter()
                .map(|d| d.id.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let recipe_family_ids = world
        .get_resource::<CapabilityRegistry>()
        .map(|registry| {
            registry
                .recipe_family_descriptors(None)
                .into_iter()
                .map(|d| d.id.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let session_recipe_draft_ids = world
        .get_resource::<RecipeDraftRegistry>()
        .map(|registry| {
            registry
                .snapshot()
                .into_iter()
                .map(|draft| draft.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let assembly_pattern_draft_ids = world
        .get_resource::<AssemblyPatternDraftRegistry>()
        .map(|registry| {
            registry
                .snapshot()
                .into_iter()
                .map(|draft| draft.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let parametric_type_ids = world
        .get_resource::<ParametricRegistry>()
        .map(|registry| {
            registry
                .list_public()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let catalog_provider_ids = world
        .get_resource::<CapabilityRegistry>()
        .map(|registry| {
            registry
                .catalog_provider_descriptors()
                .iter()
                .map(|d| d.id.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let generation_prior_ids = world
        .get_resource::<CapabilityRegistry>()
        .map(|registry| {
            registry
                .generation_prior_descriptors(None)
                .into_iter()
                .map(|d| d.id.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let constraint_ids = world
        .get_resource::<CapabilityRegistry>()
        .map(|registry| {
            registry
                .constraint_descriptors()
                .iter()
                .map(|d| d.id.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let corpus_gap_ids = world
        .get_resource::<CorpusGapQueue>()
        .map(|queue| {
            queue
                .list()
                .iter()
                .map(|gap| gap.id.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let source_ids = world
        .get_resource::<SourceRegistry>()
        .map(|registry| {
            registry
                .iter()
                .map(|entry| format!("{}@{}", entry.source_id.as_str(), entry.revision.as_str()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let curated_manifest_ids = world
        .get_resource::<CuratedManifestRegistry>()
        .map(|registry| {
            registry
                .iter()
                .map(|manifest| manifest.meta.id.as_str().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let material_spec_ids = world
        .get_resource::<MaterialSpecRegistry>()
        .map(|registry| {
            registry
                .iter()
                .map(|spec| spec.meta.id.as_str().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut must_read_guidance_card_ids = Vec::new();
    let mut guidance_overrides = Vec::new();
    must_read_guidance_card_ids.push("dkg.snapshot.start".into());
    must_read_guidance_card_ids.push("dkg.no_curated_path".into());
    if let Some(guidance) = world.get_resource::<AuthoringGuidance>() {
        if !guidance.is_empty() {
            let id = format!("authoring_guidance:{}", guidance.guidance_id);
            must_read_guidance_card_ids.push(id.clone());
            guidance_overrides.push(CapabilitySnapshotFact {
                classification: "guidance_override".into(),
                id,
                summary: format!(
                    "Active authoring guidance version {} is prompt-text authoritative.",
                    guidance.version
                ),
            });
            for reference in guidance
                .references
                .iter()
                .take(guidance_limit.saturating_sub(1))
            {
                must_read_guidance_card_ids
                    .push(format!("{}:{}", reference.kind, reference.target));
            }
        }
    }
    must_read_guidance_card_ids.truncate(guidance_limit);

    let mut no_curated_paths = Vec::new();
    if let Some(registry) = world.get_resource::<CapabilityRegistry>() {
        for class in registry.element_class_descriptors() {
            let recipe_count = registry.recipe_family_descriptors(Some(&class.id)).len();
            if recipe_count == 0 {
                no_curated_paths.push(NoCuratedPathInfo {
                    element_class: class.id.0.clone(),
                    missing_artifact_kind: "recipe_or_executable_asset".into(),
                    suggested_next_tool: "request_corpus_expansion".into(),
                    guidance_card_ids: must_read_guidance_card_ids.clone(),
                    related_installed_or_learned_asset_ids: session_recipe_draft_ids
                        .iter()
                        .chain(assembly_pattern_draft_ids.iter())
                        .take(if expanded { 16 } else { 4 })
                        .cloned()
                        .collect(),
                });
            }
        }
    }
    no_curated_paths.sort_by(|a, b| a.element_class.cmp(&b.element_class));
    let no_curated_path_count = no_curated_paths.len();
    no_curated_paths.truncate(gap_limit);

    let summary = CapabilitySnapshotSummary {
        element_class_count: element_class_ids.len(),
        recipe_family_count: recipe_family_ids.len(),
        session_recipe_draft_count: session_recipe_draft_ids.len(),
        assembly_pattern_draft_count: assembly_pattern_draft_ids.len(),
        parametric_type_count: parametric_type_ids.len(),
        catalog_provider_count: catalog_provider_ids.len(),
        generation_prior_count: generation_prior_ids.len(),
        constraint_count: constraint_ids.len(),
        corpus_gap_count: corpus_gap_ids.len(),
        source_count: source_ids.len(),
        curated_manifest_count: curated_manifest_ids.len(),
        material_spec_count: material_spec_ids.len(),
        no_curated_path_count,
    };

    let computed = CapabilitySnapshotComputed {
        element_class_ids: take_ids(element_class_ids),
        recipe_family_ids: take_ids(recipe_family_ids),
        session_recipe_draft_ids: take_ids(session_recipe_draft_ids),
        assembly_pattern_draft_ids: take_ids(assembly_pattern_draft_ids),
        parametric_type_ids: take_ids(parametric_type_ids),
        catalog_provider_ids: take_ids(catalog_provider_ids),
        generation_prior_ids: take_ids(generation_prior_ids),
        constraint_ids: take_ids(constraint_ids),
        corpus_gap_ids: take_ids(corpus_gap_ids),
        source_ids: take_ids(source_ids),
        curated_manifest_ids: take_ids(curated_manifest_ids),
        material_spec_ids: take_ids(material_spec_ids),
        maturity_flags: vec![
            "computed:registry_counts".into(),
            "computed:no_curated_path_summaries".into(),
            "guidance_override:does_not_unlock_promotion".into(),
            "evidence_backed:empty_until_runtime_evidence_refs_land".into(),
        ],
    };

    let mut snapshot = CapabilitySnapshotInfo {
        snapshot_version: 1,
        expanded,
        size_budget_bytes: 12 * 1024,
        estimated_json_bytes: 0,
        summary,
        computed,
        evidence_backed: Vec::new(),
        guidance_overrides,
        no_curated_paths,
        must_read_guidance_card_ids,
        next_tools: vec![
            "list_element_classes".into(),
            "discover_curated_paths".into(),
            "select_recipe".into(),
            "parametric.list_types".into(),
            "list_corpus_gaps".into(),
            "request_corpus_expansion".into(),
            "list_guidance_cards".into(),
            "get_guidance_card".into(),
            "get_authoring_guidance".into(),
        ],
    };
    snapshot.estimated_json_bytes = serde_json::to_vec(&snapshot)
        .map(|bytes| bytes.len())
        .unwrap_or_default();
    snapshot
}

#[cfg(feature = "model-api")]
pub fn handle_list_recipe_families(
    world: &World,
    element_class: Option<String>,
) -> Vec<RecipeFamilyInfo> {
    handle_list_recipe_families_with_options(world, element_class, false)
}

#[cfg(feature = "model-api")]
pub fn handle_list_recipe_families_with_options(
    world: &World,
    element_class: Option<String>,
    include_session_drafts: bool,
) -> Vec<RecipeFamilyInfo> {
    use crate::capability_registry::{CapabilityRegistry, ElementClassId};
    use crate::plugins::recipe_drafts::{RecipeDraftRegistry, RecipeDraftStatus};

    let filter = element_class
        .as_deref()
        .map(|s| ElementClassId(s.to_string()));

    let mut families: Vec<RecipeFamilyInfo> = world
        .get_resource::<CapabilityRegistry>()
        .map(|registry| {
            registry
                .recipe_family_descriptors(filter.as_ref())
                .into_iter()
                .map(|d| RecipeFamilyInfo {
                    id: d.id.0.clone(),
                    target_class: d.target_class.0.clone(),
                    label: d.label.clone(),
                    description: d.description.clone(),
                    supported_refinement_levels: d
                        .supported_refinement_levels
                        .iter()
                        .map(|s| s.as_str().to_string())
                        .collect(),
                    parameters: d
                        .parameters
                        .iter()
                        .map(|p| RecipeParameterInfo {
                            name: p.name.clone(),
                            value_schema: p.value_schema.clone(),
                            default: p.default.clone(),
                        })
                        .collect(),
                    is_session_draft: false,
                })
                .collect()
        })
        .unwrap_or_default();

    if include_session_drafts {
        if let Some(registry) = world.get_resource::<RecipeDraftRegistry>() {
            families.extend(
                registry
                    .list(element_class.as_deref(), Some(RecipeDraftStatus::Installed))
                    .into_iter()
                    .map(|draft| RecipeFamilyInfo {
                        id: draft.id,
                        target_class: draft.target_class,
                        label: draft.label,
                        description: draft.description,
                        supported_refinement_levels: draft.supported_refinement_levels,
                        parameters: draft
                            .parameters
                            .into_iter()
                            .map(|parameter| RecipeParameterInfo {
                                name: parameter.name,
                                value_schema: parameter.value_schema,
                                default: parameter.default,
                            })
                            .collect(),
                        is_session_draft: true,
                    }),
            );
        }
    }

    families
}

#[cfg(feature = "model-api")]
pub fn handle_select_recipe(
    world: &World,
    element_class: String,
    context: serde_json::Value,
) -> ApiResult<Vec<RecipeRankingInfo>> {
    use crate::capability_registry::{CapabilityRegistry, ElementClassId};
    use crate::plugins::recipe_drafts::RecipeDraftRegistry;
    use crate::plugins::refinement::RefinementState;

    let include_session_drafts = context
        .get("include_session_drafts")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let registry = world.get_resource::<CapabilityRegistry>();
    if registry.is_none() && !include_session_drafts {
        return Err("CapabilityRegistry not found".to_string());
    }

    let class_id = ElementClassId(element_class.clone());

    // Optionally filter by target_state from the context object.
    let target_state: Option<RefinementState> = context
        .get("target_state")
        .and_then(|v| v.as_str())
        .and_then(RefinementState::from_str);

    // PP76: consult registered GenerationPriorDescriptors scoped to
    // RecipeSelection for this element class.  Each prior is called with the
    // evaluation context derived from the request JSON; the resulting weights
    // are multiplied together (all priors must agree).  If no priors match,
    // weight defaults to 1.0 (neutral — original PP71 behaviour).
    //
    // The terrain_slope_foundation prior registered by talos3d-architecture-core
    // reproduces the behaviour previously hard-coded here (the PP72 TODO stub).
    use crate::capability_registry::{PriorContext, PriorScope};

    let prior_context = PriorContext::from_json(&context);
    let recipe_selection_priors: Vec<_> = registry
        .map(|registry| {
            registry
                .generation_prior_descriptors(Some(&class_id))
                .into_iter()
                .filter(|d| matches!(&d.scope, PriorScope::RecipeSelection { .. }))
                .collect()
        })
        .unwrap_or_default();

    let mut viable: Vec<RecipeRankingInfo> = registry
        .map(|registry| {
            registry
                .recipe_family_descriptors(Some(&class_id))
                .into_iter()
                .filter(|d| {
                    // Viable = supports the requested state (or all states if no filter).
                    target_state.is_none_or(|ts| d.supported_refinement_levels.contains(&ts))
                })
                .filter_map(|d| {
                    // Evaluate all applicable priors for this recipe family.  A prior
                    // is applicable when its scope matches either "all families for the
                    // class" (recipe_family: None) or exactly this family.
                    let weight = recipe_selection_priors
                        .iter()
                        .filter(|p| match &p.scope {
                            PriorScope::RecipeSelection { recipe_family, .. } => {
                                recipe_family.is_none()
                                    || recipe_family.as_ref().map(|rf| rf.0.as_str())
                                        == Some(d.id.0.as_str())
                            }
                            _ => false,
                        })
                        .map(|p| (p.prior_fn)(&prior_context).weight)
                        .fold(1.0_f32, |acc, w| acc * w);
                    if weight <= 0.0 {
                        return None;
                    }
                    Some(RecipeRankingInfo {
                        how_to_instantiate: format!(
                            "Call instantiate_recipe {{ family_id: {:?}, target_class: {:?}, parameters: {{...}} }}",
                            d.id.0, d.target_class.0
                        ),
                        id: d.id.0.clone(),
                        target_class: d.target_class.0.clone(),
                        label: d.label.clone(),
                        weight,
                        is_session_draft: false,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if include_session_drafts {
        if let Some(registry) = world.get_resource::<RecipeDraftRegistry>() {
            viable.extend(
                registry
                    .installed_for_class(&element_class)
                    .into_iter()
                    .filter(|draft| {
                        target_state.is_none_or(|ts| {
                            draft
                                .supported_refinement_levels
                                .iter()
                                .any(|state| state == ts.as_str())
                        })
                    })
                    .map(|draft| RecipeRankingInfo {
                        how_to_instantiate: format!(
                            "Draft only — use inspect_recipe_draft {{ id: {:?} }} to review; not yet executable via instantiate_recipe",
                            draft.id
                        ),
                        id: draft.id,
                        target_class: draft.target_class,
                        label: draft.label,
                        // Installed drafts are consultable but intentionally rank below
                        // executable shipped recipes until invocation exists.
                        weight: 0.25,
                        is_session_draft: true,
                    }),
            );
        }
    }

    // Sort descending by weight so the highest-weight recipe comes first.
    viable.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(viable)
}

#[cfg(feature = "model-api")]
pub fn handle_discover_curated_paths(
    world: &World,
    request: CuratedPathDiscoveryRequest,
) -> ApiResult<CuratedPathDiscoveryInfo> {
    use crate::capability_registry::{CapabilityRegistry, ElementClassId, PriorScope};
    use crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftRegistry;
    use crate::plugins::recipe_drafts::RecipeDraftRegistry;
    use crate::relational::registry::ParametricRegistry;

    let path_kind = request.path_kind.unwrap_or_else(|| "recipe".into());
    let guidance_card_ids = vec![
        "dkg.snapshot.start".into(),
        "dkg.no_curated_path".into(),
        "dkg.close_gap".into(),
    ];
    let mut related_asset_ids = Vec::new();
    if let Some(registry) = world.get_resource::<RecipeDraftRegistry>() {
        related_asset_ids.extend(
            registry
                .snapshot()
                .into_iter()
                .map(|draft| draft.meta.id.as_str().to_string()),
        );
    }
    if let Some(registry) = world.get_resource::<AssemblyPatternDraftRegistry>() {
        related_asset_ids.extend(
            registry
                .snapshot()
                .into_iter()
                .map(|draft| draft.meta.id.as_str().to_string()),
        );
    }
    related_asset_ids.sort();
    related_asset_ids.truncate(12);

    let mut recipe_rankings = Vec::new();
    let mut parametric_types = Vec::new();
    let mut generation_priors = Vec::new();

    match path_kind.as_str() {
        "recipe" => {
            let element_class = request
                .element_class
                .clone()
                .ok_or_else(|| "recipe discovery requires element_class".to_string())?;
            recipe_rankings = handle_select_recipe(world, element_class, request.context)?;
        }
        "parametric" => {
            parametric_types = world
                .get_resource::<ParametricRegistry>()
                .map(|registry| {
                    registry
                        .list_public()
                        .into_iter()
                        .filter(|(id, label)| {
                            request
                                .element_class
                                .as_deref()
                                .is_none_or(|needle| id.contains(needle) || label.contains(needle))
                        })
                        .map(
                            |(id, label)| crate::plugins::parametric_mcp::ParametricTypeInfo {
                                id,
                                label,
                            },
                        )
                        .collect()
                })
                .unwrap_or_default();
        }
        "prior" => {
            let class_id = request
                .element_class
                .as_ref()
                .map(|class| ElementClassId(class.clone()));
            generation_priors = world
                .get_resource::<CapabilityRegistry>()
                .map(|registry| {
                    registry
                        .generation_prior_descriptors(class_id.as_ref())
                        .into_iter()
                        .filter(|prior| matches!(prior.scope, PriorScope::RecipeSelection { .. }))
                        .map(|prior| GenerationPriorInfo {
                            id: prior.id.0.clone(),
                            label: prior.label.clone(),
                            description: prior.description.clone(),
                            scope: serde_json::to_value(&prior.scope)
                                .unwrap_or(serde_json::Value::Null),
                            license: prior.source_provenance.license.as_str().to_string(),
                            source_version: prior.source_provenance.source_version.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        other => {
            return Err(format!(
                "unknown path_kind '{other}'; expected recipe, parametric, or prior"
            ));
        }
    }

    let has_path = !recipe_rankings.is_empty()
        || !parametric_types.is_empty()
        || !generation_priors.is_empty();
    let no_curated_path = (!has_path).then(|| NoCuratedPathInfo {
        element_class: request
            .element_class
            .clone()
            .unwrap_or_else(|| "<unspecified>".into()),
        missing_artifact_kind: format!("{path_kind}_curated_path"),
        suggested_next_tool: "request_corpus_expansion".into(),
        guidance_card_ids: guidance_card_ids.clone(),
        related_installed_or_learned_asset_ids: related_asset_ids.clone(),
    });

    Ok(CuratedPathDiscoveryInfo {
        path_kind,
        element_class: request.element_class,
        recipe_rankings,
        parametric_types,
        generation_priors,
        related_asset_ids,
        no_curated_path,
        suggested_next_tool: "request_corpus_expansion".into(),
        guidance_card_ids,
    })
}

#[cfg(feature = "model-api")]
pub fn handle_list_guidance_cards(_world: &World, task: Option<String>) -> Vec<GuidanceCardInfo> {
    let mut cards = dkc_guidance_cards();
    if let Some(task) = task {
        cards.retain(|card| {
            card.task_tags
                .iter()
                .any(|tag| tag.contains(&task) || task.contains(tag))
        });
    }
    cards
}

#[cfg(feature = "model-api")]
pub fn handle_get_guidance_card(_world: &World, card_id: String) -> ApiResult<GuidanceCardInfo> {
    dkc_guidance_cards()
        .into_iter()
        .find(|card| card.id == card_id)
        .ok_or_else(|| format!("guidance card not found: '{card_id}'"))
}

#[cfg(feature = "model-api")]
fn dkc_guidance_cards() -> Vec<GuidanceCardInfo> {
    vec![
        GuidanceCardInfo {
            id: "dkg.snapshot.start".into(),
            title: "Start With Capability Snapshot".into(),
            task_tags: vec!["discovery".into(), "authoring".into()],
            summary: "Call get_capability_snapshot before authoring so current registries, gaps, and must-read cards are visible.".into(),
            referenced_tool_ids: vec!["get_capability_snapshot".into()],
            next_card_ids: vec!["dkg.no_curated_path".into()],
            json_examples: vec![serde_json::json!({ "expanded": false })],
        },
        GuidanceCardInfo {
            id: "dkg.no_curated_path".into(),
            title: "No Curated Path Is A Gap".into(),
            task_tags: vec!["gap".into(), "anti_bluff".into()],
            summary: "Empty discovery is explicit NoCuratedPath; request corpus expansion instead of hand-rolling domain geometry.".into(),
            referenced_tool_ids: vec![
                "discover_curated_paths".into(),
                "request_corpus_expansion".into(),
            ],
            next_card_ids: vec!["dkg.close_gap".into()],
            json_examples: vec![serde_json::json!({
                "path_kind": "recipe",
                "element_class": "roof_system",
                "context": { "target_state": "Constructible" }
            })],
        },
        GuidanceCardInfo {
            id: "dkg.close_gap".into(),
            title: "Close The Gap As Knowledge".into(),
            task_tags: vec!["curation".into(), "draft".into()],
            summary: "Save acquired expertise as a curation-shaped draft with evidence slots and project scope before relying on it later.".into(),
            referenced_tool_ids: vec![
                "save_recipe_draft".into(),
                "save_assembly_pattern_draft".into(),
                "parametric.create".into(),
            ],
            next_card_ids: vec!["dkg.executable_asset".into()],
            json_examples: vec![serde_json::json!({
                "scope": "project",
                "label": "Learned roof system",
                "target_class": "roof_system",
                "source_passage_refs": []
            })],
        },
        GuidanceCardInfo {
            id: "dkg.executable_asset".into(),
            title: "Executable Learned Assets".into(),
            task_tags: vec!["execution".into(), "geometry".into()],
            summary: "Only evidence-backed runtime claims can advertise geometry emission or re-synthesis behavior.".into(),
            referenced_tool_ids: vec!["parametric.create".into(), "take_screenshot".into()],
            next_card_ids: Vec::new(),
            json_examples: vec![serde_json::json!({ "type_id": "learned.asset", "overrides": {} })],
        },
    ]
}

// ---------------------------------------------------------------------------
// PP74: Constraint layer handlers
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_list_constraints(world: &World, _scope: Option<String>) -> Vec<ConstraintInfo> {
    use crate::capability_registry::CapabilityRegistry;

    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return Vec::new();
    };
    registry
        .constraint_descriptors()
        .iter()
        .map(|d| ConstraintInfo {
            id: d.id.0.clone(),
            label: d.label.clone(),
            description: d.description.clone(),
            default_severity: d.default_severity.as_str().to_string(),
            rationale: d.rationale.clone(),
            element_classes: d
                .applicability
                .element_classes
                .iter()
                .map(|c| c.0.clone())
                .collect(),
            required_state: d
                .applicability
                .required_state
                .map(|s| s.as_str().to_string()),
        })
        .collect()
}

/// Read findings from the `Findings` resource (populated by the last sweep).
///
/// Called after `validation_sweep_system` runs in the dispatch path.
#[cfg(feature = "model-api")]
fn handle_run_validation_v2(world: &World, element_id: Option<u64>) -> Vec<ValidationFindingInfo> {
    use crate::plugins::validation::Findings;

    if let Some(eid) = element_id {
        if crate::plugins::refinement::is_parked_refinement_entity(world, ElementId(eid)) {
            return Vec::new();
        }
    }

    let Some(findings) = world.get_resource::<Findings>() else {
        return Vec::new();
    };

    let iter: Box<dyn Iterator<Item = &crate::capability_registry::Finding>> =
        if let Some(eid) = element_id {
            Box::new(
                findings
                    .cache
                    .iter()
                    .filter(move |((_, e), _)| *e == eid)
                    .flat_map(|(_, v)| v.iter()),
            )
        } else {
            Box::new(findings.all())
        };

    iter.map(finding_to_info).collect()
}

/// Convert a `Finding` to the MCP-facing `ValidationFindingInfo`.
#[cfg(feature = "model-api")]
fn finding_to_info(f: &crate::capability_registry::Finding) -> ValidationFindingInfo {
    ValidationFindingInfo {
        finding_id: f.id.0.clone(),
        entity_element_id: f.subject,
        validator: f.constraint_id.0.clone(),
        severity: f.severity.as_str().to_string(),
        message: f.message.clone(),
        rationale: f.rationale.clone(),
        // obligation_id not carried on PP74 Finding; left None here.
        // The finding_id encodes obligation identity in its string format.
        obligation_id: None,
    }
}

/// Look up a finding by `FindingId` in the `Findings` index and return a rich
/// explanation JSON object.
#[cfg(feature = "model-api")]
fn handle_explain_finding_v2(world: &World, finding_id: String) -> ApiResult<serde_json::Value> {
    use crate::capability_registry::{CapabilityRegistry, FindingId};
    use crate::plugins::validation::Findings;

    let fid = FindingId(finding_id.clone());

    let Some(findings) = world.get_resource::<Findings>() else {
        return Err("Findings resource not initialised; run a validation sweep first".into());
    };

    let Some(finding) = findings.index.get(&fid) else {
        return Err(format!("Unknown finding_id '{finding_id}'"));
    };

    let constraint_rationale = world
        .get_resource::<CapabilityRegistry>()
        .and_then(|r| r.constraint_descriptor(&finding.constraint_id))
        .map(|c| c.rationale.clone())
        .unwrap_or_else(|| finding.rationale.clone());

    Ok(serde_json::json!({
        "finding_id": finding.id.0,
        "constraint_id": finding.constraint_id.0,
        "subject_element_id": finding.subject,
        "severity": finding.severity.as_str(),
        "message": finding.message,
        "rationale": constraint_rationale,
        "backlink": finding.backlink.as_ref().map(|p| &p.0),
        "emitted_at": finding.emitted_at,
    }))
}

#[cfg(feature = "model-api")]
fn handle_preview_promotion(
    world: &mut World,
    element_id: u64,
    target_state_str: String,
    recipe_id: Option<String>,
    overrides: serde_json::Value,
) -> ApiResult<PreviewPromotionResult> {
    let plan = build_refinement_promotion_plan(
        world,
        element_id,
        target_state_str.clone(),
        recipe_id,
        overrides,
    )?;

    Ok(PreviewPromotionResult {
        element_id,
        would_transition_to: target_state_str,
        obligation_set: plan.obligations.clone(),
        findings: plan.findings.clone(),
        plan,
    })
}

#[cfg(feature = "model-api")]
fn build_refinement_promotion_plan(
    world: &World,
    element_id: u64,
    target_state_str: String,
    recipe_id: Option<String>,
    overrides: serde_json::Value,
) -> ApiResult<RefinementPromotionPlanInfo> {
    use crate::capability_registry::{
        effective_obligations, CapabilityRegistry, ElementClassAssignment,
    };
    use crate::plugins::refinement::{
        resolve_refinement_subtree, validate_declared_state_obligations, Obligation, ObligationSet,
        ObligationStatus, RefinementState, RefinementStateComponent,
    };

    let target_state = RefinementState::from_str(&target_state_str)
        .ok_or_else(|| format!("Unknown refinement state: '{target_state_str}'"))?;

    let eid = ElementId(element_id);
    ensure_refinable_entity_exists(world, eid)?;
    let entity = find_entity_by_element_id_readonly(world, eid)
        .ok_or_else(|| format!("Entity not found: {element_id}"))?;

    let current_state = world
        .get::<RefinementStateComponent>(entity)
        .map(|component| component.state)
        .unwrap_or_default();
    if target_state <= current_state {
        return Err(format!(
            "Cannot promote from {} to {} — target must be higher",
            current_state.as_str(),
            target_state.as_str()
        ));
    }

    let subtree = resolve_refinement_subtree(world, eid)?;
    let assignment = world.get::<ElementClassAssignment>(entity).cloned();
    let registry = world.get_resource::<CapabilityRegistry>();
    let requested_recipe_id = recipe_id.clone();
    let effective_recipe_id = requested_recipe_id.clone().or_else(|| {
        assignment
            .as_ref()
            .and_then(|assignment| assignment.active_recipe.as_ref())
            .map(|id| id.0.clone())
    });
    let recipe_descriptor = registry.and_then(|registry| {
        effective_recipe_id.as_ref().and_then(|id| {
            registry
                .recipe_family_descriptor(&crate::capability_registry::RecipeFamilyId(id.clone()))
        })
    });
    let class_descriptor = registry.and_then(|registry| {
        assignment
            .as_ref()
            .and_then(|assignment| registry.element_class_descriptor(&assignment.element_class))
    });

    let mut missing_inputs = Vec::new();
    if let Some(recipe_id) = requested_recipe_id.as_deref() {
        if world
            .get_resource::<crate::plugins::recipe_drafts::RecipeDraftRegistry>()
            .and_then(|registry| registry.get(recipe_id))
            .is_some()
        {
            missing_inputs.push(format!(
                "recipe draft '{recipe_id}' is consultable but not executable"
            ));
        } else if recipe_descriptor.is_none() {
            missing_inputs.push(format!("unknown recipe family '{recipe_id}'"));
        }
    }
    if let (Some(assignment), Some(recipe)) = (&assignment, recipe_descriptor) {
        if assignment.element_class != recipe.target_class {
            missing_inputs.push(format!(
                "recipe '{}' targets '{}' but entity is assigned to '{}'",
                recipe.id.0, recipe.target_class.0, assignment.element_class.0
            ));
        }
    }
    if let Some(recipe) = recipe_descriptor {
        if !recipe.supported_refinement_levels.contains(&target_state) {
            missing_inputs.push(format!(
                "recipe '{}' does not support target state {}",
                recipe.id.0,
                target_state.as_str()
            ));
        }
        let override_keys = overrides
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        for parameter in &recipe.parameters {
            if parameter.default.is_none() && !override_keys.contains(&parameter.name) {
                missing_inputs.push(format!(
                    "missing required recipe parameter '{}'",
                    parameter.name
                ));
            }
        }
    }

    let obligation_templates = class_descriptor
        .map(|class| effective_obligations(class, recipe_descriptor, target_state))
        .unwrap_or_default();
    let obligation_set = ObligationSet {
        entries: obligation_templates
            .into_iter()
            .map(|template| Obligation {
                id: template.id,
                role: template.role,
                required_by_state: template.required_by_state,
                status: ObligationStatus::Unresolved,
            })
            .collect(),
    };
    let obligations: Vec<ObligationInfo> = obligation_set
        .entries
        .iter()
        .map(obligation_to_info)
        .collect();
    let findings: Vec<ValidationFindingInfo> =
        validate_declared_state_obligations(element_id, target_state, &obligation_set)
            .into_iter()
            .map(refinement_finding_to_info)
            .collect();

    let validators = promotion_plan_validators(world, entity, target_state);
    let changed_entities = subtree
        .active_element_ids
        .iter()
        .map(|id| PromotionPlanEntityChangeInfo {
            element_id: Some(*id),
            action: "set_refinement_state".to_string(),
            reason: format!(
                "default selected for promotion to {}",
                target_state.as_str()
            ),
        })
        .collect::<Vec<_>>();
    let generated_entities = recipe_descriptor
        .map(|recipe| {
            vec![PromotionPlanEntityChangeInfo {
                element_id: None,
                action: "invoke_recipe".to_string(),
                reason: format!("recipe '{}' may create children during commit", recipe.id.0),
            }]
        })
        .unwrap_or_default();
    let mut derived_graph_additions = changed_entities
        .iter()
        .filter_map(|change| change.element_id)
        .map(|id| format!("refinement_state:{id}:{}", target_state.as_str()))
        .collect::<Vec<_>>();
    derived_graph_additions.extend(
        obligations
            .iter()
            .map(|obligation| format!("obligation:{element_id}:{}", obligation.id)),
    );
    derived_graph_additions.extend(
        validators
            .iter()
            .map(|validator| format!("validator:{}", validator.id)),
    );
    if let Some(recipe_id) = effective_recipe_id.as_ref() {
        derived_graph_additions.push(format!("recipe_generate:{recipe_id}"));
    }
    derived_graph_additions.sort();
    derived_graph_additions.dedup();

    Ok(RefinementPromotionPlanInfo {
        plan_id: format!(
            "promotion:{element_id}:{}:{}:{}",
            current_state.as_str(),
            target_state.as_str(),
            effective_recipe_id.as_deref().unwrap_or("-")
        ),
        target: RefinementPromotionTargetInfo {
            kind: "refinement_subtree".to_string(),
            root_element_id: element_id,
        },
        affected_scope: RefinementPromotionScopeInfo {
            root_element_id: subtree.root_element_id,
            default_selected_element_ids: subtree.active_element_ids,
            editable: true,
            project_wide: false,
        },
        current_state: current_state.as_str().to_string(),
        target_state: target_state.as_str().to_string(),
        recipe_id: effective_recipe_id,
        default_commit_policy: "require_clean".to_string(),
        supported_commit_policies: vec![
            "require_clean".to_string(),
            "accept_with_waivers".to_string(),
            "accept_partial".to_string(),
        ],
        changed_entities,
        generated_entities,
        parked_entities: Vec::new(),
        removed_entities: Vec::new(),
        obligations,
        validators,
        missing_inputs: missing_inputs.clone(),
        findings,
        derived_graph_additions,
        can_commit: missing_inputs.is_empty(),
    })
}

#[cfg(feature = "model-api")]
fn obligation_to_info(obligation: &crate::plugins::refinement::Obligation) -> ObligationInfo {
    ObligationInfo {
        id: obligation.id.0.clone(),
        role: obligation.role.0.clone(),
        required_by_state: obligation.required_by_state.as_str().to_string(),
        status: match &obligation.status {
            crate::plugins::refinement::ObligationStatus::Unresolved => "Unresolved".to_string(),
            crate::plugins::refinement::ObligationStatus::SatisfiedBy(id) => {
                format!("SatisfiedBy:{id}")
            }
            crate::plugins::refinement::ObligationStatus::Deferred(reason) => {
                format!("Deferred:{reason}")
            }
            crate::plugins::refinement::ObligationStatus::Waived(rationale) => {
                format!("Waived:{rationale}")
            }
        },
    }
}

#[cfg(feature = "model-api")]
fn refinement_finding_to_info(
    finding: crate::plugins::refinement::ValidationFinding,
) -> ValidationFindingInfo {
    ValidationFindingInfo {
        finding_id: finding.finding_id,
        entity_element_id: finding.entity_element_id,
        validator: finding.validator,
        severity: finding.severity.as_str().to_string(),
        message: finding.message,
        rationale: finding.rationale,
        obligation_id: finding.obligation_id.map(|id| id.0),
    }
}

#[cfg(feature = "model-api")]
fn promotion_plan_validators(
    world: &World,
    entity: Entity,
    target_state: crate::plugins::refinement::RefinementState,
) -> Vec<PromotionPlanValidatorInfo> {
    use crate::capability_registry::{CapabilityRegistry, ElementClassAssignment};

    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return Vec::new();
    };
    let entity_class = world
        .get::<ElementClassAssignment>(entity)
        .map(|assignment| assignment.element_class.clone());
    registry
        .constraint_descriptors()
        .iter()
        .filter(|descriptor| {
            if !descriptor.applicability.element_classes.is_empty() {
                let matches_class = entity_class
                    .as_ref()
                    .map(|class| descriptor.applicability.element_classes.contains(class))
                    .unwrap_or(false);
                if !matches_class {
                    return false;
                }
            }
            descriptor
                .applicability
                .required_state
                .is_none_or(|required| target_state >= required)
        })
        .map(|descriptor| PromotionPlanValidatorInfo {
            id: descriptor.id.0.clone(),
            label: descriptor.label.clone(),
            role: descriptor.role.as_str().to_string(),
            default_severity: descriptor.default_severity.as_str().to_string(),
            required_state: descriptor
                .applicability
                .required_state
                .map(|state| state.as_str().to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// PP75: Catalog provider handlers
// ---------------------------------------------------------------------------

/// Return a summary of every registered catalog provider.
#[cfg(feature = "model-api")]
pub fn handle_list_catalog_providers(world: &World) -> Vec<CatalogProviderInfo> {
    use crate::capability_registry::CapabilityRegistry;

    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return Vec::new();
    };
    registry
        .catalog_provider_descriptors()
        .iter()
        .map(|d| CatalogProviderInfo {
            id: d.id.0.clone(),
            label: d.label.clone(),
            description: d.description.clone(),
            category: d.category.as_str().to_string(),
            region: d.region.clone(),
            license: d.license.as_str().to_string(),
            source_version: d.source_version.clone(),
        })
        .collect()
}

/// Query a catalog provider by id, passing the raw JSON filter through.
///
/// PP75: every registered provider's `query_fn` ignores the filter and returns
/// all rows. Real filtering is a follow-on.
#[cfg(feature = "model-api")]
pub fn handle_catalog_query(
    world: &World,
    provider_id: String,
    filter: serde_json::Value,
) -> ApiResult<Vec<CatalogRowInfo>> {
    use crate::capability_registry::{CapabilityRegistry, CatalogProviderId};

    let registry = world
        .get_resource::<CapabilityRegistry>()
        .ok_or_else(|| "CapabilityRegistry not found".to_string())?;

    let pid = CatalogProviderId(provider_id.clone());
    let descriptor = registry
        .catalog_provider_descriptor(&pid)
        .ok_or_else(|| format!("Unknown catalog provider '{provider_id}'"))?;

    let rows = (descriptor.query_fn)(&filter);

    Ok(rows
        .into_iter()
        .map(|row| CatalogRowInfo {
            row_id: row.row_id.0,
            category: row.category.as_str().to_string(),
            data: row.data,
            license: row.provenance.license.as_str().to_string(),
            source_version: row.provenance.source_version,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// PP76: Generation prior handlers
// ---------------------------------------------------------------------------

/// Return a summary of every registered generation prior, optionally filtered
/// by scope.
///
/// `scope_filter` may carry keys `element_class` (string) and/or `claim_path`
/// (string). If absent or empty, all priors are returned.
#[cfg(feature = "model-api")]
pub fn handle_list_generation_priors(
    world: &World,
    scope_filter: Option<serde_json::Value>,
) -> Vec<GenerationPriorInfo> {
    use crate::capability_registry::{CapabilityRegistry, ElementClassId};

    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return Vec::new();
    };

    // Derive optional element-class filter from the scope_filter object.
    let element_class_filter: Option<ElementClassId> = scope_filter
        .as_ref()
        .and_then(|v| v.get("element_class"))
        .and_then(|v| v.as_str())
        .map(|s| ElementClassId(s.to_owned()));

    registry
        .generation_prior_descriptors(element_class_filter.as_ref())
        .into_iter()
        .map(|d| GenerationPriorInfo {
            id: d.id.0.clone(),
            label: d.label.clone(),
            description: d.description.clone(),
            scope: serde_json::to_value(&d.scope).unwrap_or_default(),
            license: d.source_provenance.license.as_str().to_string(),
            source_version: d.source_provenance.source_version.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// PP78: Corpus operations handlers
// ---------------------------------------------------------------------------

/// Return summaries of all unresolved corpus gaps.
#[cfg(feature = "model-api")]
pub fn handle_list_corpus_gaps(world: &World) -> Vec<CorpusGapInfo> {
    use crate::plugins::corpus_gap::CorpusGapQueue;

    let Some(queue) = world.get_resource::<CorpusGapQueue>() else {
        return Vec::new();
    };
    queue.list().iter().map(corpus_gap_to_info).collect()
}

/// Push a new corpus gap and return the created entry.
#[cfg(feature = "model-api")]
pub fn handle_request_corpus_expansion(
    world: &mut World,
    element_class: Option<String>,
    jurisdiction: Option<String>,
    kind: String,
    rationale: String,
) -> CorpusGapInfo {
    use crate::curation::AssetKindId;
    use crate::plugins::corpus_gap::{CorpusGap, CorpusGapId, CorpusGapQueue};
    use std::time::{SystemTime, UNIX_EPOCH};

    let reported_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let gap = CorpusGap {
        id: CorpusGapId(String::new()), // overwritten by queue.push
        element_class: element_class.clone(),
        kind: Some(AssetKindId::new(kind.clone())),
        jurisdiction: jurisdiction.clone(),
        missing_artifact_kind: kind.clone(),
        context: serde_json::json!({ "rationale": rationale }),
        reported_by: "agent".into(),
        reported_at,
    };

    if !world.contains_resource::<CorpusGapQueue>() {
        world.insert_resource(CorpusGapQueue::default());
    }
    let id = world.resource_mut::<CorpusGapQueue>().push(gap);

    // Re-borrow to read back the just-inserted gap.
    let queue = world.resource::<CorpusGapQueue>();
    let gap = queue.list().iter().find(|g| g.id == id).unwrap();
    corpus_gap_to_info(gap)
}

/// Look up a passage in the `CorpusPassageRegistry`.
#[cfg(feature = "model-api")]
pub fn handle_lookup_source_passage(
    world: &World,
    passage_ref: String,
) -> ApiResult<PassageLookupInfo> {
    use crate::capability_registry::PassageRef;
    use crate::plugins::corpus_gap::CorpusPassageRegistry;

    let registry = world
        .get_resource::<CorpusPassageRegistry>()
        .ok_or_else(|| "CorpusPassageRegistry not found in world".to_string())?;

    let pref = PassageRef(passage_ref.clone());
    let entry = registry
        .get(&pref)
        .ok_or_else(|| format!("passage '{passage_ref}' not found in CorpusPassageRegistry"))?;

    Ok(PassageLookupInfo {
        passage_ref,
        text: entry.text.clone(),
        source: entry.provenance.source.clone(),
        source_version: entry.provenance.source_version.clone(),
        jurisdiction: entry.provenance.jurisdiction.clone(),
        license: entry.provenance.license.as_str().to_string(),
    })
}

/// Produce a Rust validator skeleton anchored to a corpus passage.
#[cfg(feature = "model-api")]
pub fn handle_draft_rule_pack(
    world: &World,
    chunk_id: String,
    element_class: String,
) -> ApiResult<DraftRulePackInfo> {
    use crate::capability_registry::PassageRef;
    use crate::plugins::corpus_gap::CorpusPassageRegistry;

    let registry = world
        .get_resource::<CorpusPassageRegistry>()
        .ok_or_else(|| "CorpusPassageRegistry not found in world".to_string())?;

    let pref = PassageRef(chunk_id.clone());
    let entry = registry
        .get(&pref)
        .ok_or_else(|| format!("chunk_id '{chunk_id}' not found in CorpusPassageRegistry"))?;

    // Sanitise the passage ref into a valid Rust identifier fragment.
    let ident = chunk_id.replace([':', '-', '/'], "_").to_lowercase();
    let class_ident = element_class.replace(['-', '/'], "_").to_lowercase();

    let passage_preview: String = entry.text.chars().take(120).collect();

    let rust_skeleton = format!(
        r#"// Auto-scaffolded by draft_rule_pack (PP78). Complete the validator body before registering.
// Source passage: {chunk_id}
// "{passage_preview}..."

use std::sync::Arc;
use talos3d_core::capability_registry::{{
    Applicability, ConstraintDescriptor, ConstraintId, ElementClassId, Finding, FindingId,
    PassageRef, Severity,
}};

pub fn {ident}_constraint() -> ConstraintDescriptor {{
    ConstraintDescriptor {{
        id: ConstraintId("{ident}".into()),
        label: "Draft {element_class} rule from {chunk_id}".into(),
        description: "Draft validator scaffold generated from source passage {chunk_id}.".into(),
        applicability: Applicability {{
            element_classes: vec![ElementClassId("{class_ident}".into())],
            required_state: None,
        }},
        default_severity: Severity::Error,
        rationale: "Source-backed draft rule generated from passage {chunk_id}; author must encode the final validation criteria before registration.".into(),
        source_backlink: Some(PassageRef("{chunk_id}".into())),
        validator: Arc::new(|entity, world| {{
            // Read parameters from entity components and return findings here.
            vec![]
        }}),
    }}
}}
"#
    );

    Ok(DraftRulePackInfo {
        rust_skeleton,
        backlink: chunk_id,
        notes: vec![
            "Complete the validator body before registering; the skeleton returns no findings as-is.".into(),
            "Review the generated label, description, and rationale against the source passage.".into(),
            "Add the new constraint to your Plugin::build via app.register_constraint(…).".into(),
        ],
    })
}

/// Check all registered constraint backlinks against the `CorpusPassageRegistry`.
#[cfg(feature = "model-api")]
pub fn handle_check_rule_pack_backlinks(world: &World) -> BacklinkCheckReportInfo {
    use crate::plugins::corpus_gap::resolve_all_rule_pack_backlinks;

    let report = resolve_all_rule_pack_backlinks(world);
    BacklinkCheckReportInfo {
        total: report.total,
        resolved: report.resolved,
        broken: report
            .broken
            .into_iter()
            .map(|b| BrokenBacklinkInfo {
                constraint_id: b.constraint_id,
                passage_ref: b.passage_ref,
            })
            .collect(),
    }
}

#[cfg(feature = "model-api")]
pub fn handle_list_recipe_drafts(
    world: &World,
    target_class: Option<String>,
    status: Option<String>,
) -> ApiResult<Vec<RecipeDraftInfo>> {
    use crate::plugins::recipe_drafts::{RecipeDraftRegistry, RecipeDraftStatus};

    let parsed_status = status
        .as_deref()
        .map(|value| {
            RecipeDraftStatus::from_str(value)
                .ok_or_else(|| format!("unknown recipe draft status '{value}'"))
        })
        .transpose()?;

    let Some(registry) = world.get_resource::<RecipeDraftRegistry>() else {
        return Ok(Vec::new());
    };

    Ok(registry
        .list(target_class.as_deref(), parsed_status)
        .iter()
        .map(recipe_draft_to_info)
        .collect())
}

#[cfg(feature = "model-api")]
pub fn handle_get_recipe_draft(
    world: &World,
    recipe_draft_id: String,
) -> ApiResult<RecipeDraftInfo> {
    use crate::plugins::recipe_drafts::RecipeDraftRegistry;

    let registry = world
        .get_resource::<RecipeDraftRegistry>()
        .ok_or_else(|| "RecipeDraftRegistry not found in world".to_string())?;
    let draft = registry
        .get(&recipe_draft_id)
        .ok_or_else(|| format!("recipe draft not found: '{recipe_draft_id}'"))?;
    Ok(recipe_draft_to_info(draft))
}

#[cfg(feature = "model-api")]
pub fn handle_save_recipe_draft(
    world: &mut World,
    request: SaveRecipeDraftRequest,
) -> ApiResult<RecipeDraftInfo> {
    use crate::plugins::corpus_gap::{CorpusGapQueue, CorpusPassageRegistry};
    use crate::plugins::knowledge_assets::{parse_scope, KnowledgeResidency};
    use crate::plugins::recipe_drafts::{
        recipe_draft_meta_for, RecipeDraftArtifact, RecipeDraftParameter, RecipeDraftRegistry,
        RecipeDraftStatus,
    };

    if let Some(ref gap_id) = request.gap_id {
        let Some(queue) = world.get_resource::<CorpusGapQueue>() else {
            return Err("CorpusGapQueue not found in world".to_string());
        };
        let found = queue.list().iter().any(|gap| gap.id.0 == *gap_id);
        if !found {
            return Err(format!("unknown corpus gap id '{gap_id}'"));
        }
    }

    if !request.source_passage_refs.is_empty() {
        let registry = world
            .get_resource::<CorpusPassageRegistry>()
            .ok_or_else(|| "CorpusPassageRegistry not found in world".to_string())?;
        for passage_ref in &request.source_passage_refs {
            let found = registry
                .iter()
                .any(|(registered_ref, _)| registered_ref == passage_ref.as_str());
            if !found {
                return Err(format!("unknown source passage ref '{passage_ref}'"));
            }
        }
    }

    let status = match request.status.as_deref() {
        Some(value) => RecipeDraftStatus::from_str(value)
            .ok_or_else(|| format!("unknown recipe draft status '{value}'"))?,
        None => request
            .recipe_draft_id
            .as_deref()
            .and_then(|id| {
                world
                    .get_resource::<RecipeDraftRegistry>()
                    .and_then(|registry| registry.get(id))
                    .map(|draft| draft.status)
            })
            .unwrap_or(RecipeDraftStatus::Drafted),
    };
    let scope = parse_scope(request.scope.as_deref())?;

    world.init_resource::<RecipeDraftRegistry>();
    let saved = world
        .resource_mut::<RecipeDraftRegistry>()
        .save(RecipeDraftArtifact {
            id: request.recipe_draft_id.unwrap_or_default(),
            meta: crate::plugins::knowledge_assets::default_recipe_draft_meta(),
            residency: if scope == crate::curation::Scope::Session {
                KnowledgeResidency::SessionCache
            } else {
                KnowledgeResidency::ProjectFile
            },
            label: request.label,
            description: request.description,
            target_class: request.target_class,
            supported_refinement_levels: request.supported_refinement_levels,
            parameters: request
                .parameters
                .into_iter()
                .map(|parameter| RecipeDraftParameter {
                    name: parameter.name,
                    value_schema: parameter.value_schema,
                    default: parameter.default,
                })
                .collect(),
            jurisdiction: request.jurisdiction,
            gap_id: request.gap_id,
            source_passage_refs: request.source_passage_refs,
            evidence_slots: request.evidence_slots,
            runtime_claims: request.runtime_claims,
            acquisition_context: request.acquisition_context,
            draft_script: request.draft_script,
            notes: request.notes,
            status,
            created_at: 0,
            updated_at: 0,
        });
    if saved.meta.scope != scope {
        let mut saved = saved;
        saved.meta = recipe_draft_meta_for(&saved, scope);
        let saved = world.resource_mut::<RecipeDraftRegistry>().save(saved);
        return Ok(recipe_draft_to_info(&saved));
    }

    Ok(recipe_draft_to_info(&saved))
}

#[cfg(feature = "model-api")]
pub fn handle_set_recipe_draft_status(
    world: &mut World,
    recipe_draft_id: String,
    status: String,
) -> ApiResult<RecipeDraftInfo> {
    use crate::plugins::recipe_drafts::{RecipeDraftRegistry, RecipeDraftStatus};

    let status = RecipeDraftStatus::from_str(&status)
        .ok_or_else(|| format!("unknown recipe draft status '{status}'"))?;
    let mut registry = world
        .get_resource_mut::<RecipeDraftRegistry>()
        .ok_or_else(|| "RecipeDraftRegistry not found in world".to_string())?;
    let updated = registry.set_status(&recipe_draft_id, status)?;
    Ok(recipe_draft_to_info(&updated))
}

#[cfg(feature = "model-api")]
pub fn handle_list_assembly_pattern_drafts(
    world: &World,
    target_type: Option<String>,
    status: Option<String>,
) -> ApiResult<Vec<AssemblyPatternDraftInfo>> {
    use crate::plugins::assembly_pattern_drafts::{
        AssemblyPatternDraftRegistry, AssemblyPatternDraftStatus,
    };

    let parsed_status = status
        .as_deref()
        .map(|value| {
            AssemblyPatternDraftStatus::from_str(value)
                .ok_or_else(|| format!("unknown assembly pattern draft status '{value}'"))
        })
        .transpose()?;

    let Some(registry) = world.get_resource::<AssemblyPatternDraftRegistry>() else {
        return Ok(Vec::new());
    };

    Ok(registry
        .list(target_type.as_deref(), parsed_status)
        .iter()
        .map(assembly_pattern_draft_to_info)
        .collect())
}

#[cfg(feature = "model-api")]
pub fn handle_get_assembly_pattern_draft(
    world: &World,
    assembly_pattern_draft_id: String,
) -> ApiResult<AssemblyPatternDraftInfo> {
    use crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftRegistry;

    let registry = world
        .get_resource::<AssemblyPatternDraftRegistry>()
        .ok_or_else(|| "AssemblyPatternDraftRegistry not found in world".to_string())?;
    let draft = registry.get(&assembly_pattern_draft_id).ok_or_else(|| {
        format!("assembly pattern draft not found: '{assembly_pattern_draft_id}'")
    })?;
    Ok(assembly_pattern_draft_to_info(draft))
}

#[cfg(feature = "model-api")]
pub fn handle_save_assembly_pattern_draft(
    world: &mut World,
    request: SaveAssemblyPatternDraftRequest,
) -> ApiResult<AssemblyPatternDraftInfo> {
    use crate::capability_registry::{AssemblyPatternLayerDescriptor, AssemblyPatternRelationRule};
    use crate::plugins::assembly_pattern_drafts::{
        assembly_pattern_draft_meta_for, AssemblyPatternDraftArtifact,
        AssemblyPatternDraftRegistry, AssemblyPatternDraftStatus,
    };
    use crate::plugins::corpus_gap::{CorpusGapQueue, CorpusPassageRegistry};
    use crate::plugins::knowledge_assets::{parse_scope, KnowledgeResidency};

    if let Some(ref gap_id) = request.gap_id {
        let Some(queue) = world.get_resource::<CorpusGapQueue>() else {
            return Err("CorpusGapQueue not found in world".to_string());
        };
        let found = queue.list().iter().any(|gap| gap.id.0 == *gap_id);
        if !found {
            return Err(format!("unknown corpus gap id '{gap_id}'"));
        }
    }

    if !request.source_passage_refs.is_empty() {
        let registry = world
            .get_resource::<CorpusPassageRegistry>()
            .ok_or_else(|| "CorpusPassageRegistry not found in world".to_string())?;
        for passage_ref in &request.source_passage_refs {
            let found = registry
                .iter()
                .any(|(registered_ref, _)| registered_ref == passage_ref.as_str());
            if !found {
                return Err(format!("unknown source passage ref '{passage_ref}'"));
            }
        }
    }

    if request.target_types.is_empty() {
        return Err("assembly pattern drafts require at least one target_type".to_string());
    }

    let status = match request.status.as_deref() {
        Some(value) => AssemblyPatternDraftStatus::from_str(value)
            .ok_or_else(|| format!("unknown assembly pattern draft status '{value}'"))?,
        None => request
            .assembly_pattern_draft_id
            .as_deref()
            .and_then(|id| {
                world
                    .get_resource::<AssemblyPatternDraftRegistry>()
                    .and_then(|registry| registry.get(id))
                    .map(|draft| draft.status)
            })
            .unwrap_or(AssemblyPatternDraftStatus::Drafted),
    };
    let scope = parse_scope(request.scope.as_deref())?;

    world.init_resource::<AssemblyPatternDraftRegistry>();
    let saved =
        world
            .resource_mut::<AssemblyPatternDraftRegistry>()
            .save(AssemblyPatternDraftArtifact {
                id: request.assembly_pattern_draft_id.unwrap_or_default(),
                meta: crate::plugins::knowledge_assets::default_assembly_pattern_draft_meta(),
                residency: if scope == crate::curation::Scope::Session {
                    KnowledgeResidency::SessionCache
                } else {
                    KnowledgeResidency::ProjectFile
                },
                label: request.label,
                description: request.description,
                target_types: request.target_types,
                axis: request.axis,
                layers: request
                    .layers
                    .into_iter()
                    .map(|layer| AssemblyPatternLayerDescriptor {
                        layer_id: layer.layer_id,
                        label: layer.label,
                        role: layer.role,
                        material_hint: layer.material_hint,
                        optional: layer.optional,
                    })
                    .collect(),
                relation_rules: request
                    .relation_rules
                    .into_iter()
                    .map(|rule| AssemblyPatternRelationRule {
                        relation_type: rule.relation_type,
                        source_layer_id: rule.source_layer_id,
                        target_layer_id: rule.target_layer_id,
                        required: rule.required,
                        rationale: rule.rationale,
                    })
                    .collect(),
                root_layer_ids: request.root_layer_ids,
                requires_support_path: request.requires_support_path,
                tags: request.tags,
                parameter_schema: request.parameter_schema,
                jurisdiction: request.jurisdiction,
                gap_id: request.gap_id,
                source_passage_refs: request.source_passage_refs,
                evidence_slots: request.evidence_slots,
                runtime_claims: request.runtime_claims,
                acquisition_context: request.acquisition_context,
                notes: request.notes,
                status,
                created_at: 0,
                updated_at: 0,
            });
    if saved.meta.scope != scope {
        let mut saved = saved;
        saved.meta = assembly_pattern_draft_meta_for(&saved, scope);
        let saved = world
            .resource_mut::<AssemblyPatternDraftRegistry>()
            .save(saved);
        return Ok(assembly_pattern_draft_to_info(&saved));
    }

    Ok(assembly_pattern_draft_to_info(&saved))
}

#[cfg(feature = "model-api")]
pub fn handle_set_assembly_pattern_draft_status(
    world: &mut World,
    assembly_pattern_draft_id: String,
    status: String,
) -> ApiResult<AssemblyPatternDraftInfo> {
    use crate::plugins::assembly_pattern_drafts::{
        AssemblyPatternDraftRegistry, AssemblyPatternDraftStatus,
    };

    let status = AssemblyPatternDraftStatus::from_str(&status)
        .ok_or_else(|| format!("unknown assembly pattern draft status '{status}'"))?;
    let mut registry = world
        .get_resource_mut::<AssemblyPatternDraftRegistry>()
        .ok_or_else(|| "AssemblyPatternDraftRegistry not found in world".to_string())?;
    let updated = registry.set_status(&assembly_pattern_draft_id, status)?;
    Ok(assembly_pattern_draft_to_info(&updated))
}

#[cfg(feature = "model-api")]
pub fn handle_materialize_learned_asset(
    world: &mut World,
    request: MaterializeLearnedAssetRequest,
) -> ApiResult<MaterializeLearnedAssetResult> {
    use crate::plugins::knowledge_assets::RuntimeCapabilityClaim;
    use crate::plugins::recipe_drafts::RecipeDraftRegistry;

    let draft = world
        .get_resource::<RecipeDraftRegistry>()
        .and_then(|registry| {
            registry.snapshot().into_iter().find(|draft| {
                draft.meta.id.as_str() == request.asset_id || draft.id == request.asset_id
            })
        })
        .ok_or_else(|| format!("learned asset not found: '{}'", request.asset_id))?;

    let mut evidence_backed_claim_ids: Vec<String> = draft
        .runtime_claims
        .iter()
        .filter(|claim| claim.is_evidence_backed())
        .map(|claim| claim.claim_id.clone())
        .collect();
    if !evidence_backed_claim_ids
        .iter()
        .any(|id| id == "geometry_emission")
    {
        return Err(
            "learned asset cannot materialize: missing evidence-backed geometry_emission runtime claim"
                .into(),
        );
    }

    let parametric_value = draft
        .draft_script
        .get("parametric_create")
        .cloned()
        .ok_or_else(|| {
            "learned asset cannot materialize: draft_script.parametric_create is missing"
                .to_string()
        })?;
    let mut parametric_request: crate::plugins::parametric_mcp::CreateParametricRequest =
        serde_json::from_value(parametric_value)
            .map_err(|error| format!("invalid parametric_create payload: {error}"))?;
    for (key, value) in request.overrides {
        parametric_request.overrides.insert(key, value);
    }
    if request.placement.is_some() {
        parametric_request.placement = request.placement;
    }

    let response = crate::plugins::parametric_mcp::world_create(world, parametric_request)?;
    let last_verified = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);

    if let Some(mut registry) = world.get_resource_mut::<RecipeDraftRegistry>() {
        let mut refreshed = draft.clone();
        for claim in &mut refreshed.runtime_claims {
            if claim.claim_id == "geometry_emission" {
                claim.last_verified = Some(last_verified);
                if claim.verification_method.is_none() {
                    claim.verification_method = Some("parametric.create replay".into());
                }
            }
        }
        if !refreshed
            .runtime_claims
            .iter()
            .any(RuntimeCapabilityClaim::is_evidence_backed)
        {
            refreshed.runtime_claims.push(RuntimeCapabilityClaim {
                claim_id: "geometry_emission".into(),
                capability: "materializes_geometry".into(),
                evidence_refs: draft
                    .runtime_claims
                    .iter()
                    .flat_map(|claim| claim.evidence_refs.clone())
                    .collect(),
                last_verified: Some(last_verified),
                verification_method: Some("parametric.create replay".into()),
            });
        }
        let saved = registry.save(refreshed);
        evidence_backed_claim_ids = saved
            .runtime_claims
            .iter()
            .filter(|claim| claim.is_evidence_backed())
            .map(|claim| claim.claim_id.clone())
            .collect();
    }

    Ok(MaterializeLearnedAssetResult {
        asset_id: draft.meta.id.as_str().to_string(),
        execution_path: "parametric.create".into(),
        element_ids: response.element_ids,
        evidence_backed_claim_ids,
        last_verified,
    })
}

#[cfg(feature = "model-api")]
fn corpus_gap_to_info(gap: &crate::plugins::corpus_gap::CorpusGap) -> CorpusGapInfo {
    CorpusGapInfo {
        id: gap.id.0.clone(),
        element_class: gap.element_class.clone(),
        kind: gap.kind.as_ref().map(|kind| kind.as_str().to_string()),
        jurisdiction: gap.jurisdiction.clone(),
        missing_artifact_kind: gap.missing_artifact_kind.clone(),
        context: gap.context.clone(),
        reported_by: gap.reported_by.clone(),
        reported_at: gap.reported_at,
    }
}

#[cfg(feature = "model-api")]
fn recipe_draft_to_info(
    draft: &crate::plugins::recipe_drafts::RecipeDraftArtifact,
) -> RecipeDraftInfo {
    RecipeDraftInfo {
        id: draft.id.clone(),
        curation: draft_curation_to_info(
            &draft.meta,
            draft.residency.clone(),
            draft.evidence_slots.len(),
            &draft.runtime_claims,
        ),
        label: draft.label.clone(),
        description: draft.description.clone(),
        target_class: draft.target_class.clone(),
        supported_refinement_levels: draft.supported_refinement_levels.clone(),
        parameters: draft
            .parameters
            .iter()
            .map(|parameter| RecipeParameterInfo {
                name: parameter.name.clone(),
                value_schema: parameter.value_schema.clone(),
                default: parameter.default.clone(),
            })
            .collect(),
        jurisdiction: draft.jurisdiction.clone(),
        gap_id: draft.gap_id.clone(),
        source_passage_refs: draft.source_passage_refs.clone(),
        evidence_slots: draft.evidence_slots.clone(),
        runtime_claims: draft.runtime_claims.clone(),
        acquisition_context: draft.acquisition_context.clone(),
        draft_script: draft.draft_script.clone(),
        notes: draft.notes.clone(),
        status: draft.status.as_str().to_string(),
        consultable: matches!(
            draft.status,
            crate::plugins::recipe_drafts::RecipeDraftStatus::Installed
        ),
        created_at: draft.created_at,
        updated_at: draft.updated_at,
    }
}

#[cfg(feature = "model-api")]
fn draft_curation_to_info(
    meta: &crate::curation::CurationMeta,
    residency: crate::plugins::knowledge_assets::KnowledgeResidency,
    evidence_slot_count: usize,
    runtime_claims: &[crate::plugins::knowledge_assets::RuntimeCapabilityClaim],
) -> CurationAssetInfo {
    CurationAssetInfo {
        asset_id: meta.id.as_str().to_string(),
        kind: meta.kind.as_str().to_string(),
        scope: crate::plugins::knowledge_assets::scope_as_str(meta.scope).to_string(),
        trust: crate::plugins::knowledge_assets::trust_as_str(meta.trust).to_string(),
        validation: crate::plugins::knowledge_assets::validation_as_str(&meta.validation)
            .to_string(),
        residency,
        evidence_ref_count: meta.provenance.evidence.len(),
        evidence_slot_count,
        runtime_claim_count: runtime_claims.len(),
        evidence_backed_runtime_claim_count: runtime_claims
            .iter()
            .filter(|claim| claim.is_evidence_backed())
            .count(),
    }
}

#[cfg(feature = "model-api")]
fn assembly_pattern_to_info(
    pattern: &crate::capability_registry::AssemblyPatternDescriptor,
    is_session_draft: bool,
    status: Option<String>,
    consultable: bool,
) -> AssemblyPatternInfo {
    AssemblyPatternInfo {
        id: pattern.pattern_id.clone(),
        label: pattern.label.clone(),
        description: pattern.description.clone(),
        target_types: pattern.target_types.clone(),
        axis: pattern.axis.clone(),
        layers: pattern
            .layers
            .iter()
            .map(|layer| AssemblyPatternLayerInfo {
                layer_id: layer.layer_id.clone(),
                label: layer.label.clone(),
                role: layer.role.clone(),
                material_hint: layer.material_hint.clone(),
                optional: layer.optional,
            })
            .collect(),
        relation_rules: pattern
            .relation_rules
            .iter()
            .map(|rule| AssemblyPatternRelationRuleInfo {
                relation_type: rule.relation_type.clone(),
                source_layer_id: rule.source_layer_id.clone(),
                target_layer_id: rule.target_layer_id.clone(),
                required: rule.required,
                rationale: rule.rationale.clone(),
            })
            .collect(),
        root_layer_ids: pattern.root_layer_ids.clone(),
        requires_support_path: pattern.requires_support_path,
        tags: pattern.tags.clone(),
        parameter_schema: pattern.parameter_schema.clone(),
        is_session_draft,
        status,
        consultable,
    }
}

#[cfg(feature = "model-api")]
fn assembly_pattern_draft_to_pattern_info(
    draft: &crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftArtifact,
) -> AssemblyPatternInfo {
    assembly_pattern_to_info(
        &draft.to_descriptor(),
        true,
        Some(draft.status.as_str().to_string()),
        matches!(
            draft.status,
            crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftStatus::Installed
        ),
    )
}

#[cfg(feature = "model-api")]
fn assembly_pattern_draft_to_info(
    draft: &crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftArtifact,
) -> AssemblyPatternDraftInfo {
    AssemblyPatternDraftInfo {
        id: draft.id.clone(),
        curation: draft_curation_to_info(
            &draft.meta,
            draft.residency.clone(),
            draft.evidence_slots.len(),
            &draft.runtime_claims,
        ),
        label: draft.label.clone(),
        description: draft.description.clone(),
        target_types: draft.target_types.clone(),
        axis: draft.axis.clone(),
        layers: draft
            .layers
            .iter()
            .map(|layer| AssemblyPatternLayerInfo {
                layer_id: layer.layer_id.clone(),
                label: layer.label.clone(),
                role: layer.role.clone(),
                material_hint: layer.material_hint.clone(),
                optional: layer.optional,
            })
            .collect(),
        relation_rules: draft
            .relation_rules
            .iter()
            .map(|rule| AssemblyPatternRelationRuleInfo {
                relation_type: rule.relation_type.clone(),
                source_layer_id: rule.source_layer_id.clone(),
                target_layer_id: rule.target_layer_id.clone(),
                required: rule.required,
                rationale: rule.rationale.clone(),
            })
            .collect(),
        root_layer_ids: draft.root_layer_ids.clone(),
        requires_support_path: draft.requires_support_path,
        tags: draft.tags.clone(),
        parameter_schema: draft.parameter_schema.clone(),
        jurisdiction: draft.jurisdiction.clone(),
        gap_id: draft.gap_id.clone(),
        source_passage_refs: draft.source_passage_refs.clone(),
        evidence_slots: draft.evidence_slots.clone(),
        runtime_claims: draft.runtime_claims.clone(),
        acquisition_context: draft.acquisition_context.clone(),
        notes: draft.notes.clone(),
        status: draft.status.as_str().to_string(),
        consultable: matches!(
            draft.status,
            crate::plugins::assembly_pattern_drafts::AssemblyPatternDraftStatus::Installed
        ),
        created_at: draft.created_at,
        updated_at: draft.updated_at,
    }
}

#[cfg(test)]
mod tests;
