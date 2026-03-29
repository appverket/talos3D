use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
#[cfg(feature = "model-api")]
use serde_json::Value;

use crate::authored_entity::{BoxedEntity, PropertyValueKind};
use crate::capability_registry::CapabilityRegistry;
use crate::plugins::identity::{ElementId, ElementIdAllocator};
use crate::plugins::modeling::group::{GroupEditContext, GroupMembers};
use crate::plugins::modeling::semantics::{geometry_semantics_for_snapshot, GeometrySemantics};
#[cfg(feature = "model-api")]
use crate::plugins::{
    camera::focus_orbit_camera_on_bounds,
    commands::{
        find_entity_by_element_id, queue_command_events, ApplyEntityChangesCommand,
        BeginCommandGroup, CreateEntityCommand, DeleteEntitiesCommand, EndCommandGroup,
        ResolvedDeleteEntitiesCommand,
    },
    document_properties::DocumentProperties,
    history::{apply_pending_history_commands, HistorySet},
    import::{import_file_now, ImportRegistry, ImporterDescriptor},
    layers::{LayerAssignment, LayerRegistry, LayerState},
    persistence::{load_project_from_path, save_project_to_path},
    selection::Selected,
    toolbar::{
        update_toolbar_layout_entry, ToolbarDock, ToolbarLayoutState, ToolbarRegistry,
        ToolbarSection,
    },
};

#[cfg(feature = "model-api")]
use std::{
    sync::{mpsc, Mutex},
    thread,
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
pub struct ModelApiPlugin;

#[cfg(feature = "model-api")]
impl Plugin for ModelApiPlugin {
    fn build(&self, app: &mut App) {
        let (sender, receiver) = mpsc::channel();
        app.insert_resource(ModelApiReceiver(Mutex::new(receiver)));
        app.add_systems(Update, poll_model_api_requests.before(HistorySet::Queue));
        spawn_model_api_server(sender);
    }
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityEntry {
    pub element_id: u64,
    pub entity_type: String,
    pub label: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelSummary {
    pub entity_counts: HashMap<String, usize>,
    pub assembly_counts: HashMap<String, usize>,
    pub relation_counts: HashMap<String, usize>,
    pub bounding_box: Option<BoundingBox>,
    /// Domain-specific metrics contributed by capabilities.
    pub metrics: HashMap<String, serde_json::Value>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityPropertyDetails {
    pub name: String,
    pub label: String,
    pub kind: String,
    pub value: serde_json::Value,
    pub editable: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityDetails {
    pub element_id: u64,
    pub entity_type: String,
    pub label: String,
    pub snapshot: serde_json::Value,
    pub geometry_semantics: Option<GeometrySemantics>,
    pub properties: Vec<EntityPropertyDetails>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolbarSectionDetails {
    pub label: String,
    pub command_ids: Vec<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolbarDetails {
    pub id: String,
    pub label: String,
    pub dock: String,
    pub order: u32,
    pub visible: bool,
    pub sections: Vec<ToolbarSectionDetails>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditingContextInfo {
    pub is_root: bool,
    pub stack: Vec<EditingContextEntry>,
    pub breadcrumb: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditingContextEntry {
    pub element_id: u64,
    pub name: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupMemberEntry {
    pub element_id: u64,
    pub entity_type: String,
    pub label: String,
    pub is_group: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerInfo {
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub color: Option<[f32; 4]>,
    pub active: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SplitResult {
    pub box_a_element_id: u64,
    pub box_b_element_id: u64,
    pub group_element_id: u64,
}

// --- Assembly / Relation types ---

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VocabularyInfo {
    pub assembly_types: Vec<crate::capability_registry::AssemblyTypeDescriptor>,
    pub relation_types: Vec<crate::capability_registry::RelationTypeDescriptor>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssemblyEntry {
    pub element_id: u64,
    pub assembly_type: String,
    pub label: String,
    pub member_count: usize,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssemblyDetails {
    pub element_id: u64,
    pub assembly_type: String,
    pub label: String,
    pub members: Vec<AssemblyMemberEntry>,
    pub parameters: serde_json::Value,
    pub metadata: serde_json::Value,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssemblyMemberEntry {
    pub target: u64,
    pub role: String,
    /// "entity" or "assembly"
    pub member_kind: String,
    /// The entity type_name or assembly_type.
    pub member_type: String,
    pub label: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationEntry {
    pub element_id: u64,
    pub source: u64,
    pub target: u64,
    pub relation_type: String,
    pub parameters: serde_json::Value,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateAssemblyResult {
    pub assembly_id: u64,
    pub relation_ids: Vec<u64>,
}

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
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
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
#[derive(Resource)]
struct ModelApiReceiver(Mutex<mpsc::Receiver<ModelApiRequest>>);

#[cfg(feature = "model-api")]
enum ModelApiRequest {
    ListEntities(oneshot::Sender<Vec<EntityEntry>>),
    GetEntity {
        element_id: u64,
        response: oneshot::Sender<Option<serde_json::Value>>,
    },
    GetEntityDetails {
        element_id: u64,
        response: oneshot::Sender<Option<EntityDetails>>,
    },
    ModelSummary(oneshot::Sender<ModelSummary>),
    ListImporters(oneshot::Sender<Vec<ImporterDescriptor>>),
    CreateEntity {
        json: Value,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ImportFile {
        path: String,
        format_hint: Option<String>,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    DeleteEntities {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<usize>>,
    },
    Transform {
        request: TransformToolRequest,
        response: oneshot::Sender<ApiResult<Vec<Value>>>,
    },
    SetProperty {
        element_id: u64,
        property_name: String,
        value: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    ListHandles {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<HandleInfo>>>,
    },
    GetDocumentProperties(oneshot::Sender<serde_json::Value>),
    SetDocumentProperties {
        partial: serde_json::Value,
        response: oneshot::Sender<ApiResult<serde_json::Value>>,
    },
    ListToolbars(oneshot::Sender<Vec<ToolbarDetails>>),
    SetToolbarLayout {
        updates: Vec<ToolbarLayoutUpdate>,
        response: oneshot::Sender<ApiResult<Vec<ToolbarDetails>>>,
    },
    ListCommands(oneshot::Sender<Value>),
    InvokeCommand {
        command_id: String,
        parameters: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    PrepareSiteSurface {
        request: PrepareSiteSurfaceRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::command_registry::CommandResult>>,
    },
    GetEditingContext(oneshot::Sender<EditingContextInfo>),
    EnterGroup {
        element_id: u64,
        response: oneshot::Sender<ApiResult<EditingContextInfo>>,
    },
    ExitGroup(oneshot::Sender<ApiResult<EditingContextInfo>>),
    ListGroupMembers {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<GroupMemberEntry>>>,
    },
    // --- Layer Management ---
    ListLayers(oneshot::Sender<Vec<LayerInfo>>),
    SetLayerVisibility {
        name: String,
        visible: bool,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    SetLayerLocked {
        name: String,
        locked: bool,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    AssignLayer {
        element_id: u64,
        layer_name: String,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    CreateLayer {
        name: String,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    // --- Selection ---
    GetSelection(oneshot::Sender<Vec<u64>>),
    SetSelection {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    // --- Face Subdivision ---
    SplitBoxFace {
        element_id: u64,
        face_id: u32,
        split_position: f32,
        response: oneshot::Sender<ApiResult<SplitResult>>,
    },
    // --- Screenshot ---
    TakeScreenshot {
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    SaveProject {
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    FrameModel {
        response: oneshot::Sender<ApiResult<BoundingBox>>,
    },
    FrameEntities {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<BoundingBox>>,
    },
    LoadProject {
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    // --- Semantic Assembly / Relation ---
    ListVocabulary(oneshot::Sender<VocabularyInfo>),
    CreateAssembly {
        request: CreateAssemblyRequest,
        response: oneshot::Sender<ApiResult<CreateAssemblyResult>>,
    },
    GetAssembly {
        element_id: u64,
        response: oneshot::Sender<ApiResult<AssemblyDetails>>,
    },
    ListAssemblies(oneshot::Sender<Vec<AssemblyEntry>>),
    QueryRelations {
        source: Option<u64>,
        target: Option<u64>,
        relation_type: Option<String>,
        response: oneshot::Sender<Vec<RelationEntry>>,
    },
    ListAssemblyMembers {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<AssemblyMemberEntry>>>,
    },
}

#[cfg(feature = "model-api")]
fn poll_model_api_requests(world: &mut World) {
    loop {
        let Some(request) = next_model_api_request(world) else {
            break;
        };
        handle_model_api_request(world, request);
    }
}

#[cfg(feature = "model-api")]
fn next_model_api_request(world: &World) -> Option<ModelApiRequest> {
    let receiver = world.get_resource::<ModelApiReceiver>()?;
    let guard = receiver.0.lock().ok()?;
    match guard.try_recv() {
        Ok(request) => Some(request),
        Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => None,
    }
}

#[cfg(feature = "model-api")]
type ApiResult<T> = Result<T, String>;

#[cfg(feature = "model-api")]
fn handle_model_api_request(world: &mut World, request: ModelApiRequest) {
    match request {
        ModelApiRequest::ListEntities(response) => {
            let _ = response.send(list_entities(world));
        }
        ModelApiRequest::GetEntity {
            element_id,
            response,
        } => {
            let _ = response.send(get_entity_snapshot(world, ElementId(element_id)));
        }
        ModelApiRequest::GetEntityDetails {
            element_id,
            response,
        } => {
            let _ = response.send(get_entity_details(world, ElementId(element_id)));
        }
        ModelApiRequest::ModelSummary(response) => {
            let _ = response.send(model_summary(world));
        }
        ModelApiRequest::ListImporters(response) => {
            let importers = world.resource::<ImportRegistry>().list_importers();
            let _ = response.send(importers);
        }
        ModelApiRequest::CreateEntity { json, response } => {
            let _ = response.send(handle_create_entity(world, json));
        }
        ModelApiRequest::ImportFile {
            path,
            format_hint,
            response,
        } => {
            let _ = response.send(handle_import_file(world, &path, format_hint.as_deref()));
        }
        ModelApiRequest::DeleteEntities {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_delete_entities(world, element_ids));
        }
        ModelApiRequest::Transform { request, response } => {
            let _ = response.send(handle_transform(world, request));
        }
        ModelApiRequest::SetProperty {
            element_id,
            property_name,
            value,
            response,
        } => {
            let _ = response.send(handle_set_property(
                world,
                element_id,
                &property_name,
                value,
            ));
        }
        ModelApiRequest::ListHandles {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_handles(world, element_id));
        }
        ModelApiRequest::GetDocumentProperties(response) => {
            let props = world.resource::<DocumentProperties>();
            let json = serde_json::to_value(props.clone()).unwrap_or_default();
            let _ = response.send(json);
        }
        ModelApiRequest::SetDocumentProperties { partial, response } => {
            let _ = response.send(handle_set_document_properties(world, partial));
        }
        ModelApiRequest::ListToolbars(response) => {
            let _ = response.send(list_toolbars(world));
        }
        ModelApiRequest::SetToolbarLayout { updates, response } => {
            let _ = response.send(handle_set_toolbar_layout(world, updates));
        }
        ModelApiRequest::ListCommands(response) => {
            let schema = world
                .resource::<crate::plugins::command_registry::CommandRegistry>()
                .export_schema();
            let _ = response.send(schema);
        }
        ModelApiRequest::InvokeCommand {
            command_id,
            parameters,
            response,
        } => {
            let _ = response.send(handle_invoke_command(world, &command_id, parameters));
        }
        ModelApiRequest::PrepareSiteSurface { request, response } => {
            let _ = response.send(handle_prepare_site_surface(world, request));
        }
        ModelApiRequest::GetEditingContext(response) => {
            let _ = response.send(get_editing_context(world));
        }
        ModelApiRequest::EnterGroup {
            element_id,
            response,
        } => {
            let _ = response.send(handle_enter_group(world, element_id));
        }
        ModelApiRequest::ExitGroup(response) => {
            let _ = response.send(handle_exit_group(world));
        }
        ModelApiRequest::ListGroupMembers {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_group_members(world, element_id));
        }
        // --- Layer Management ---
        ModelApiRequest::ListLayers(response) => {
            let _ = response.send(handle_list_layers(world));
        }
        ModelApiRequest::SetLayerVisibility {
            name,
            visible,
            response,
        } => {
            let _ = response.send(handle_set_layer_visibility(world, &name, visible));
        }
        ModelApiRequest::SetLayerLocked {
            name,
            locked,
            response,
        } => {
            let _ = response.send(handle_set_layer_locked(world, &name, locked));
        }
        ModelApiRequest::AssignLayer {
            element_id,
            layer_name,
            response,
        } => {
            let _ = response.send(handle_assign_layer(world, element_id, &layer_name));
        }
        ModelApiRequest::CreateLayer { name, response } => {
            let _ = response.send(handle_create_layer(world, &name));
        }
        // --- Selection ---
        ModelApiRequest::GetSelection(response) => {
            let _ = response.send(handle_get_selection(world));
        }
        ModelApiRequest::SetSelection {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_set_selection(world, element_ids));
        }
        // --- Face Subdivision ---
        ModelApiRequest::SplitBoxFace {
            element_id,
            face_id,
            split_position,
            response,
        } => {
            let _ = response.send(handle_split_box_face(
                world,
                element_id,
                face_id,
                split_position,
            ));
        }
        // --- Screenshot ---
        ModelApiRequest::TakeScreenshot { path, response } => {
            let _ = response.send(handle_take_screenshot(world, &path));
        }
        ModelApiRequest::SaveProject { path, response } => {
            let _ = response.send(handle_save_project(world, &path));
        }
        ModelApiRequest::FrameModel { response } => {
            let _ = response.send(handle_frame_model(world));
        }
        ModelApiRequest::FrameEntities {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_frame_entities(world, &element_ids));
        }
        ModelApiRequest::LoadProject { path, response } => {
            let _ = response.send(handle_load_project(world, &path));
        }
        // --- Semantic Assembly / Relation ---
        ModelApiRequest::ListVocabulary(response) => {
            let _ = response.send(handle_list_vocabulary(world));
        }
        ModelApiRequest::CreateAssembly { request, response } => {
            let _ = response.send(handle_create_assembly(world, request));
        }
        ModelApiRequest::GetAssembly {
            element_id,
            response,
        } => {
            let _ = response.send(handle_get_assembly(world, element_id));
        }
        ModelApiRequest::ListAssemblies(response) => {
            let _ = response.send(handle_list_assemblies(world));
        }
        ModelApiRequest::QueryRelations {
            source,
            target,
            relation_type,
            response,
        } => {
            let _ = response.send(handle_query_relations(world, source, target, relation_type));
        }
        ModelApiRequest::ListAssemblyMembers {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_assembly_members(world, element_id));
        }
    }
}

const MODEL_API_HTTP_PORT: u16 = 24842;

#[cfg(feature = "model-api")]
fn spawn_model_api_server(sender: mpsc::Sender<ModelApiRequest>) {
    let http_sender = sender.clone();

    // Stdio transport (existing)
    let spawn_result = thread::Builder::new()
        .name("talos3d-model-api".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("failed to build model API runtime: {error}");
                    return;
                }
            };

            runtime.block_on(async move {
                let server = ModelApiServer::new(sender);
                let transport = transport::stdio();
                match server.serve(transport).await {
                    Ok(service) => {
                        if let Err(error) = service.waiting().await {
                            eprintln!("model API server failed while waiting: {error}");
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if !message.contains("connection closed") {
                            eprintln!("failed to start model API server: {message}");
                        }
                    }
                }
            });
        });

    if let Err(error) = spawn_result {
        eprintln!("failed to spawn model API server thread: {error}");
    }

    // HTTP transport for streamable MCP clients
    let spawn_result = thread::Builder::new()
        .name("talos3d-model-api-http".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("failed to build model API HTTP runtime: {error}");
                    return;
                }
            };

            runtime.block_on(async move {
                let ct = tokio_util::sync::CancellationToken::new();
                let sender = http_sender;
                let config = StreamableHttpServerConfig {
                    stateful_mode: false,
                    json_response: true,
                    cancellation_token: ct.clone(),
                    ..Default::default()
                };
                let service: StreamableHttpService<ModelApiServer, LocalSessionManager> =
                    StreamableHttpService::new(
                        move || Ok(ModelApiServer::new(sender.clone())),
                        Default::default(),
                        config,
                    );

                let router = axum::Router::new().nest_service("/mcp", service);
                let addr = format!("127.0.0.1:{MODEL_API_HTTP_PORT}");
                let tcp_listener = match tokio::net::TcpListener::bind(&addr).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("failed to bind model API HTTP on {addr}: {error}");
                        return;
                    }
                };
                eprintln!("model API HTTP server listening on http://{addr}/mcp");
                if let Err(error) = axum::serve(tcp_listener, router)
                    .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                    .await
                {
                    eprintln!("model API HTTP server failed: {error}");
                }
            });
        });

    if let Err(error) = spawn_result {
        eprintln!("failed to spawn model API HTTP thread: {error}");
    }
}

#[cfg(feature = "model-api")]
#[derive(Debug, Clone)]
struct ModelApiServer {
    sender: mpsc::Sender<ModelApiRequest>,
    tool_router: ToolRouter<Self>,
}

#[cfg(feature = "model-api")]
impl ModelApiServer {
    fn new(sender: mpsc::Sender<ModelApiRequest>) -> Self {
        Self {
            sender,
            tool_router: Self::tool_router(),
        }
    }

    async fn request_list_entities(&self) -> Result<Vec<EntityEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListEntities(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_entity(
        &self,
        element_id: u64,
    ) -> Result<Option<serde_json::Value>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetEntity {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_entity_details(
        &self,
        element_id: u64,
    ) -> Result<Option<EntityDetails>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetEntityDetails {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_model_summary(&self) -> Result<ModelSummary, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ModelSummary(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_list_importers(&self) -> Result<Vec<ImporterDescriptor>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListImporters(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_create_entity(&self, json: Value) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateEntity { json, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_import_file(
        &self,
        path: String,
        format_hint: Option<String>,
    ) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ImportFile {
                path,
                format_hint,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_entities(&self, element_ids: Vec<u64>) -> ApiResult<usize> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteEntities {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_transform(&self, request: TransformToolRequest) -> ApiResult<Vec<Value>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::Transform { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_property(
        &self,
        element_id: u64,
        property_name: String,
        value: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetProperty {
                element_id,
                property_name,
                value,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_handles(&self, element_id: u64) -> ApiResult<Vec<HandleInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListHandles {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_document_properties(&self) -> Result<serde_json::Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDocumentProperties(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_document_properties(
        &self,
        partial: serde_json::Value,
    ) -> ApiResult<serde_json::Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetDocumentProperties { partial, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_toolbars(&self) -> Result<Vec<ToolbarDetails>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListToolbars(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_toolbar_layout(
        &self,
        updates: Vec<ToolbarLayoutUpdate>,
    ) -> ApiResult<Vec<ToolbarDetails>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetToolbarLayout { updates, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_commands(&self) -> Result<Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListCommands(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_invoke_command(
        &self,
        command_id: String,
        parameters: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InvokeCommand {
                command_id,
                parameters,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_prepare_site_surface(
        &self,
        request: PrepareSiteSurfaceRequest,
    ) -> ApiResult<crate::plugins::command_registry::CommandResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PrepareSiteSurface { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_editing_context(&self) -> Result<EditingContextInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetEditingContext(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_enter_group(&self, element_id: u64) -> ApiResult<EditingContextInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::EnterGroup {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_exit_group(&self) -> ApiResult<EditingContextInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExitGroup(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_group_members(
        &self,
        element_id: u64,
    ) -> ApiResult<Vec<GroupMemberEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListGroupMembers {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Layer Management ---

    async fn request_list_layers(&self) -> Result<Vec<LayerInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListLayers(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_layer_visibility(
        &self,
        name: String,
        visible: bool,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetLayerVisibility {
                name,
                visible,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_layer_locked(
        &self,
        name: String,
        locked: bool,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetLayerLocked {
                name,
                locked,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_assign_layer(
        &self,
        element_id: u64,
        layer_name: String,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AssignLayer {
                element_id,
                layer_name,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_layer(&self, name: String) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateLayer { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Selection ---

    async fn request_get_selection(&self) -> Result<Vec<u64>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetSelection(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_selection(&self, element_ids: Vec<u64>) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetSelection {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Face Subdivision ---

    async fn request_split_box_face(
        &self,
        element_id: u64,
        face_id: u32,
        split_position: f32,
    ) -> ApiResult<SplitResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SplitBoxFace {
                element_id,
                face_id,
                split_position,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Screenshot ---

    async fn request_take_screenshot(&self, path: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::TakeScreenshot { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_save_project(&self, path: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SaveProject { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_frame_model(&self) -> ApiResult<BoundingBox> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::FrameModel { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_frame_entities(&self, element_ids: Vec<u64>) -> ApiResult<BoundingBox> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::FrameEntities {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_load_project(&self, path: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::LoadProject { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Semantic Assembly / Relation requests ---

    async fn request_list_vocabulary(&self) -> Result<VocabularyInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListVocabulary(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_create_assembly(
        &self,
        request: CreateAssemblyRequest,
    ) -> ApiResult<CreateAssemblyResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateAssembly { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_assembly(&self, element_id: u64) -> ApiResult<AssemblyDetails> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetAssembly {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_assemblies(&self) -> Result<Vec<AssemblyEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListAssemblies(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_query_relations(
        &self,
        source: Option<u64>,
        target: Option<u64>,
        relation_type: Option<String>,
    ) -> Result<Vec<RelationEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::QueryRelations {
                source,
                target,
                relation_type,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_list_assembly_members(
        &self,
        element_id: u64,
    ) -> ApiResult<Vec<AssemblyMemberEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListAssemblyMembers {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }
}

#[cfg(feature = "model-api")]
fn json_tool_result<T: Serialize>(value: T) -> Result<CallToolResult, McpError> {
    let content = Content::json(value)?;
    Ok(CallToolResult::success(vec![content]))
}

#[cfg(feature = "model-api")]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for ModelApiServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some("Read and write access to the Talos3D authored model.".into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetEntityRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeleteEntitiesRequest {
    element_ids: Vec<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrameEntitiesRequest {
    element_ids: Vec<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportFileRequest {
    path: String,
    format_hint: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformToolRequest {
    pub element_ids: Vec<u64>,
    pub operation: String,
    pub axis: Option<String>,
    pub value: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetPropertyRequest {
    element_id: u64,
    property_name: String,
    value: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListHandlesRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetToolbarLayoutRequest {
    updates: Vec<ToolbarLayoutUpdate>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvokeCommandRequest {
    command_id: String,
    #[serde(default)]
    parameters: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareSiteSurfaceRequest {
    #[serde(default)]
    pub source_element_ids: Vec<u64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub delete_source: bool,
    #[serde(default = "default_true")]
    pub center_at_origin: bool,
    #[serde(default)]
    pub contour_layers: Vec<String>,
    #[serde(default)]
    pub join_tolerance: Option<f32>,
    #[serde(default)]
    pub drape_sample_spacing: Option<f32>,
    #[serde(default)]
    pub max_triangle_area: Option<f32>,
    #[serde(default)]
    pub minimum_angle: Option<f32>,
    #[serde(default)]
    pub contour_interval: Option<f32>,
}

#[cfg(feature = "model-api")]
fn default_true() -> bool {
    true
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnterGroupRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListGroupMembersRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetLayerVisibilityRequest {
    name: String,
    visible: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetLayerLockedRequest {
    name: String,
    locked: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssignLayerRequest {
    element_id: u64,
    layer_name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateLayerRequest {
    name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetSelectionRequest {
    element_ids: Vec<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SplitBoxFaceRequest {
    element_id: u64,
    face_id: u32,
    /// Split position from 0.0 to 1.0 along the split axis.
    split_position: f32,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TakeScreenshotRequest {
    /// File path to save the screenshot. Defaults to /tmp/talos_screenshot.png.
    #[serde(default = "default_screenshot_path")]
    path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SaveProjectRequest {
    path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoadProjectRequest {
    path: String,
}

#[cfg(feature = "model-api")]
fn default_screenshot_path() -> String {
    "/tmp/talos_screenshot.png".to_string()
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolbarLayoutUpdate {
    toolbar_id: String,
    dock: Option<String>,
    order: Option<u32>,
    visible: Option<bool>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct HandlePosition {
    x: f32,
    y: f32,
    z: f32,
}

#[cfg(feature = "model-api")]
impl From<Vec3> for HandlePosition {
    fn from(position: Vec3) -> Self {
        Self {
            x: position.x,
            y: position.y,
            z: position.z,
        }
    }
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandleInfo {
    id: String,
    position: HandlePosition,
    kind: String,
    label: String,
}

// --- Assembly / Relation request types ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAssemblyRequest {
    pub assembly_type: String,
    pub label: String,
    pub members: Vec<AssemblyMemberRefRequest>,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub relations: Vec<CreateRelationRequest>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyMemberRefRequest {
    pub target: u64,
    pub role: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRelationRequest {
    pub source: u64,
    pub target: u64,
    #[serde(rename = "type")]
    pub relation_type: String,
    #[serde(default)]
    pub parameters: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetAssemblyRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryRelationsRequest {
    source: Option<u64>,
    target: Option<u64>,
    relation_type: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListAssemblyMembersRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[tool_router(router = tool_router)]
impl ModelApiServer {
    #[tool(
        name = "list_entities",
        description = "List all authored entities in the model."
    )]
    async fn list_entities_tool(&self) -> Result<CallToolResult, McpError> {
        let entities = self
            .request_list_entities()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(entities)
    }

    #[tool(
        name = "get_entity",
        description = "Get a full entity snapshot by element ID."
    )]
    async fn get_entity_tool(
        &self,
        Parameters(params): Parameters<GetEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snapshot = self
            .request_get_entity(params.element_id)
            .await
            .map_err(|error| McpError::internal_error(error, None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("entity {} not found", params.element_id), None)
            })?;
        json_tool_result(snapshot)
    }

    #[tool(
        name = "get_entity_details",
        description = "Get an entity snapshot plus a normalized property list by element ID."
    )]
    async fn get_entity_details_tool(
        &self,
        Parameters(params): Parameters<GetEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .request_get_entity_details(params.element_id)
            .await
            .map_err(|error| McpError::internal_error(error, None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("entity {} not found", params.element_id), None)
            })?;
        json_tool_result(details)
    }

    #[tool(
        name = "model_summary",
        description = "Get aggregate information about the authored model."
    )]
    async fn model_summary_tool(&self) -> Result<CallToolResult, McpError> {
        let summary = self
            .request_model_summary()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(summary)
    }

    #[tool(
        name = "list_importers",
        description = "List all registered file importers."
    )]
    async fn list_importers_tool(&self) -> Result<CallToolResult, McpError> {
        let importers = self
            .request_list_importers()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(importers)
    }

    #[tool(
        name = "create_entity",
        description = "Create an authored entity from a typed JSON object."
    )]
    async fn create_entity_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_create_entity(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "import_file",
        description = "Import a supported file from disk and return the created entity IDs."
    )]
    async fn import_file_tool(
        &self,
        Parameters(params): Parameters<ImportFileRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_ids = self
            .request_import_file(params.path, params.format_hint)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_ids)
    }

    #[tool(
        name = "delete_entities",
        description = "Delete one or more entities by element ID."
    )]
    async fn delete_entities_tool(
        &self,
        Parameters(params): Parameters<DeleteEntitiesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let deleted_count = self
            .request_delete_entities(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(deleted_count)
    }

    #[tool(
        name = "transform",
        description = "Move, rotate, or scale entities through the command pipeline."
    )]
    async fn transform_tool(
        &self,
        Parameters(params): Parameters<TransformToolRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snapshots = self
            .request_transform(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(snapshots)
    }

    #[tool(
        name = "set_property",
        description = "Set a single authored property on an entity."
    )]
    async fn set_property_tool(
        &self,
        Parameters(params): Parameters<SetPropertyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snapshot = self
            .request_set_property(params.element_id, params.property_name, params.value)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(snapshot)
    }

    #[tool(
        name = "set_entity_property",
        description = "Set a single authored property on an entity and return the updated snapshot."
    )]
    async fn set_entity_property_tool(
        &self,
        Parameters(params): Parameters<SetPropertyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snapshot = self
            .request_set_property(params.element_id, params.property_name, params.value)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(snapshot)
    }

    #[tool(
        name = "list_handles",
        description = "List the read-only edit handles for an entity."
    )]
    async fn list_handles_tool(
        &self,
        Parameters(params): Parameters<ListHandlesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let handles = self
            .request_list_handles(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(handles)
    }

    #[tool(
        name = "get_document_properties",
        description = "Get the current document properties (units, grid, snap, domain defaults)."
    )]
    async fn get_document_properties_tool(&self) -> Result<CallToolResult, McpError> {
        let props = self
            .request_get_document_properties()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(props)
    }

    #[tool(
        name = "set_document_properties",
        description = "Merge partial JSON into document properties. Only provided fields are updated."
    )]
    async fn set_document_properties_tool(
        &self,
        Parameters(partial): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let updated = self
            .request_set_document_properties(partial)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(updated)
    }

    #[tool(
        name = "list_toolbars",
        description = "List registered toolbars, their sections, and current layout state."
    )]
    async fn list_toolbars_tool(&self) -> Result<CallToolResult, McpError> {
        let toolbars = self
            .request_list_toolbars()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(toolbars)
    }

    #[tool(
        name = "set_toolbar_layout",
        description = "Update toolbar dock, order, or visibility and return the resulting layout."
    )]
    async fn set_toolbar_layout_tool(
        &self,
        Parameters(params): Parameters<SetToolbarLayoutRequest>,
    ) -> Result<CallToolResult, McpError> {
        let toolbars = self
            .request_set_toolbar_layout(params.updates)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(toolbars)
    }

    #[tool(
        name = "list_commands",
        description = "List all registered commands with their descriptors, parameter schemas, and capability ownership."
    )]
    async fn list_commands_tool(&self) -> Result<CallToolResult, McpError> {
        let commands = self
            .request_list_commands()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(commands)
    }

    #[tool(
        name = "invoke_command",
        description = "Execute a registered command by ID with optional parameters. Returns a CommandResult with created/modified/deleted entity IDs."
    )]
    async fn invoke_command_tool(
        &self,
        Parameters(params): Parameters<InvokeCommandRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_invoke_command(params.command_id, params.parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "prepare_site_surface",
        description = "Repair selected or explicitly listed contour entities, create elevation curves, and generate a draped terrain surface. This wraps the terrain.prepare_site_surface command in a dedicated MCP tool."
    )]
    async fn prepare_site_surface_tool(
        &self,
        Parameters(params): Parameters<PrepareSiteSurfaceRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_prepare_site_surface(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "get_editing_context",
        description = "Get the current group editing context: whether at root or inside a group, with a breadcrumb path."
    )]
    async fn get_editing_context_tool(&self) -> Result<CallToolResult, McpError> {
        let context = self
            .request_get_editing_context()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(context)
    }

    #[tool(
        name = "enter_group",
        description = "Enter a group for editing. Only the group's direct children become selectable. Returns the updated editing context."
    )]
    async fn enter_group_tool(
        &self,
        Parameters(params): Parameters<EnterGroupRequest>,
    ) -> Result<CallToolResult, McpError> {
        let context = self
            .request_enter_group(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(context)
    }

    #[tool(
        name = "exit_group",
        description = "Exit the current group editing context and return to its parent. At root level this is a no-op."
    )]
    async fn exit_group_tool(&self) -> Result<CallToolResult, McpError> {
        let context = self
            .request_exit_group()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(context)
    }

    #[tool(
        name = "list_group_members",
        description = "List the direct members of a group by element ID."
    )]
    async fn list_group_members_tool(
        &self,
        Parameters(params): Parameters<ListGroupMembersRequest>,
    ) -> Result<CallToolResult, McpError> {
        let members = self
            .request_list_group_members(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(members)
    }

    // --- Layer Management ---

    #[tool(
        name = "list_layers",
        description = "List all layers with their visibility, locked state, color, and whether each is the active layer."
    )]
    async fn list_layers_tool(&self) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_list_layers()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "set_layer_visibility",
        description = "Toggle a layer's visibility on or off."
    )]
    async fn set_layer_visibility_tool(
        &self,
        Parameters(params): Parameters<SetLayerVisibilityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_set_layer_visibility(params.name, params.visible)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "set_layer_locked",
        description = "Toggle a layer's locked state. Locked layers block selection and editing."
    )]
    async fn set_layer_locked_tool(
        &self,
        Parameters(params): Parameters<SetLayerLockedRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_set_layer_locked(params.name, params.locked)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "assign_layer",
        description = "Move an entity to a different layer by name."
    )]
    async fn assign_layer_tool(
        &self,
        Parameters(params): Parameters<AssignLayerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_assign_layer(params.element_id, params.layer_name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "create_layer",
        description = "Create a new layer. Returns the updated layer list."
    )]
    async fn create_layer_tool(
        &self,
        Parameters(params): Parameters<CreateLayerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_create_layer(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    // --- Selection ---

    #[tool(
        name = "get_selection",
        description = "Get the element IDs of all currently selected entities."
    )]
    async fn get_selection_tool(&self) -> Result<CallToolResult, McpError> {
        let selection = self
            .request_get_selection()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(selection)
    }

    #[tool(
        name = "set_selection",
        description = "Replace the current selection with the given element IDs."
    )]
    async fn set_selection_tool(
        &self,
        Parameters(params): Parameters<SetSelectionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let selection = self
            .request_set_selection(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(selection)
    }

    // --- Face Subdivision ---

    #[tool(
        name = "split_box_face",
        description = "Split a box entity into two boxes along a face axis. face_id 0-5 maps to -X,+X,-Y,+Y,-Z,+Z. split_position is 0.0-1.0 along the split axis. Returns the new element IDs for the two boxes and the CompositeSolid group."
    )]
    async fn split_box_face_tool(
        &self,
        Parameters(params): Parameters<SplitBoxFaceRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_split_box_face(params.element_id, params.face_id, params.split_position)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- Screenshot ---

    #[tool(
        name = "take_screenshot",
        description = "Capture a screenshot of the 3D viewport and save it to disk. The screenshot captures the rendered 3D scene (without egui UI overlays). Returns the file path where the screenshot was saved."
    )]
    async fn take_screenshot_tool(
        &self,
        Parameters(params): Parameters<TakeScreenshotRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_take_screenshot(params.path)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    #[tool(
        name = "frame_model",
        description = "Frame the orbit camera around the authored model and return the fitted bounding box."
    )]
    async fn frame_model_tool(&self) -> Result<CallToolResult, McpError> {
        let bounds = self
            .request_frame_model()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(bounds)
    }

    #[tool(
        name = "frame_entities",
        description = "Frame the orbit camera around the given authored entities and return the fitted bounding box."
    )]
    async fn frame_entities_tool(
        &self,
        Parameters(params): Parameters<FrameEntitiesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let bounds = self
            .request_frame_entities(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(bounds)
    }

    #[tool(
        name = "save_project",
        description = "Save the current Talos3D project to a specific path on disk and return the resolved file path."
    )]
    async fn save_project_tool(
        &self,
        Parameters(params): Parameters<SaveProjectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_save_project(params.path)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    #[tool(
        name = "load_project",
        description = "Load a Talos3D project from a specific path on disk and return the resolved file path."
    )]
    async fn load_project_tool(
        &self,
        Parameters(params): Parameters<LoadProjectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_load_project(params.path)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    // --- Semantic Assembly / Relation tools ---

    #[tool(
        name = "list_vocabulary",
        description = "List all registered assembly types and relation types. This is how AI discovers what domain concepts are available."
    )]
    async fn list_vocabulary_tool(&self) -> Result<CallToolResult, McpError> {
        let vocab = self
            .request_list_vocabulary()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(vocab)
    }

    #[tool(
        name = "create_assembly",
        description = "Create a semantic assembly with typed members and optionally create relations. The entire operation is one undoable unit."
    )]
    async fn create_assembly_tool(
        &self,
        Parameters(params): Parameters<CreateAssemblyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_create_assembly(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "get_assembly",
        description = "Get full details of a semantic assembly by element ID, including members enriched with entity type and label."
    )]
    async fn get_assembly_tool(
        &self,
        Parameters(params): Parameters<GetAssemblyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .request_get_assembly(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(details)
    }

    #[tool(
        name = "list_assemblies",
        description = "List all semantic assemblies in the model with their type, label, and member count."
    )]
    async fn list_assemblies_tool(&self) -> Result<CallToolResult, McpError> {
        let assemblies = self
            .request_list_assemblies()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(assemblies)
    }

    #[tool(
        name = "query_relations",
        description = "Query semantic relations, optionally filtering by source element ID, target element ID, or relation type."
    )]
    async fn query_relations_tool(
        &self,
        Parameters(params): Parameters<QueryRelationsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let relations = self
            .request_query_relations(params.source, params.target, params.relation_type)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(relations)
    }

    #[tool(
        name = "list_assembly_members",
        description = "List the members of a specific assembly with their roles, types, and labels."
    )]
    async fn list_assembly_members_tool(
        &self,
        Parameters(params): Parameters<ListAssemblyMembersRequest>,
    ) -> Result<CallToolResult, McpError> {
        let members = self
            .request_list_assembly_members(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(members)
    }
}

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
    let mut registry = world.resource_mut::<LayerRegistry>();
    let def = registry
        .layers
        .get_mut(name)
        .ok_or_else(|| format!("Layer '{name}' not found"))?;
    def.visible = visible;
    drop(registry);
    Ok(handle_list_layers(world))
}

#[cfg(feature = "model-api")]
fn handle_set_layer_locked(
    world: &mut World,
    name: &str,
    locked: bool,
) -> Result<Vec<LayerInfo>, String> {
    let mut registry = world.resource_mut::<LayerRegistry>();
    let def = registry
        .layers
        .get_mut(name)
        .ok_or_else(|| format!("Layer '{name}' not found"))?;
    def.locked = locked;
    drop(registry);
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
    let mut registry = world.resource_mut::<LayerRegistry>();
    if registry.layers.contains_key(name) {
        return Err(format!("Layer '{name}' already exists"));
    }
    registry.create_layer(name.to_string());
    drop(registry);
    Ok(handle_list_layers(world))
}

// --- Selection Handlers ---

#[cfg(feature = "model-api")]
fn handle_get_selection(world: &mut World) -> Vec<u64> {
    let mut query = world.query_filtered::<&ElementId, With<Selected>>();
    query.iter(world).map(|id| id.0).collect()
}

#[cfg(feature = "model-api")]
fn handle_set_selection(world: &mut World, element_ids: Vec<u64>) -> Result<Vec<u64>, String> {
    use std::collections::HashSet;

    let target_ids: HashSet<ElementId> = element_ids.iter().copied().map(ElementId).collect();

    // Verify all target entities exist
    for eid in &target_ids {
        ensure_entity_exists(world, *eid)?;
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
    };
    let snapshot_b: PrimitiveSnapshot<BoxPrimitive> = PrimitiveSnapshot {
        element_id: id_b,
        primitive: prim_b,
        rotation,
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
    use bevy::render::view::screenshot::{save_to_disk, Screenshot};
    use std::path::PathBuf;

    let path_buf = PathBuf::from(path);
    let path_owned = path.to_string();
    world
        .commands()
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path_buf));
    world.flush();

    Ok(path_owned)
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
    let bounds = aggregate_snapshot_bounds(snapshots.iter().map(|(_, snapshot)| snapshot))
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
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
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
    use crate::plugins::command_registry::{CommandRegistry, CommandResult};

    let handler = world
        .resource::<CommandRegistry>()
        .handler_for(command_id)
        .ok_or_else(|| format!("Unknown command: {command_id}"))?;
    let result: CommandResult = handler(world, &parameters)?;
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

// --- Semantic Assembly / Relation handlers ---

#[cfg(feature = "model-api")]
fn handle_list_vocabulary(world: &World) -> VocabularyInfo {
    let registry = world.resource::<CapabilityRegistry>();
    VocabularyInfo {
        assembly_types: registry.assembly_type_descriptors().to_vec(),
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

#[cfg(feature = "model-api")]
fn flush_model_api_write_pipeline(world: &mut World) {
    queue_command_events(world);
    apply_pending_history_commands(world);
}

#[cfg(feature = "model-api")]
pub fn handle_create_entity(world: &mut World, json: Value) -> Result<u64, String> {
    let object = json
        .as_object()
        .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
    let entity_type = required_string(object, "type")?.to_ascii_lowercase();
    let registry = world.resource::<CapabilityRegistry>();
    let factory = registry.factory_for(&entity_type).ok_or_else(|| {
        let valid_types: Vec<&str> = registry.factories().iter().map(|f| f.type_name()).collect();
        format!(
            "Invalid entity type '{entity_type}'. Valid types: {}",
            valid_types.join(", ")
        )
    })?;
    let snapshot = factory.from_create_request(world, &json)?;
    let element_id = snapshot.element_id();
    send_event(
        world,
        crate::plugins::commands::CreateEntityCommand { snapshot },
    );

    flush_model_api_write_pipeline(world);

    get_entity_snapshot(world, element_id)
        .map(|_| element_id.0)
        .ok_or_else(|| format!("Failed to create entity of type '{entity_type}'"))
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
        ensure_entity_exists(world, *element_id)?;
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
        format!("Invalid toolbar dock: {value}. Expected one of top, bottom, left, right")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::CapabilityRegistry;
    #[cfg(feature = "model-api")]
    use crate::importers::obj::ObjImporter;
    #[cfg(feature = "model-api")]
    use crate::plugins::modeling::primitives::CylinderPrimitive;
    #[cfg(feature = "model-api")]
    use crate::plugins::modeling::snapshots::TriangleMeshFactory;
    use crate::plugins::modeling::{
        generic_factory::PrimitiveFactory,
        primitives::{BoxPrimitive, PlanePrimitive, Polyline, ShapeRotation},
        snapshots::PolylineFactory,
    };
    #[cfg(feature = "model-api")]
    use crate::plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        document_properties::DocumentProperties,
        history::{History, PendingCommandQueue},
        identity::ElementIdAllocator,
        import::ImportRegistry,
        toolbar::{
            ToolbarDescriptor, ToolbarDock, ToolbarLayoutEntry, ToolbarLayoutState,
            ToolbarRegistry, ToolbarSection,
        },
    };
    use serde_json::json;
    #[cfg(feature = "model-api")]
    use serde_json::Value;
    #[cfg(feature = "model-api")]
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn list_entities_and_model_summary_reflect_authored_world() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
        registry.register_factory(PolylineFactory);
        world.insert_resource(registry);

        world.spawn((
            ElementId(1),
            BoxPrimitive {
                centre: Vec3::new(2.0, 1.0, 1.5),
                half_extents: Vec3::new(0.5, 0.5, 0.5),
            },
            ShapeRotation::default(),
        ));
        world.spawn((
            ElementId(2),
            PlanePrimitive {
                corner_a: Vec2::new(-1.0, -2.0),
                corner_b: Vec2::new(3.0, 2.0),
                elevation: 0.4,
            },
            ShapeRotation(Quat::from_rotation_y(0.2)),
        ));
        world.spawn((
            ElementId(3),
            Polyline {
                points: vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 1.0)],
            },
        ));

        let entities = list_entities(&world);
        assert_eq!(entities.len(), 3);
        assert_eq!(entities[0].entity_type, "box");
        assert_eq!(entities[1].entity_type, "plane");
        assert_eq!(entities[2].entity_type, "polyline");

        let summary = model_summary(&world);
        assert_eq!(summary.entity_counts.get("box"), Some(&1));
        assert_eq!(summary.entity_counts.get("plane"), Some(&1));
        assert_eq!(summary.entity_counts.get("polyline"), Some(&1));
        assert!(summary.bounding_box.is_some());
    }

    #[test]
    fn get_entity_snapshot_returns_serialized_snapshot() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
        registry.register_factory(PolylineFactory);
        world.insert_resource(registry);

        world.spawn((
            ElementId(7),
            PlanePrimitive {
                corner_a: Vec2::new(-1.0, -2.0),
                corner_b: Vec2::new(3.0, 2.0),
                elevation: 0.4,
            },
            ShapeRotation(Quat::from_rotation_y(0.2)),
        ));

        let snapshot =
            get_entity_snapshot(&world, ElementId(7)).expect("plane snapshot should be present");

        // PrimitiveSnapshot::to_json() serialises the primitive itself.
        let expected = serde_json::to_value(&PlanePrimitive {
            corner_a: Vec2::new(-1.0, -2.0),
            corner_b: Vec2::new(3.0, 2.0),
            elevation: 0.4,
        })
        .unwrap();

        assert_eq!(snapshot, expected);
        assert!(get_entity_snapshot(&world, ElementId(999)).is_none());
    }

    #[test]
    fn get_entity_details_returns_normalized_property_list() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        world.insert_resource(registry);

        world.spawn((
            ElementId(3),
            BoxPrimitive {
                centre: Vec3::new(1.0, 2.0, 3.0),
                half_extents: Vec3::new(4.0, 5.0, 6.0),
            },
            ShapeRotation::default(),
        ));

        let details =
            get_entity_details(&world, ElementId(3)).expect("box details should be present");

        assert_eq!(details.entity_type, "box");
        assert_eq!(
            details
                .geometry_semantics
                .as_ref()
                .map(|semantics| &semantics.role),
            Some(&crate::plugins::modeling::semantics::GeometryRole::SolidRoot)
        );
        assert_eq!(
            details
                .geometry_semantics
                .as_ref()
                .map(|semantics| semantics
                    .evaluated_body
                    .as_ref()
                    .and_then(|body| body.volume))
                .flatten(),
            Some(960.0)
        );
        assert_eq!(details.properties.len(), 2);
        assert_eq!(details.properties[0].name, "center");
        assert_eq!(details.properties[0].kind, "vec3");
        assert_eq!(details.properties[0].value, json!([1.0, 2.0, 3.0]));
        assert!(details.properties[0].editable);
    }

    #[cfg(feature = "model-api")]
    fn init_model_api_test_world() -> World {
        let mut world = World::new();
        world.insert_resource(Events::<CreateBoxCommand>::default());
        world.insert_resource(Events::<CreateCylinderCommand>::default());
        world.insert_resource(Events::<CreatePlaneCommand>::default());
        world.insert_resource(Events::<CreatePolylineCommand>::default());
        world.insert_resource(Events::<CreateTriangleMeshCommand>::default());
        world.insert_resource(Events::<CreateEntityCommand>::default());
        world.insert_resource(Events::<DeleteEntitiesCommand>::default());
        world.insert_resource(Events::<ResolvedDeleteEntitiesCommand>::default());
        world.insert_resource(Events::<ApplyEntityChangesCommand>::default());
        world.insert_resource(Events::<BeginCommandGroup>::default());
        world.insert_resource(Events::<EndCommandGroup>::default());
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(History::default());
        world.insert_resource(ElementIdAllocator::default());
        let mut import_registry = ImportRegistry::default();
        import_registry.register_importer(ObjImporter);
        world.insert_resource(import_registry);
        world.insert_resource(DocumentProperties::default());
        let mut toolbar_registry = ToolbarRegistry::default();
        toolbar_registry.register(ToolbarDescriptor {
            id: "core".to_string(),
            label: "Core".to_string(),
            default_dock: ToolbarDock::Top,
            sections: vec![ToolbarSection {
                label: "Select".to_string(),
                command_ids: vec!["core.select_tool".to_string()],
            }],
        });
        toolbar_registry.register(ToolbarDescriptor {
            id: "modeling".to_string(),
            label: "Modeling".to_string(),
            default_dock: ToolbarDock::Left,
            sections: vec![ToolbarSection {
                label: "Primitives".to_string(),
                command_ids: vec!["modeling.place_box".to_string()],
            }],
        });
        world.insert_resource(toolbar_registry);
        let mut toolbar_layout_state = ToolbarLayoutState::default();
        toolbar_layout_state.entries.insert(
            "core".to_string(),
            ToolbarLayoutEntry {
                dock: ToolbarDock::Top,
                order: 0,
                visible: true,
            },
        );
        toolbar_layout_state.entries.insert(
            "modeling".to_string(),
            ToolbarLayoutEntry {
                dock: ToolbarDock::Left,
                order: 0,
                visible: true,
            },
        );
        world.insert_resource(toolbar_layout_state);
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        registry.register_factory(PrimitiveFactory::<CylinderPrimitive>::new());
        registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
        registry.register_factory(PolylineFactory);
        registry.register_factory(TriangleMeshFactory);
        world.insert_resource(registry);
        world
    }

    #[cfg(feature = "model-api")]
    fn write_temp_obj(contents: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("talos3d-model-api-{unique}.obj"));
        fs::write(&path, contents).expect("temp obj should be written");
        path
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn write_handlers_create_transform_delete_and_list_handles() {
        let mut world = init_model_api_test_world();

        let box_id = handle_create_entity(
            &mut world,
            json!({
                "type": "box",
                "centre": [1.0, 2.0, 3.0],
                "half_extents": [0.5, 0.75, 1.0]
            }),
        )
        .expect("box should be created");
        assert_eq!(box_id, 0);

        let transformed = handle_transform(
            &mut world,
            TransformToolRequest {
                element_ids: vec![box_id],
                operation: "move".to_string(),
                axis: Some("X".to_string()),
                value: json!(2.5),
            },
        )
        .expect("transform should succeed");
        assert_eq!(transformed.len(), 1);

        let box_snapshot =
            get_entity_snapshot(&world, ElementId(box_id)).expect("box snapshot should exist");
        assert_eq!(
            box_snapshot["Box"]["primitive"]["centre"],
            json!([3.5, 2.0, 3.0])
        );

        let handles = handle_list_handles(&world, box_id).expect("box handles should exist");
        assert_eq!(handles.len(), 9);
        assert_eq!(handles[0].kind, "Vertex");

        let deleted_count =
            handle_delete_entities(&mut world, vec![box_id]).expect("delete should remove the box");
        assert_eq!(deleted_count, 1);
        assert!(get_entity_snapshot(&world, ElementId(box_id)).is_none());
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn set_property_validates_entity_specific_fields() {
        let mut world = init_model_api_test_world();
        let box_id = handle_create_entity(
            &mut world,
            json!({
                "type": "box",
                "centre": [0.0, 0.0, 0.0],
                "half_extents": [1.0, 2.0, 3.0]
            }),
        )
        .expect("box should be created");

        let updated =
            handle_set_property(&mut world, box_id, "half_extents", json!([4.0, 5.0, 6.0]))
                .expect("setting box half extents should succeed");
        assert_eq!(
            updated["Box"]["primitive"]["half_extents"],
            json!([4.0, 5.0, 6.0])
        );

        let error = handle_set_property(&mut world, box_id, "radius", json!(1.0))
            .expect_err("invalid box property should fail");
        assert!(error.contains("Valid properties: centre, half_extents"));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn toolbar_handlers_list_and_update_toolbar_layout() {
        let mut world = init_model_api_test_world();

        let listed = list_toolbars(&world);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "modeling");

        let updated = handle_set_toolbar_layout(
            &mut world,
            vec![ToolbarLayoutUpdate {
                toolbar_id: "modeling".to_string(),
                dock: Some("bottom".to_string()),
                order: Some(3),
                visible: Some(false),
            }],
        )
        .expect("toolbar layout update should succeed");

        let modeling = updated
            .iter()
            .find(|toolbar| toolbar.id == "modeling")
            .expect("modeling toolbar should be listed");
        assert_eq!(modeling.dock, "bottom");
        assert_eq!(modeling.order, 3);
        assert!(!modeling.visible);

        let error = handle_set_toolbar_layout(
            &mut world,
            vec![ToolbarLayoutUpdate {
                toolbar_id: "core".to_string(),
                dock: None,
                order: None,
                visible: Some(false),
            }],
        )
        .expect_err("core toolbar should remain visible");
        assert!(error.contains("cannot be hidden"));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn poll_model_api_requests_services_channel_queries() {
        let mut world = init_model_api_test_world();
        world.spawn((
            ElementId(1),
            PlanePrimitive {
                corner_a: Vec2::ZERO,
                corner_b: Vec2::new(4.0, 2.0),
                elevation: 0.0,
            },
            ShapeRotation::default(),
        ));

        let (sender, receiver) = mpsc::channel();
        world.insert_resource(ModelApiReceiver(Mutex::new(receiver)));

        let (list_response, list_receiver) = oneshot::channel();
        sender
            .send(ModelApiRequest::ListEntities(list_response))
            .expect("list request should send");

        let (summary_response, summary_receiver) = oneshot::channel();
        sender
            .send(ModelApiRequest::ModelSummary(summary_response))
            .expect("summary request should send");

        poll_model_api_requests(&mut world);

        let list = list_receiver
            .blocking_recv()
            .expect("list response should arrive");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].entity_type, "plane");

        let summary = summary_receiver
            .blocking_recv()
            .expect("summary response should arrive");
        assert_eq!(summary.entity_counts.get("plane"), Some(&1));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn import_handlers_list_importers_and_create_triangle_meshes() {
        let mut world = init_model_api_test_world();

        let importers = world.resource::<ImportRegistry>().list_importers();
        assert_eq!(importers.len(), 1);
        assert_eq!(importers[0].format_name, "Wavefront OBJ");
        assert_eq!(importers[0].extensions, vec!["obj"]);

        let path = write_temp_obj("o Imported\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
        let imported_ids = handle_import_file(&mut world, path.to_str().unwrap_or_default(), None)
            .expect("OBJ import should succeed");
        assert_eq!(imported_ids.len(), 1);

        let snapshot = get_entity_snapshot(&world, ElementId(imported_ids[0]))
            .expect("triangle mesh snapshot should exist");
        assert_eq!(
            snapshot["TriangleMesh"]["primitive"]["name"],
            json!("Imported")
        );

        let entities = list_entities(&world);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, "triangle_mesh");

        let _ = fs::remove_file(path);
    }

    #[cfg(feature = "model-api")]
    #[tokio::test]
    async fn mcp_tools_return_structured_model_data() {
        let (sender, receiver) = mpsc::channel();
        let worker_handle = tokio::task::spawn_blocking(move || {
            let mut world = init_model_api_test_world();
            world.spawn((
                ElementId(10),
                BoxPrimitive {
                    centre: Vec3::new(1.0, 1.0, 1.0),
                    half_extents: Vec3::splat(0.5),
                },
                ShapeRotation::default(),
            ));
            world.spawn((
                ElementId(11),
                PlanePrimitive {
                    corner_a: Vec2::new(-1.0, -1.0),
                    corner_b: Vec2::new(1.0, 1.0),
                    elevation: 0.0,
                },
                ShapeRotation::default(),
            ));

            while let Ok(request) = receiver.recv() {
                handle_model_api_request(&mut world, request);
            }
        });

        let server = ModelApiServer::new(sender);
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 22);

        let listed: Vec<EntityEntry> = server
            .list_entities_tool()
            .await
            .expect("list_entities tool should succeed")
            .into_typed()
            .expect("list_entities result should deserialize");
        assert_eq!(listed.len(), 2);

        let box_snapshot: serde_json::Value = server
            .get_entity_tool(Parameters(GetEntityRequest { element_id: 10 }))
            .await
            .expect("get_entity tool should succeed")
            .into_typed()
            .expect("get_entity result should deserialize");
        assert_eq!(box_snapshot["Box"]["element_id"], 10);

        let box_details: EntityDetails = server
            .get_entity_details_tool(Parameters(GetEntityRequest { element_id: 10 }))
            .await
            .expect("get_entity_details tool should succeed")
            .into_typed()
            .expect("get_entity_details result should deserialize");
        assert_eq!(box_details.entity_type, "box");
        assert_eq!(box_details.properties.len(), 2);

        let summary: ModelSummary = server
            .model_summary_tool()
            .await
            .expect("model_summary tool should succeed")
            .into_typed()
            .expect("model_summary result should deserialize");
        assert_eq!(summary.entity_counts.get("box"), Some(&1));
        assert_eq!(summary.entity_counts.get("plane"), Some(&1));

        let importers: Vec<ImporterDescriptor> = server
            .list_importers_tool()
            .await
            .expect("list_importers tool should succeed")
            .into_typed()
            .expect("list_importers result should deserialize");
        assert_eq!(importers.len(), 1);
        assert_eq!(importers[0].format_name, "Wavefront OBJ");

        let obj_path = write_temp_obj("o FromTool\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
        let imported_ids: Vec<u64> = server
            .import_file_tool(Parameters(ImportFileRequest {
                path: obj_path.to_string_lossy().to_string(),
                format_hint: None,
            }))
            .await
            .expect("import_file tool should succeed")
            .into_typed()
            .expect("import_file result should deserialize");
        assert_eq!(imported_ids.len(), 1);

        let imported_snapshot: Value = server
            .get_entity_tool(Parameters(GetEntityRequest {
                element_id: imported_ids[0],
            }))
            .await
            .expect("get_entity for imported triangle mesh should succeed")
            .into_typed()
            .expect("imported get_entity result should deserialize");
        assert_eq!(
            imported_snapshot["TriangleMesh"]["primitive"]["name"],
            json!("FromTool")
        );

        let updated_snapshot: Value = server
            .set_entity_property_tool(Parameters(SetPropertyRequest {
                element_id: 10,
                property_name: "half_extents".to_string(),
                value: json!([2.0, 2.0, 2.0]),
            }))
            .await
            .expect("set_entity_property tool should succeed")
            .into_typed()
            .expect("set_entity_property result should deserialize");
        assert_eq!(
            updated_snapshot["Box"]["primitive"]["half_extents"],
            json!([2.0, 2.0, 2.0])
        );

        let toolbars: Vec<ToolbarDetails> = server
            .list_toolbars_tool()
            .await
            .expect("list_toolbars tool should succeed")
            .into_typed()
            .expect("list_toolbars result should deserialize");
        assert_eq!(toolbars.len(), 2);

        let updated_toolbars: Vec<ToolbarDetails> = server
            .set_toolbar_layout_tool(Parameters(SetToolbarLayoutRequest {
                updates: vec![ToolbarLayoutUpdate {
                    toolbar_id: "modeling".to_string(),
                    dock: Some("right".to_string()),
                    order: Some(4),
                    visible: Some(true),
                }],
            }))
            .await
            .expect("set_toolbar_layout tool should succeed")
            .into_typed()
            .expect("set_toolbar_layout result should deserialize");
        let modeling_toolbar = updated_toolbars
            .iter()
            .find(|toolbar| toolbar.id == "modeling")
            .expect("modeling toolbar should be returned");
        assert_eq!(modeling_toolbar.dock, "right");
        assert_eq!(modeling_toolbar.order, 4);

        let _ = fs::remove_file(obj_path);

        drop(server);
        worker_handle.await.expect("worker should stop cleanly");
    }
}
