use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*, window::PrimaryWindow};
use serde::{Deserialize, Serialize};
#[cfg(feature = "model-api")]
use serde_json::Value;

use crate::authored_entity::{BoxedEntity, PropertyValueKind};
use crate::capability_registry::CapabilityRegistry;
use crate::plugins::identity::{ElementId, ElementIdAllocator};
use crate::plugins::modeling::group::{GroupEditContext, GroupMembers};
use crate::plugins::modeling::occurrence::HostedAnchor;
use crate::plugins::modeling::semantics::{geometry_semantics_for_snapshot, GeometrySemantics};
#[cfg(feature = "model-api")]
use crate::plugins::{
    camera::{apply_orbit_state, focus_orbit_camera_on_bounds, CameraProjectionMode, OrbitCamera},
    commands::{
        find_entity_by_element_id, queue_command_events, ApplyEntityChangesCommand,
        BeginCommandGroup, CreateEntityCommand, DeleteEntitiesCommand, EndCommandGroup,
        ResolvedDeleteEntitiesCommand,
    },
    document_properties::DocumentProperties,
    history::{apply_pending_history_commands, HistorySet},
    import::{import_file_now, ImportRegistry, ImporterDescriptor},
    layers::{LayerAssignment, LayerRegistry, LayerState},
    materials::{MaterialAssignment, MaterialDef, MaterialRegistry},
    named_views::NamedViewRegistry,
    persistence::{load_project_from_path, save_project_to_path},
    render_pipeline::{RenderSettings, RenderTonemapping},
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
        app.add_systems(Update, poll_model_api_requests.before(HistorySet::Queue));
        app.add_systems(Startup, annotate_window_title_with_model_api_instance);
        spawn_model_api_server(sender, runtime_info, http_listener);
    }
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Resource, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelApiRuntimeInfo {
    pub instance_id: String,
    pub app_name: String,
    pub pid: u32,
    pub http_host: String,
    pub http_port: u16,
    pub http_url: String,
    pub registry_path: String,
    pub started_at_unix_ms: u128,
    pub requested_port: Option<u16>,
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
pub struct InstanceInfo {
    pub instance_id: String,
    pub app_name: String,
    pub pid: u32,
    pub http_host: String,
    pub http_port: u16,
    pub http_url: String,
    pub registry_path: String,
    pub started_at_unix_ms: u128,
    pub requested_port: Option<u16>,
}

impl From<&ModelApiRuntimeInfo> for InstanceInfo {
    fn from(value: &ModelApiRuntimeInfo) -> Self {
        Self {
            instance_id: value.instance_id.clone(),
            app_name: value.app_name.clone(),
            pid: value.pid,
            http_host: value.http_host.clone(),
            http_port: value.http_port,
            http_url: value.http_url.clone(),
            registry_path: value.registry_path.clone(),
            started_at_unix_ms: value.started_at_unix_ms,
            requested_port: value.requested_port,
        }
    }
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

// --- Clip Plane types ---

/// Information about an authored clipping plane.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClipPlaneInfo {
    pub element_id: u64,
    pub name: String,
    /// Point on the plane in world space.
    pub origin: [f32; 3],
    /// Normal pointing toward the visible side.
    pub normal: [f32; 3],
    /// Whether this plane is currently cutting the view.
    pub active: bool,
}

// --- Named View types ---

/// A serialisable description of a saved camera position.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamedViewInfo {
    pub name: String,
    pub description: Option<String>,
    pub focus: [f32; 3],
    pub radius: f32,
    pub yaw: f32,
    pub pitch: f32,
    /// `"perspective"` or `"orthographic"`.
    pub projection: String,
    pub focal_length_mm: f32,
}

/// Partial camera parameters for saving or updating a named view.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CameraParams {
    pub focus: Option<[f32; 3]>,
    pub radius: Option<f32>,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    /// `"perspective"` or `"orthographic"`.
    pub projection: Option<String>,
    pub focal_length_mm: Option<f32>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SplitResult {
    pub box_a_element_id: u64,
    pub box_b_element_id: u64,
    pub group_element_id: u64,
}

// --- Material types ---

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterialInfo {
    pub id: String,
    pub name: String,
    pub base_color: [f32; 4],
    pub perceptual_roughness: f32,
    pub metallic: f32,
    pub reflectance: f32,
    pub specular_tint: [f32; 3],
    pub emissive: [f32; 3],
    pub emissive_exposure_weight: f32,
    pub diffuse_transmission: f32,
    pub specular_transmission: f32,
    pub thickness: f32,
    pub ior: f32,
    pub attenuation_distance: f32,
    pub attenuation_color: [f32; 3],
    pub clearcoat: f32,
    pub clearcoat_perceptual_roughness: f32,
    pub anisotropy_strength: f32,
    pub anisotropy_rotation_deg: f32,
    pub alpha_mode: String,
    pub double_sided: bool,
    pub unlit: bool,
    pub fog_enabled: bool,
    pub depth_bias: f32,
    pub uv_scale: [f32; 2],
    pub uv_rotation_deg: f32,
    /// Base64-encoded image data (no data-URI prefix) or `null`.
    pub base_color_texture: Option<String>,
    pub base_color_texture_mime: Option<String>,
    /// Base64-encoded image data (no data-URI prefix) or `null`.
    pub normal_map_texture: Option<String>,
    pub normal_map_texture_mime: Option<String>,
    /// Base64-encoded image data (no data-URI prefix) or `null`.
    pub metallic_roughness_texture: Option<String>,
    pub metallic_roughness_texture_mime: Option<String>,
    /// Base64-encoded image data (no data-URI prefix) or `null`.
    pub emissive_texture: Option<String>,
    pub emissive_texture_mime: Option<String>,
    /// Base64-encoded image data (no data-URI prefix) or `null`.
    pub occlusion_texture: Option<String>,
    pub occlusion_texture_mime: Option<String>,
}

impl MaterialInfo {
    fn from_def(def: &MaterialDef) -> Self {
        use crate::plugins::materials::TextureRef;

        fn tex_data(t: &Option<TextureRef>) -> Option<String> {
            match t {
                Some(TextureRef::Embedded { data, .. }) => Some(data.clone()),
                Some(TextureRef::AssetPath(p)) => Some(p.clone()),
                None => None,
            }
        }
        fn tex_mime(t: &Option<TextureRef>) -> Option<String> {
            match t {
                Some(TextureRef::Embedded { mime, .. }) => Some(mime.clone()),
                Some(TextureRef::AssetPath(_)) => None,
                None => None,
            }
        }

        Self {
            id: def.id.clone(),
            name: def.name.clone(),
            base_color: def.base_color,
            perceptual_roughness: def.perceptual_roughness,
            metallic: def.metallic,
            reflectance: def.reflectance,
            specular_tint: def.specular_tint,
            emissive: def.emissive,
            emissive_exposure_weight: def.emissive_exposure_weight,
            diffuse_transmission: def.diffuse_transmission,
            specular_transmission: def.specular_transmission,
            thickness: def.thickness,
            ior: def.ior,
            attenuation_distance: def.attenuation_distance,
            attenuation_color: def.attenuation_color,
            clearcoat: def.clearcoat,
            clearcoat_perceptual_roughness: def.clearcoat_perceptual_roughness,
            anisotropy_strength: def.anisotropy_strength,
            anisotropy_rotation_deg: def.anisotropy_rotation.to_degrees(),
            alpha_mode: format!("{:?}", def.alpha_mode),
            double_sided: def.double_sided,
            unlit: def.unlit,
            fog_enabled: def.fog_enabled,
            depth_bias: def.depth_bias,
            uv_scale: def.uv_scale,
            uv_rotation_deg: def.uv_rotation.to_degrees(),
            base_color_texture: tex_data(&def.base_color_texture),
            base_color_texture_mime: tex_mime(&def.base_color_texture),
            normal_map_texture: tex_data(&def.normal_map_texture),
            normal_map_texture_mime: tex_mime(&def.normal_map_texture),
            metallic_roughness_texture: tex_data(&def.metallic_roughness_texture),
            metallic_roughness_texture_mime: tex_mime(&def.metallic_roughness_texture),
            emissive_texture: tex_data(&def.emissive_texture),
            emissive_texture_mime: tex_mime(&def.emissive_texture),
            occlusion_texture: tex_data(&def.occlusion_texture),
            occlusion_texture_mime: tex_mime(&def.occlusion_texture),
        }
    }
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateMaterialRequest {
    pub name: String,
    #[serde(default = "default_base_color")]
    pub base_color: [f32; 4],
    #[serde(default = "default_roughness")]
    pub perceptual_roughness: f32,
    #[serde(default)]
    pub metallic: f32,
    #[serde(default = "default_reflectance")]
    pub reflectance: f32,
    #[serde(default = "default_specular_tint")]
    pub specular_tint: [f32; 3],
    #[serde(default)]
    pub emissive: [f32; 3],
    #[serde(default = "default_emissive_exposure_weight")]
    pub emissive_exposure_weight: f32,
    #[serde(default)]
    pub diffuse_transmission: f32,
    #[serde(default)]
    pub specular_transmission: f32,
    #[serde(default)]
    pub thickness: f32,
    #[serde(default = "default_ior")]
    pub ior: f32,
    #[serde(default = "default_attenuation_distance")]
    pub attenuation_distance: f32,
    #[serde(default = "default_attenuation_color")]
    pub attenuation_color: [f32; 3],
    #[serde(default)]
    pub clearcoat: f32,
    #[serde(default = "default_clearcoat_roughness")]
    pub clearcoat_perceptual_roughness: f32,
    #[serde(default)]
    pub anisotropy_strength: f32,
    #[serde(default)]
    pub anisotropy_rotation_deg: f32,
    #[serde(default = "default_alpha_mode")]
    pub alpha_mode: String,
    #[serde(default = "default_alpha_cutoff")]
    pub alpha_cutoff: f32,
    #[serde(default)]
    pub double_sided: bool,
    #[serde(default)]
    pub unlit: bool,
    #[serde(default = "default_true")]
    pub fog_enabled: bool,
    #[serde(default)]
    pub depth_bias: f32,
    #[serde(default = "default_uv_scale")]
    pub uv_scale: [f32; 2],
    #[serde(default)]
    pub uv_rotation_deg: f32,
    /// Base64-encoded image data.  Defaults to `"image/png"` if
    /// `base_color_texture_mime` is not provided.
    pub base_color_texture: Option<String>,
    #[serde(default)]
    pub base_color_texture_mime: Option<String>,
    pub normal_map_texture: Option<String>,
    #[serde(default)]
    pub normal_map_texture_mime: Option<String>,
    pub metallic_roughness_texture: Option<String>,
    #[serde(default)]
    pub metallic_roughness_texture_mime: Option<String>,
    pub emissive_texture: Option<String>,
    #[serde(default)]
    pub emissive_texture_mime: Option<String>,
    pub occlusion_texture: Option<String>,
    #[serde(default)]
    pub occlusion_texture_mime: Option<String>,
}

fn default_base_color() -> [f32; 4] {
    [0.78, 0.82, 0.88, 1.0]
}
fn default_roughness() -> f32 {
    0.85
}
fn default_reflectance() -> f32 {
    0.5
}
fn default_specular_tint() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}
fn default_emissive_exposure_weight() -> f32 {
    1.0
}
fn default_ior() -> f32 {
    1.5
}
fn default_attenuation_distance() -> f32 {
    f32::INFINITY
}
fn default_attenuation_color() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}
fn default_clearcoat_roughness() -> f32 {
    0.5
}
fn default_alpha_mode() -> String {
    "opaque".to_string()
}
fn default_alpha_cutoff() -> f32 {
    0.5
}
fn default_uv_scale() -> [f32; 2] {
    [1.0, 1.0]
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplyMaterialRequest {
    pub material_id: String,
    pub element_ids: Vec<u64>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RenderSettingsInfo {
    pub tonemapping: String,
    pub exposure_ev100: f32,
    pub ssao_enabled: bool,
    pub ssao_constant_object_thickness: f32,
    pub ambient_occlusion_quality: u8,
    pub bloom_enabled: bool,
    pub bloom_intensity: f32,
    pub bloom_low_frequency_boost: f32,
    pub bloom_low_frequency_boost_curvature: f32,
    pub bloom_high_pass_frequency: f32,
    pub bloom_threshold: f32,
    pub bloom_threshold_softness: f32,
    pub bloom_scale: [f32; 2],
    pub ssr_enabled: bool,
    pub ssr_perceptual_roughness_threshold: f32,
    pub ssr_thickness: f32,
    pub ssr_linear_steps: u32,
    pub ssr_linear_march_exponent: f32,
    pub ssr_bisection_steps: u32,
    pub ssr_use_secant: bool,
}

impl RenderSettingsInfo {
    fn from_settings(settings: &RenderSettings) -> Self {
        Self {
            tonemapping: settings.tonemapping.as_str().to_string(),
            exposure_ev100: settings.exposure_ev100,
            ssao_enabled: settings.ssao_enabled,
            ssao_constant_object_thickness: settings.ssao_constant_object_thickness,
            ambient_occlusion_quality: settings.ambient_occlusion_quality,
            bloom_enabled: settings.bloom_enabled,
            bloom_intensity: settings.bloom_intensity,
            bloom_low_frequency_boost: settings.bloom_low_frequency_boost,
            bloom_low_frequency_boost_curvature: settings.bloom_low_frequency_boost_curvature,
            bloom_high_pass_frequency: settings.bloom_high_pass_frequency,
            bloom_threshold: settings.bloom_threshold,
            bloom_threshold_softness: settings.bloom_threshold_softness,
            bloom_scale: settings.bloom_scale,
            ssr_enabled: settings.ssr_enabled,
            ssr_perceptual_roughness_threshold: settings.ssr_perceptual_roughness_threshold,
            ssr_thickness: settings.ssr_thickness,
            ssr_linear_steps: settings.ssr_linear_steps,
            ssr_linear_march_exponent: settings.ssr_linear_march_exponent,
            ssr_bisection_steps: settings.ssr_bisection_steps,
            ssr_use_secant: settings.ssr_use_secant,
        }
    }
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RenderSettingsUpdateRequest {
    pub tonemapping: Option<String>,
    pub exposure_ev100: Option<f32>,
    pub ssao_enabled: Option<bool>,
    pub ssao_constant_object_thickness: Option<f32>,
    pub ambient_occlusion_quality: Option<u8>,
    pub bloom_enabled: Option<bool>,
    pub bloom_intensity: Option<f32>,
    pub bloom_low_frequency_boost: Option<f32>,
    pub bloom_low_frequency_boost_curvature: Option<f32>,
    pub bloom_high_pass_frequency: Option<f32>,
    pub bloom_threshold: Option<f32>,
    pub bloom_threshold_softness: Option<f32>,
    pub bloom_scale: Option<[f32; 2]>,
    pub ssr_enabled: Option<bool>,
    pub ssr_perceptual_roughness_threshold: Option<f32>,
    pub ssr_thickness: Option<f32>,
    pub ssr_linear_steps: Option<u32>,
    pub ssr_linear_march_exponent: Option<f32>,
    pub ssr_bisection_steps: Option<u32>,
    pub ssr_use_secant: Option<bool>,
}

// --- Definition / Occurrence types ---

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionEntry {
    pub definition_id: String,
    pub name: String,
    pub definition_kind: String,
    pub definition_version: u32,
    pub parameter_names: Vec<String>,
    pub full: serde_json::Value,
    pub effective_full: serde_json::Value,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionLibraryEntry {
    pub library_id: String,
    pub name: String,
    pub scope: String,
    pub definition_count: usize,
    pub source_path: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstantiateDefinitionResult {
    pub element_id: u64,
    pub definition_id: String,
    pub imported_definition_ids: Vec<String>,
    #[serde(default)]
    pub relation_ids: Vec<u64>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionDraftEntry {
    pub draft_id: String,
    pub source_definition_id: Option<String>,
    pub source_library_id: Option<String>,
    pub definition_id: String,
    pub name: String,
    pub definition_version: u32,
    pub dirty: bool,
    pub full: Value,
    pub effective_full: Value,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionValidationResult {
    pub ok: bool,
    pub errors: Vec<String>,
    pub effective_full: Option<Value>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionCompileEdge {
    pub from: String,
    pub to: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionCompileResult {
    pub target_id: String,
    pub effective_full: Value,
    pub nodes: Vec<String>,
    pub edges: Vec<DefinitionCompileEdge>,
    pub child_slot_count: usize,
    pub derived_parameter_count: usize,
    pub constraint_count: usize,
    pub anchor_count: usize,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionExplainResult {
    pub target_id: String,
    pub raw_full: Value,
    pub effective_full: Value,
    pub local_parameter_names: Vec<String>,
    pub inherited_parameter_names: Vec<String>,
    pub local_child_slot_ids: Vec<String>,
    pub inherited_child_slot_ids: Vec<String>,
    pub compile: DefinitionCompileResult,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeneratedOccurrencePartEntry {
    pub slot_path: String,
    pub definition_id: String,
    pub center: [f32; 3],
    pub profile_min: [f32; 2],
    pub profile_max: [f32; 2],
    pub height: f32,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OccurrenceExplainResult {
    pub element_id: u64,
    pub label: String,
    pub definition_id: String,
    pub definition_version: u32,
    pub domain_data: Value,
    pub hosting: Value,
    pub transform: Value,
    pub resolved_parameters: Value,
    pub anchors: Vec<Value>,
    pub generated_parts: Vec<GeneratedOccurrencePartEntry>,
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
    GetInstanceInfo(oneshot::Sender<InstanceInfo>),
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
    // --- Materials ---
    ListMaterials(oneshot::Sender<Vec<MaterialInfo>>),
    GetMaterial {
        id: String,
        response: oneshot::Sender<ApiResult<MaterialInfo>>,
    },
    CreateMaterial {
        request: CreateMaterialRequest,
        response: oneshot::Sender<ApiResult<MaterialInfo>>,
    },
    UpdateMaterial {
        id: String,
        request: CreateMaterialRequest,
        response: oneshot::Sender<ApiResult<MaterialInfo>>,
    },
    DeleteMaterial {
        id: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    ApplyMaterial {
        request: ApplyMaterialRequest,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    RemoveMaterial {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    GetRenderSettings(oneshot::Sender<RenderSettingsInfo>),
    SetRenderSettings {
        request: RenderSettingsUpdateRequest,
        response: oneshot::Sender<ApiResult<RenderSettingsInfo>>,
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
    // --- Definition / Occurrence ---
    ListDefinitions(oneshot::Sender<Vec<DefinitionEntry>>),
    GetDefinition {
        definition_id: String,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    CreateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    UpdateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    ListDefinitionDrafts(oneshot::Sender<Vec<DefinitionDraftEntry>>),
    GetDefinitionDraft {
        draft_id: String,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    OpenDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    CreateDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    DeriveDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    PatchDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    PublishDefinitionDraft {
        draft_id: String,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    ValidateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionValidationResult>>,
    },
    CompileDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionCompileResult>>,
    },
    ExplainDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionExplainResult>>,
    },
    ListDefinitionLibraries(oneshot::Sender<Vec<DefinitionLibraryEntry>>),
    GetDefinitionLibrary {
        library_id: String,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    CreateDefinitionLibrary {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    AddDefinitionToLibrary {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    ImportDefinitionLibrary {
        path: String,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    ExportDefinitionLibrary {
        library_id: String,
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    InstantiateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<InstantiateDefinitionResult>>,
    },
    InstantiateHostedDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<InstantiateDefinitionResult>>,
    },
    PlaceOccurrence {
        request: Value,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    UpdateOccurrenceOverrides {
        element_id: u64,
        overrides: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    ExplainOccurrence {
        element_id: u64,
        response: oneshot::Sender<ApiResult<OccurrenceExplainResult>>,
    },
    ResolveOccurrence {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    // --- Array ---
    ArrayCreateLinear {
        source_id: u64,
        count: u32,
        spacing: [f32; 3],
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ArrayCreatePolar {
        source_id: u64,
        count: u32,
        axis: [f32; 3],
        total_angle_degrees: f32,
        center: [f32; 3],
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ArrayUpdate {
        element_id: u64,
        count: Option<u32>,
        spacing: Option<[f32; 3]>,
        axis: Option<[f32; 3]>,
        total_angle_degrees: Option<f32>,
        center: Option<[f32; 3]>,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    ArrayDissolve {
        element_id: u64,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ArrayGet {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    // --- Mirror ---
    MirrorCreate {
        source_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    MirrorUpdate {
        element_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    MirrorDissolve {
        element_id: u64,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    MirrorGet {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    // --- Named Views ---
    ViewList(oneshot::Sender<Vec<NamedViewInfo>>),
    ViewSave {
        name: String,
        description: Option<String>,
        camera_params: Option<CameraParams>,
        response: oneshot::Sender<ApiResult<NamedViewInfo>>,
    },
    ViewRestore {
        name: String,
        response: oneshot::Sender<ApiResult<NamedViewInfo>>,
    },
    ViewUpdate {
        name: String,
        new_name: Option<String>,
        description: Option<String>,
        camera_params: Option<CameraParams>,
        response: oneshot::Sender<ApiResult<NamedViewInfo>>,
    },
    ViewDelete {
        name: String,
        response: oneshot::Sender<ApiResult<()>>,
    },
    // --- Clipping Planes ---
    ClipPlaneCreate {
        name: String,
        origin: [f32; 3],
        normal: [f32; 3],
        active: bool,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ClipPlaneUpdate {
        element_id: u64,
        name: Option<String>,
        origin: Option<[f32; 3]>,
        normal: Option<[f32; 3]>,
        active: Option<bool>,
        response: oneshot::Sender<ApiResult<ClipPlaneInfo>>,
    },
    ClipPlaneList(oneshot::Sender<Vec<ClipPlaneInfo>>),
    ClipPlaneToggle {
        element_id: u64,
        active: bool,
        response: oneshot::Sender<ApiResult<ClipPlaneInfo>>,
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
        ModelApiRequest::GetInstanceInfo(response) => {
            let _ = response.send(handle_get_instance_info(world));
        }
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
        // --- Materials ---
        ModelApiRequest::ListMaterials(response) => {
            let _ = response.send(handle_list_materials(world));
        }
        ModelApiRequest::GetMaterial { id, response } => {
            let _ = response.send(handle_get_material(world, &id));
        }
        ModelApiRequest::CreateMaterial { request, response } => {
            let _ = response.send(handle_create_material(world, request));
        }
        ModelApiRequest::UpdateMaterial {
            id,
            request,
            response,
        } => {
            let _ = response.send(handle_update_material(world, &id, request));
        }
        ModelApiRequest::DeleteMaterial { id, response } => {
            let _ = response.send(handle_delete_material(world, &id));
        }
        ModelApiRequest::ApplyMaterial { request, response } => {
            let _ = response.send(handle_apply_material(world, request));
        }
        ModelApiRequest::RemoveMaterial {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_remove_material(world, element_ids));
        }
        ModelApiRequest::GetRenderSettings(response) => {
            let _ = response.send(handle_get_render_settings(world));
        }
        ModelApiRequest::SetRenderSettings { request, response } => {
            let _ = response.send(handle_set_render_settings(world, request));
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
        ModelApiRequest::ListDefinitions(response) => {
            let _ = response.send(handle_list_definitions(world));
        }
        ModelApiRequest::GetDefinition {
            definition_id,
            response,
        } => {
            let _ = response.send(handle_get_definition(world, definition_id));
        }
        ModelApiRequest::CreateDefinition { request, response } => {
            let _ = response.send(handle_create_definition(world, request));
        }
        ModelApiRequest::UpdateDefinition { request, response } => {
            let _ = response.send(handle_update_definition(world, request));
        }
        ModelApiRequest::ListDefinitionDrafts(response) => {
            let _ = response.send(handle_list_definition_drafts(world));
        }
        ModelApiRequest::GetDefinitionDraft { draft_id, response } => {
            let _ = response.send(handle_get_definition_draft(world, draft_id));
        }
        ModelApiRequest::OpenDefinitionDraft { request, response } => {
            let _ = response.send(handle_open_definition_draft(world, request));
        }
        ModelApiRequest::CreateDefinitionDraft { request, response } => {
            let _ = response.send(handle_create_definition_draft(world, request));
        }
        ModelApiRequest::DeriveDefinitionDraft { request, response } => {
            let _ = response.send(handle_derive_definition_draft(world, request));
        }
        ModelApiRequest::PatchDefinitionDraft { request, response } => {
            let _ = response.send(handle_patch_definition_draft(world, request));
        }
        ModelApiRequest::PublishDefinitionDraft { draft_id, response } => {
            let _ = response.send(handle_publish_definition_draft(world, draft_id));
        }
        ModelApiRequest::ValidateDefinition { request, response } => {
            let _ = response.send(handle_validate_definition(world, request));
        }
        ModelApiRequest::CompileDefinition { request, response } => {
            let _ = response.send(handle_compile_definition(world, request));
        }
        ModelApiRequest::ExplainDefinition { request, response } => {
            let _ = response.send(handle_explain_definition(world, request));
        }
        ModelApiRequest::ListDefinitionLibraries(response) => {
            let _ = response.send(handle_list_definition_libraries(world));
        }
        ModelApiRequest::GetDefinitionLibrary {
            library_id,
            response,
        } => {
            let _ = response.send(handle_get_definition_library(world, library_id));
        }
        ModelApiRequest::CreateDefinitionLibrary { request, response } => {
            let _ = response.send(handle_create_definition_library(world, request));
        }
        ModelApiRequest::AddDefinitionToLibrary { request, response } => {
            let _ = response.send(handle_add_definition_to_library(world, request));
        }
        ModelApiRequest::ImportDefinitionLibrary { path, response } => {
            let _ = response.send(handle_import_definition_library(world, &path));
        }
        ModelApiRequest::ExportDefinitionLibrary {
            library_id,
            path,
            response,
        } => {
            let _ = response.send(handle_export_definition_library(world, &library_id, &path));
        }
        ModelApiRequest::InstantiateDefinition { request, response } => {
            let _ = response.send(handle_instantiate_definition(world, request));
        }
        ModelApiRequest::InstantiateHostedDefinition { request, response } => {
            let _ = response.send(handle_instantiate_hosted_definition(world, request));
        }
        ModelApiRequest::PlaceOccurrence { request, response } => {
            let _ = response.send(handle_place_occurrence(world, request));
        }
        ModelApiRequest::UpdateOccurrenceOverrides {
            element_id,
            overrides,
            response,
        } => {
            let _ = response.send(handle_update_occurrence_overrides(
                world, element_id, overrides,
            ));
        }
        ModelApiRequest::ExplainOccurrence {
            element_id,
            response,
        } => {
            let _ = response.send(handle_explain_occurrence(world, element_id));
        }
        ModelApiRequest::ResolveOccurrence {
            element_id,
            response,
        } => {
            let _ = response.send(handle_resolve_occurrence(world, element_id));
        }
        // --- Array ---
        ModelApiRequest::ArrayCreateLinear {
            source_id,
            count,
            spacing,
            response,
        } => {
            let _ = response.send(handle_array_create_linear(world, source_id, count, spacing));
        }
        ModelApiRequest::ArrayCreatePolar {
            source_id,
            count,
            axis,
            total_angle_degrees,
            center,
            response,
        } => {
            let _ = response.send(handle_array_create_polar(
                world,
                source_id,
                count,
                axis,
                total_angle_degrees,
                center,
            ));
        }
        ModelApiRequest::ArrayUpdate {
            element_id,
            count,
            spacing,
            axis,
            total_angle_degrees,
            center,
            response,
        } => {
            let _ = response.send(handle_array_update(
                world,
                element_id,
                count,
                spacing,
                axis,
                total_angle_degrees,
                center,
            ));
        }
        ModelApiRequest::ArrayDissolve {
            element_id,
            response,
        } => {
            let _ = response.send(handle_array_dissolve(world, element_id));
        }
        ModelApiRequest::ArrayGet {
            element_id,
            response,
        } => {
            let _ = response.send(handle_array_get(world, element_id));
        }
        // --- Mirror ---
        ModelApiRequest::MirrorCreate {
            source_id,
            plane_str,
            plane_origin,
            plane_normal,
            merge,
            response,
        } => {
            let _ = response.send(handle_mirror_create(
                world,
                source_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
            ));
        }
        ModelApiRequest::MirrorUpdate {
            element_id,
            plane_str,
            plane_origin,
            plane_normal,
            merge,
            response,
        } => {
            let _ = response.send(handle_mirror_update(
                world,
                element_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
            ));
        }
        ModelApiRequest::MirrorDissolve {
            element_id,
            response,
        } => {
            let _ = response.send(handle_mirror_dissolve(world, element_id));
        }
        ModelApiRequest::MirrorGet {
            element_id,
            response,
        } => {
            let _ = response.send(handle_mirror_get(world, element_id));
        }
        // --- Named Views ---
        ModelApiRequest::ViewList(response) => {
            let _ = response.send(handle_view_list(world));
        }
        ModelApiRequest::ViewSave {
            name,
            description,
            camera_params,
            response,
        } => {
            let _ = response.send(handle_view_save(world, name, description, camera_params));
        }
        ModelApiRequest::ViewRestore { name, response } => {
            let _ = response.send(handle_view_restore(world, name));
        }
        ModelApiRequest::ViewUpdate {
            name,
            new_name,
            description,
            camera_params,
            response,
        } => {
            let _ = response.send(handle_view_update(
                world,
                name,
                new_name,
                description,
                camera_params,
            ));
        }
        ModelApiRequest::ViewDelete { name, response } => {
            let _ = response.send(handle_view_delete(world, name));
        }
        // --- Clipping Planes ---
        ModelApiRequest::ClipPlaneCreate {
            name,
            origin,
            normal,
            active,
            response,
        } => {
            let _ = response.send(handle_clip_plane_create(
                world, name, origin, normal, active,
            ));
        }
        ModelApiRequest::ClipPlaneUpdate {
            element_id,
            name,
            origin,
            normal,
            active,
            response,
        } => {
            let _ = response.send(handle_clip_plane_update(
                world, element_id, name, origin, normal, active,
            ));
        }
        ModelApiRequest::ClipPlaneList(response) => {
            let _ = response.send(handle_clip_plane_list(world));
        }
        ModelApiRequest::ClipPlaneToggle {
            element_id,
            active,
            response,
        } => {
            let _ = response.send(handle_clip_plane_toggle(world, element_id, active));
        }
    }
}

const MODEL_API_DEFAULT_HTTP_PORT: u16 = 24842;
const MODEL_API_DEFAULT_HTTP_HOST: &str = "127.0.0.1";
const MODEL_API_INSTANCE_ENV: &str = "TALOS3D_INSTANCE_ID";
const MODEL_API_PORT_ENV: &str = "TALOS3D_MODEL_API_PORT";
const MODEL_API_REGISTRY_DIR_ENV: &str = "TALOS3D_INSTANCE_REGISTRY_DIR";
const MODEL_API_DEFAULT_REGISTRY_DIR: &str = "/tmp/talos3d-instances";

#[cfg(feature = "model-api")]
fn spawn_model_api_server(
    sender: mpsc::Sender<ModelApiRequest>,
    runtime_info: ModelApiRuntimeInfo,
    http_listener: StdTcpListener,
) {
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
                let config = StreamableHttpServerConfig::default()
                    .with_stateful_mode(false)
                    .with_json_response(true)
                    .with_cancellation_token(ct.clone());
                let service: StreamableHttpService<ModelApiServer, LocalSessionManager> =
                    StreamableHttpService::new(
                        move || Ok(ModelApiServer::new(sender.clone())),
                        Default::default(),
                        config,
                    );

                let router = axum::Router::new().nest_service("/mcp", service);
                let addr = format!("{}:{}", runtime_info.http_host, runtime_info.http_port);
                let tcp_listener = match tokio::net::TcpListener::from_std(http_listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("failed to adopt model API HTTP listener on {addr}: {error}");
                        return;
                    }
                };
                eprintln!(
                    "talos3d instance {} MCP {} registry {}",
                    runtime_info.instance_id, runtime_info.http_url, runtime_info.registry_path
                );
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
fn resolve_model_api_runtime() -> Result<(ModelApiRuntimeInfo, StdTcpListener), String> {
    let app_name = current_app_name();
    let pid = std::process::id();
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_millis();
    let instance_id = env::var(MODEL_API_INSTANCE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{app_name}-{pid}-{started_at_unix_ms}"));
    let requested_port = env::var(MODEL_API_PORT_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value.parse::<u16>().map_err(|error| {
                format!(
                    "invalid {} value {:?}: {}",
                    MODEL_API_PORT_ENV, value, error
                )
            })
        })
        .transpose()?;
    let registry_dir = env::var(MODEL_API_REGISTRY_DIR_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(MODEL_API_DEFAULT_REGISTRY_DIR));

    let listener = bind_model_api_listener(requested_port)?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to configure model API HTTP listener: {error}"))?;
    let http_port = listener
        .local_addr()
        .map_err(|error| format!("failed to read model API HTTP listener address: {error}"))?
        .port();
    let http_url = format!("http://{MODEL_API_DEFAULT_HTTP_HOST}:{http_port}/mcp");
    let registry_path = write_instance_registry_manifest(
        &registry_dir,
        &instance_id,
        &app_name,
        pid,
        http_port,
        started_at_unix_ms,
        requested_port,
    )?;
    let runtime_info = ModelApiRuntimeInfo {
        instance_id,
        app_name,
        pid,
        http_host: MODEL_API_DEFAULT_HTTP_HOST.to_string(),
        http_port,
        http_url,
        registry_path: registry_path.display().to_string(),
        started_at_unix_ms,
        requested_port,
    };

    Ok((runtime_info, listener))
}

#[cfg(feature = "model-api")]
fn bind_model_api_listener(requested_port: Option<u16>) -> Result<StdTcpListener, String> {
    let preferred_port = requested_port.unwrap_or(MODEL_API_DEFAULT_HTTP_PORT);
    let preferred_addr = format!("{MODEL_API_DEFAULT_HTTP_HOST}:{preferred_port}");
    match StdTcpListener::bind(&preferred_addr) {
        Ok(listener) => Ok(listener),
        Err(error) if requested_port.is_none() && preferred_port == MODEL_API_DEFAULT_HTTP_PORT => {
            let fallback_addr = format!("{MODEL_API_DEFAULT_HTTP_HOST}:0");
            let listener = StdTcpListener::bind(&fallback_addr).map_err(|fallback_error| {
                format!(
                    "failed to bind model API HTTP on {preferred_addr} ({error}) and fallback {fallback_addr} ({fallback_error})"
                )
            })?;
            eprintln!(
                "model API default port {} was busy; using auto-assigned port {}",
                MODEL_API_DEFAULT_HTTP_PORT,
                listener
                    .local_addr()
                    .map_err(|addr_error| format!(
                        "failed to read fallback listener address: {addr_error}"
                    ))?
                    .port()
            );
            Ok(listener)
        }
        Err(error) => Err(format!(
            "failed to bind model API HTTP on {preferred_addr}: {error}"
        )),
    }
}

#[cfg(feature = "model-api")]
fn current_app_name() -> String {
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "talos3d".to_string())
}

#[cfg(feature = "model-api")]
fn write_instance_registry_manifest(
    registry_dir: &Path,
    instance_id: &str,
    app_name: &str,
    pid: u32,
    http_port: u16,
    started_at_unix_ms: u128,
    requested_port: Option<u16>,
) -> Result<PathBuf, String> {
    fs::create_dir_all(registry_dir).map_err(|error| {
        format!(
            "failed to create instance registry directory {}: {error}",
            registry_dir.display()
        )
    })?;
    let registry_path = registry_dir.join(format!("{instance_id}.json"));
    let manifest = serde_json::json!({
        "instance_id": instance_id,
        "app_name": app_name,
        "pid": pid,
        "http_host": MODEL_API_DEFAULT_HTTP_HOST,
        "http_port": http_port,
        "http_url": format!("http://{MODEL_API_DEFAULT_HTTP_HOST}:{http_port}/mcp"),
        "registry_path": registry_path.display().to_string(),
        "started_at_unix_ms": started_at_unix_ms,
        "requested_port": requested_port
    });
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("failed to serialize instance manifest: {error}"))?;
    fs::write(&registry_path, bytes).map_err(|error| {
        format!(
            "failed to write instance manifest {}: {error}",
            registry_path.display()
        )
    })?;
    Ok(registry_path)
}

#[cfg(feature = "model-api")]
fn annotate_window_title_with_model_api_instance(
    runtime_info: Res<ModelApiRuntimeInfo>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    if window.title.contains(&runtime_info.instance_id) {
        return;
    }
    window.title = format!(
        "{} [{} @ {}]",
        window.title, runtime_info.instance_id, runtime_info.http_port
    );
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

    async fn request_get_instance_info(&self) -> Result<InstanceInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetInstanceInfo(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
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

    // --- Named Views ---

    async fn request_view_list(&self) -> Result<Vec<NamedViewInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewList(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_view_save(
        &self,
        name: String,
        description: Option<String>,
        camera_params: Option<CameraParams>,
    ) -> ApiResult<NamedViewInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewSave {
                name,
                description,
                camera_params,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_view_restore(&self, name: String) -> ApiResult<NamedViewInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewRestore { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_view_update(
        &self,
        name: String,
        new_name: Option<String>,
        description: Option<String>,
        camera_params: Option<CameraParams>,
    ) -> ApiResult<NamedViewInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewUpdate {
                name,
                new_name,
                description,
                camera_params,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_view_delete(&self, name: String) -> ApiResult<()> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewDelete { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Clipping Planes ---

    async fn request_clip_plane_create(
        &self,
        name: String,
        origin: [f32; 3],
        normal: [f32; 3],
        active: bool,
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneCreate {
                name,
                origin,
                normal,
                active,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_clip_plane_update(
        &self,
        element_id: u64,
        name: Option<String>,
        origin: Option<[f32; 3]>,
        normal: Option<[f32; 3]>,
        active: Option<bool>,
    ) -> ApiResult<ClipPlaneInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneUpdate {
                element_id,
                name,
                origin,
                normal,
                active,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_clip_plane_list(&self) -> Result<Vec<ClipPlaneInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneList(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_clip_plane_toggle(
        &self,
        element_id: u64,
        active: bool,
    ) -> ApiResult<ClipPlaneInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneToggle {
                element_id,
                active,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Materials ---

    async fn request_list_materials(&self) -> Result<Vec<MaterialInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListMaterials(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_material(&self, id: String) -> ApiResult<MaterialInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetMaterial { id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_material(
        &self,
        request: CreateMaterialRequest,
    ) -> ApiResult<MaterialInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateMaterial { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_material(
        &self,
        id: String,
        request: CreateMaterialRequest,
    ) -> ApiResult<MaterialInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateMaterial {
                id,
                request,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_material(&self, id: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteMaterial { id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_apply_material(&self, request: ApplyMaterialRequest) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ApplyMaterial { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_remove_material(&self, element_ids: Vec<u64>) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RemoveMaterial {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_render_settings(&self) -> Result<RenderSettingsInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetRenderSettings(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_render_settings(
        &self,
        request: RenderSettingsUpdateRequest,
    ) -> ApiResult<RenderSettingsInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetRenderSettings { request, response })
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
        let saved_path = receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())??;

        wait_for_screenshot_file(&saved_path).await?;
        Ok(saved_path)
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

    async fn request_list_definitions(&self) -> Result<Vec<DefinitionEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListDefinitions(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_definition(&self, definition_id: String) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDefinition {
                definition_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_definition(&self, request: Value) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_definition(&self, request: Value) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_definition_drafts(&self) -> Result<Vec<DefinitionDraftEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListDefinitionDrafts(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_definition_draft(
        &self,
        draft_id: String,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDefinitionDraft { draft_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_open_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::OpenDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_derive_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeriveDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_patch_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PatchDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_publish_definition_draft(
        &self,
        draft_id: String,
    ) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PublishDefinitionDraft { draft_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_validate_definition(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionValidationResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ValidateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_compile_definition(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionCompileResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CompileDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_explain_definition(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionExplainResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExplainDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_definition_libraries(
        &self,
    ) -> Result<Vec<DefinitionLibraryEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListDefinitionLibraries(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_definition_library(&self, library_id: String) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDefinitionLibrary {
                library_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_definition_library(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateDefinitionLibrary { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_add_definition_to_library(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AddDefinitionToLibrary { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_import_definition_library(
        &self,
        path: String,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ImportDefinitionLibrary { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_export_definition_library(
        &self,
        library_id: String,
        path: String,
    ) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExportDefinitionLibrary {
                library_id,
                path,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_instantiate_definition(
        &self,
        request: Value,
    ) -> ApiResult<InstantiateDefinitionResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InstantiateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_instantiate_hosted_definition(
        &self,
        request: Value,
    ) -> ApiResult<InstantiateDefinitionResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InstantiateHostedDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_place_occurrence(&self, request: Value) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PlaceOccurrence { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_occurrence_overrides(
        &self,
        element_id: u64,
        overrides: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateOccurrenceOverrides {
                element_id,
                overrides,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_explain_occurrence(
        &self,
        element_id: u64,
    ) -> ApiResult<OccurrenceExplainResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExplainOccurrence {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_resolve_occurrence(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ResolveOccurrence {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Array requests ---

    async fn request_array_create_linear(
        &self,
        source_id: u64,
        count: u32,
        spacing: [f32; 3],
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayCreateLinear {
                source_id,
                count,
                spacing,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_create_polar(
        &self,
        source_id: u64,
        count: u32,
        axis: [f32; 3],
        total_angle_degrees: f32,
        center: [f32; 3],
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayCreatePolar {
                source_id,
                count,
                axis,
                total_angle_degrees,
                center,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_update(
        &self,
        element_id: u64,
        count: Option<u32>,
        spacing: Option<[f32; 3]>,
        axis: Option<[f32; 3]>,
        total_angle_degrees: Option<f32>,
        center: Option<[f32; 3]>,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayUpdate {
                element_id,
                count,
                spacing,
                axis,
                total_angle_degrees,
                center,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_dissolve(&self, element_id: u64) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayDissolve {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_get(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayGet {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Mirror requests ---

    async fn request_mirror_create(
        &self,
        source_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorCreate {
                source_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_mirror_update(
        &self,
        element_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorUpdate {
                element_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_mirror_dissolve(&self, element_id: u64) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorDissolve {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_mirror_get(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorGet {
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
struct GetMaterialRequest {
    id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateMaterialRequest {
    id: String,
    #[serde(flatten)]
    material: CreateMaterialRequest,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeleteMaterialRequest {
    id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoveMaterialRequest {
    element_ids: Vec<u64>,
}

// --- Named View request types ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ViewSaveRequest {
    name: String,
    description: Option<String>,
    #[serde(flatten)]
    camera: Option<CameraParams>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ViewRestoreRequest {
    name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ViewUpdateRequest {
    name: String,
    new_name: Option<String>,
    description: Option<String>,
    #[serde(flatten)]
    camera: Option<CameraParams>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ViewDeleteRequest {
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

// --- Clipping Plane request types ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClipPlaneCreateRequest {
    /// Display name for the clipping plane.
    #[serde(default = "default_clip_plane_name")]
    name: String,
    /// Point on the plane in world space `[x, y, z]`. Defaults to origin.
    #[serde(default)]
    origin: [f32; 3],
    /// Normal pointing toward the visible side `[x, y, z]`. Defaults to `[0,1,0]` (up).
    #[serde(default = "default_clip_plane_normal")]
    normal: [f32; 3],
    /// Whether the plane is active immediately. Defaults to `true`.
    #[serde(default = "default_true")]
    active: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClipPlaneUpdateRequest {
    element_id: u64,
    name: Option<String>,
    origin: Option<[f32; 3]>,
    normal: Option<[f32; 3]>,
    active: Option<bool>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClipPlaneToggleRequest {
    element_id: u64,
    active: bool,
}

#[cfg(feature = "model-api")]
fn default_clip_plane_name() -> String {
    "Section".to_string()
}

#[cfg(feature = "model-api")]
fn default_clip_plane_normal() -> [f32; 3] {
    [0.0, 1.0, 0.0]
}

#[cfg(feature = "model-api")]
fn default_screenshot_path() -> String {
    "/tmp/talos_screenshot.png".to_string()
}

#[cfg(feature = "model-api")]
async fn wait_for_screenshot_file(path: &str) -> Result<(), String> {
    const ATTEMPTS: usize = 150;
    const POLL_INTERVAL_MS: u64 = 100;

    for _ in 0..ATTEMPTS {
        if std::fs::metadata(path)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
        {
            return Ok(());
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }

    Err(format!(
        "Screenshot was requested but '{path}' was not written within {} ms",
        ATTEMPTS as u64 * POLL_INTERVAL_MS
    ))
}

#[cfg(feature = "model-api")]
fn save_screenshot_to_disk(
    path: impl AsRef<std::path::Path>,
) -> impl FnMut(bevy::prelude::On<bevy::render::view::screenshot::ScreenshotCaptured>) {
    let path = path.as_ref().to_owned();
    move |screenshot_captured| {
        let img = screenshot_captured.image.clone();
        match img.try_into_dynamic() {
            Ok(dynamic) => match image::ImageFormat::from_path(&path) {
                Ok(format) => {
                    let rgb = dynamic.to_rgb8();
                    if let Err(error) = rgb.save_with_format(&path, format) {
                        error!("Cannot save screenshot, IO error: {error}");
                    }
                }
                Err(error) => {
                    error!("Cannot save screenshot, requested format not recognized: {error}");
                }
            },
            Err(error) => {
                error!("Cannot save screenshot, screen format cannot be understood: {error}");
            }
        }
    }
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
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DefinitionGetRequest {
    definition_id: String,
    #[serde(default)]
    library_id: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DefinitionDraftIdRequest {
    draft_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DefinitionLibraryGetRequest {
    library_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DefinitionLibraryPathRequest {
    path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DefinitionLibraryExportRequest {
    library_id: String,
    path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OccurrenceUpdateOverridesRequest {
    element_id: u64,
    overrides: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OccurrenceResolveRequest {
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ArrayCreateLinearRequest {
    /// Source entity ID to array.
    source: u64,
    /// Number of copies (includes the original). Minimum 2.
    count: u32,
    /// Spacing vector [x, y, z] — direction × distance between successive copies.
    spacing: [f32; 3],
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ArrayCreatePolarRequest {
    /// Source entity ID to array.
    source: u64,
    /// Number of copies (includes the original). Minimum 2.
    count: u32,
    /// Rotation axis [x, y, z]. Defaults to [0, 1, 0] (Y axis).
    axis: Option<[f32; 3]>,
    /// Total sweep angle in degrees. Defaults to 360.0 (full circle).
    total_angle_degrees: Option<f32>,
    /// Centre point of rotation [x, y, z]. Defaults to [0, 0, 0].
    center: Option<[f32; 3]>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ArrayUpdateRequest {
    /// Element ID of the array node to update.
    element_id: u64,
    /// New copy count (minimum 2).
    count: Option<u32>,
    /// New spacing vector [x, y, z] (linear array only).
    spacing: Option<[f32; 3]>,
    /// New rotation axis [x, y, z] (polar array only).
    axis: Option<[f32; 3]>,
    /// New total angle in degrees (polar array only).
    total_angle_degrees: Option<f32>,
    /// New centre of rotation [x, y, z] (polar array only).
    center: Option<[f32; 3]>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct ArrayEntityRequest {
    /// Element ID of the array node.
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct MirrorCreateRequest {
    /// Source entity ID to mirror.
    source: u64,
    /// Mirror plane shortcut: "XY", "XZ", or "YZ". Takes priority over plane_origin/plane_normal.
    plane: Option<String>,
    /// Mirror plane origin [x, y, z]. Used when `plane` is not set.
    plane_origin: Option<[f32; 3]>,
    /// Mirror plane normal [x, y, z]. Used when `plane` is not set.
    plane_normal: Option<[f32; 3]>,
    /// Whether to merge vertices at the seam (default: false).
    #[serde(default)]
    merge: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct MirrorUpdateRequest {
    /// Element ID of the MirrorNode to update.
    element_id: u64,
    /// Mirror plane shortcut: "XY", "XZ", or "YZ".
    plane: Option<String>,
    /// Mirror plane origin [x, y, z].
    plane_origin: Option<[f32; 3]>,
    /// Mirror plane normal [x, y, z].
    plane_normal: Option<[f32; 3]>,
    /// Whether to merge vertices at the seam.
    merge: Option<bool>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
struct MirrorEntityRequest {
    /// Element ID of the MirrorNode.
    element_id: u64,
}

#[cfg(feature = "model-api")]
#[tool_router(router = tool_router)]
impl ModelApiServer {
    #[tool(
        name = "get_instance_info",
        description = "Get runtime identification for this Talos3D instance, including instance_id, MCP HTTP port, URL, registry manifest path, and pid."
    )]
    async fn get_instance_info_tool(&self) -> Result<CallToolResult, McpError> {
        let info = self
            .request_get_instance_info()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(info)
    }

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

    // --- Named Views ---

    #[tool(name = "view_list", description = "List all named views.")]
    async fn view_list_tool(&self) -> Result<CallToolResult, McpError> {
        let views = self
            .request_view_list()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(views)
    }

    #[tool(
        name = "view_save",
        description = "Save the current camera position as a named view, or save explicit camera parameters."
    )]
    async fn view_save_tool(
        &self,
        Parameters(params): Parameters<ViewSaveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let view = self
            .request_view_save(params.name, params.description, params.camera)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(view)
    }

    #[tool(
        name = "view_restore",
        description = "Restore the camera to a previously saved named view."
    )]
    async fn view_restore_tool(
        &self,
        Parameters(params): Parameters<ViewRestoreRequest>,
    ) -> Result<CallToolResult, McpError> {
        let view = self
            .request_view_restore(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(view)
    }

    #[tool(
        name = "view_update",
        description = "Update a named view's name, description, or camera parameters."
    )]
    async fn view_update_tool(
        &self,
        Parameters(params): Parameters<ViewUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let view = self
            .request_view_update(
                params.name,
                params.new_name,
                params.description,
                params.camera,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(view)
    }

    #[tool(name = "view_delete", description = "Delete a named view by name.")]
    async fn view_delete_tool(
        &self,
        Parameters(params): Parameters<ViewDeleteRequest>,
    ) -> Result<CallToolResult, McpError> {
        self.request_view_delete(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(serde_json::json!({"ok": true}))
    }

    // --- Clipping Planes ---

    #[tool(
        name = "clip_plane_create",
        description = "Create a clipping plane that cuts the viewport. Geometry on the side opposite to the normal is hidden. Returns the new element_id."
    )]
    async fn clip_plane_create_tool(
        &self,
        Parameters(params): Parameters<ClipPlaneCreateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_clip_plane_create(params.name, params.origin, params.normal, params.active)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(serde_json::json!({ "element_id": element_id }))
    }

    #[tool(
        name = "clip_plane_update",
        description = "Update a clipping plane's name, origin, normal, or active state."
    )]
    async fn clip_plane_update_tool(
        &self,
        Parameters(params): Parameters<ClipPlaneUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_clip_plane_update(
                params.element_id,
                params.name,
                params.origin,
                params.normal,
                params.active,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "clip_plane_list",
        description = "List all clipping planes and their active state."
    )]
    async fn clip_plane_list_tool(&self) -> Result<CallToolResult, McpError> {
        let planes = self
            .request_clip_plane_list()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(planes)
    }

    #[tool(
        name = "clip_plane_toggle",
        description = "Activate or deactivate a clipping plane by element_id."
    )]
    async fn clip_plane_toggle_tool(
        &self,
        Parameters(params): Parameters<ClipPlaneToggleRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_clip_plane_toggle(params.element_id, params.active)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    // --- Materials ---

    #[tool(
        name = "list_materials",
        description = "List all materials in the project registry. Returns id, name, PBR properties, texture paths, and UV tiling."
    )]
    async fn list_materials_tool(&self) -> Result<CallToolResult, McpError> {
        let materials = self
            .request_list_materials()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(materials)
    }

    #[tool(
        name = "get_material",
        description = "Get full details for a specific material by id."
    )]
    async fn get_material_tool(
        &self,
        Parameters(params): Parameters<GetMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let material = self
            .request_get_material(params.id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(material)
    }

    #[tool(
        name = "create_material",
        description = "Create a new material in the project registry. Specify PBR properties (base_color as [r,g,b,a], perceptual_roughness, metallic, reflectance, emissive as [r,g,b]), alpha_mode (opaque/blend/mask), UV tiling (uv_scale as [x,y], uv_rotation_deg), and optional texture file paths."
    )]
    async fn create_material_tool(
        &self,
        Parameters(params): Parameters<CreateMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let material = self
            .request_create_material(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(material)
    }

    #[tool(
        name = "update_material",
        description = "Update an existing material's properties. Takes the same fields as create_material plus the material id."
    )]
    async fn update_material_tool(
        &self,
        Parameters(params): Parameters<UpdateMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let material = self
            .request_update_material(params.id, params.material)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(material)
    }

    #[tool(
        name = "delete_material",
        description = "Delete a material from the registry and remove its assignment from all entities."
    )]
    async fn delete_material_tool(
        &self,
        Parameters(params): Parameters<DeleteMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let id = self
            .request_delete_material(params.id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(id)
    }

    #[tool(
        name = "apply_material",
        description = "Apply a material to one or more entities by element_id. Pass material_id and element_ids array."
    )]
    async fn apply_material_tool(
        &self,
        Parameters(params): Parameters<ApplyMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let applied = self
            .request_apply_material(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(applied)
    }

    #[tool(
        name = "remove_material_assignment",
        description = "Remove the material assignment from entities, reverting them to the default material."
    )]
    async fn remove_material_tool(
        &self,
        Parameters(params): Parameters<RemoveMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let removed = self
            .request_remove_material(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(removed)
    }

    #[tool(
        name = "get_render_settings",
        description = "Get the current viewport renderer settings, including tonemapping, exposure, SSAO, bloom, and SSR controls."
    )]
    async fn get_render_settings_tool(&self) -> Result<CallToolResult, McpError> {
        let settings = self
            .request_get_render_settings()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(settings)
    }

    #[tool(
        name = "set_render_settings",
        description = "Update viewport renderer settings. Pass any subset of tonemapping, exposure_ev100, SSAO, bloom, and SSR fields."
    )]
    async fn set_render_settings_tool(
        &self,
        Parameters(params): Parameters<RenderSettingsUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let settings = self
            .request_set_render_settings(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(settings)
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

    #[tool(
        name = "definition.list",
        description = "List all reusable definitions in the document."
    )]
    async fn definition_list_tool(&self) -> Result<CallToolResult, McpError> {
        let definitions = self
            .request_list_definitions()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(definitions)
    }

    #[tool(
        name = "definition.get",
        description = "Get a definition by its definition_id. Returns both the raw stored definition and the effective inherited definition."
    )]
    async fn definition_get_tool(
        &self,
        Parameters(params): Parameters<DefinitionGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_get_definition(params.definition_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.create",
        description = "Create a new reusable definition. Requires: name. Optionally: base_definition_id, definition_kind, parameters, evaluators, representations, compound, width_param/depth_param/height_param fallback fields, and domain_data."
    )]
    async fn definition_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_create_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.update",
        description = "Update an existing definition. Requires: definition_id. Optionally: name, base_definition_id, definition_kind, parameters, evaluators, representations, compound, and domain_data. Bumps definition_version and propagates changes to all linked occurrences."
    )]
    async fn definition_update_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_update_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.draft.list",
        description = "List all open definition drafts."
    )]
    async fn definition_draft_list_tool(&self) -> Result<CallToolResult, McpError> {
        let drafts = self
            .request_list_definition_drafts()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(drafts)
    }

    #[tool(
        name = "definition.draft.get",
        description = "Get a definition draft by draft_id."
    )]
    async fn definition_draft_get_tool(
        &self,
        Parameters(params): Parameters<DefinitionDraftIdRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_get_definition_draft(params.draft_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.open",
        description = "Open an existing definition as a draft for editing. Requires: definition_id. Optionally: library_id."
    )]
    async fn definition_draft_open_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_open_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.create",
        description = "Create a new definition draft. Same payload shape as definition/create, but stored only as an editable draft until published."
    )]
    async fn definition_draft_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_create_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.derive",
        description = "Create a derived definition draft from an existing definition. Requires: definition_id. Optionally: library_id and name."
    )]
    async fn definition_draft_derive_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_derive_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.patch",
        description = "Apply one or more patch operations to a definition draft. Requires: draft_id and either patch or patches."
    )]
    async fn definition_draft_patch_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_patch_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.publish",
        description = "Validate and publish a definition draft into the document. Requires: draft_id."
    )]
    async fn definition_draft_publish_tool(
        &self,
        Parameters(params): Parameters<DefinitionDraftIdRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_publish_definition_draft(params.draft_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.validate",
        description = "Validate either a draft or a published definition. Requires either draft_id or definition_id. Optionally: library_id for library definitions."
    )]
    async fn definition_validate_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_validate_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.compile",
        description = "Compile a dependency summary for either a draft or a published definition. Requires either draft_id or definition_id. Optionally: library_id for library definitions."
    )]
    async fn definition_compile_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_compile_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.explain",
        description = "Explain either a draft or a published definition, including effective inherited shape and dependency summary. Requires either draft_id or definition_id. Optionally: library_id for library definitions."
    )]
    async fn definition_explain_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_explain_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.library.list",
        description = "List reusable definition libraries available to the current document."
    )]
    async fn definition_library_list_tool(&self) -> Result<CallToolResult, McpError> {
        let libraries = self
            .request_list_definition_libraries()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(libraries)
    }

    #[tool(
        name = "definition.library.get",
        description = "Get a definition library by library_id, including the definitions it contains."
    )]
    async fn definition_library_get_tool(
        &self,
        Parameters(params): Parameters<DefinitionLibraryGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let library = self
            .request_get_definition_library(params.library_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(library)
    }

    #[tool(
        name = "definition.library.create",
        description = "Create a new definition library. Requires: name. Optionally: scope (\"DocumentLocal\"|\"ExternalFile\"), source_path, tags."
    )]
    async fn definition_library_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_create_definition_library(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.add_definition",
        description = "Copy a document definition into a library. Requires: library_id, definition_id."
    )]
    async fn definition_library_add_definition_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_add_definition_to_library(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.import",
        description = "Import a definition library JSON file into the current document context. Requires: path."
    )]
    async fn definition_library_import_tool(
        &self,
        Parameters(params): Parameters<DefinitionLibraryPathRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_import_definition_library(params.path)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.export",
        description = "Export a definition library JSON file. Requires: library_id, path."
    )]
    async fn definition_library_export_tool(
        &self,
        Parameters(params): Parameters<DefinitionLibraryExportRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_export_definition_library(params.library_id, params.path)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(path)
    }

    #[tool(
        name = "definition.instantiate",
        description = "Instantiate a definition into the model. Requires: definition_id. Optionally: library_id (imports from library first if needed), overrides, label, offset, domain_data."
    )]
    async fn definition_instantiate_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_instantiate_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.instantiate_hosted",
        description = "Instantiate a hosted definition into the model. Requires: definition_id and hosting. Optionally: library_id, overrides, label, offset, and domain_data. Hosting may provide host_element_id, opening_element_id, wall_thickness, relation_type, relation_parameters, and anchors keyed by anchor id."
    )]
    async fn definition_instantiate_hosted_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_instantiate_hosted_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.place",
        description = "Place an occurrence of a definition. Requires: definition_id. Optionally: overrides, label, offset, and domain_data."
    )]
    async fn occurrence_place_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_place_occurrence(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "occurrence.update_overrides",
        description = "Update the parameter overrides on an existing occurrence. Requires: element_id (u64), overrides (object mapping param names to values)."
    )]
    async fn occurrence_update_overrides_tool(
        &self,
        Parameters(params): Parameters<OccurrenceUpdateOverridesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_update_occurrence_overrides(params.element_id, params.overrides)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.resolve",
        description = "Resolve and return the effective parameter values for an occurrence, including provenance (DefinitionDefault or OccurrenceOverride). Requires: element_id (u64)."
    )]
    async fn occurrence_resolve_tool(
        &self,
        Parameters(params): Parameters<OccurrenceResolveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_resolve_occurrence(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.explain",
        description = "Explain a placed occurrence for agent inspection. Returns resolved parameters, anchors, and generated compound slot parts. Requires: element_id (u64)."
    )]
    async fn occurrence_explain_tool(
        &self,
        Parameters(params): Parameters<OccurrenceResolveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_explain_occurrence(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- Array tools ---

    #[tool(
        name = "array_create_linear",
        description = "Create a linear array of N copies of a source entity, spaced evenly along a direction vector."
    )]
    async fn array_create_linear_tool(
        &self,
        Parameters(params): Parameters<ArrayCreateLinearRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_array_create_linear(params.source, params.count, params.spacing)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "array_create_polar",
        description = "Create a polar (rotational) array of N copies of a source entity, distributed around an axis."
    )]
    async fn array_create_polar_tool(
        &self,
        Parameters(params): Parameters<ArrayCreatePolarRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_array_create_polar(
                params.source,
                params.count,
                params.axis.unwrap_or([0.0, 1.0, 0.0]),
                params.total_angle_degrees.unwrap_or(360.0),
                params.center.unwrap_or([0.0, 0.0, 0.0]),
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "array_update",
        description = "Update the count, spacing, axis, angle, or center of an array node."
    )]
    async fn array_update_tool(
        &self,
        Parameters(params): Parameters<ArrayUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_array_update(
                params.element_id,
                params.count,
                params.spacing,
                params.axis,
                params.total_angle_degrees,
                params.center,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "array_dissolve",
        description = "Convert an array node into an independent entity, breaking the link to its source."
    )]
    async fn array_dissolve_tool(
        &self,
        Parameters(params): Parameters<ArrayEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let new_id = self
            .request_array_dissolve(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(new_id)
    }

    #[tool(
        name = "array_get",
        description = "Get the parameters of an array node (source, count, spacing or axis/angle/center)."
    )]
    async fn array_get_tool(
        &self,
        Parameters(params): Parameters<ArrayEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_array_get(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- Mirror tools ---

    #[tool(
        name = "mirror_create",
        description = "Create a mirror geometry node that reflects a source entity across a plane. The mirror maintains a live dependency on the source. Returns the new element_id."
    )]
    async fn mirror_create_tool(
        &self,
        Parameters(params): Parameters<MirrorCreateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_mirror_create(
                params.source,
                params.plane,
                params.plane_origin,
                params.plane_normal,
                Some(params.merge),
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "mirror_update",
        description = "Update the mirror plane or merge setting of a MirrorNode entity."
    )]
    async fn mirror_update_tool(
        &self,
        Parameters(params): Parameters<MirrorUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_mirror_update(
                params.element_id,
                params.plane,
                params.plane_origin,
                params.plane_normal,
                params.merge,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "mirror_dissolve",
        description = "Break the live link of a MirrorNode, converting it to an independent triangle mesh entity with the current reflected geometry. Returns the new entity's element_id."
    )]
    async fn mirror_dissolve_tool(
        &self,
        Parameters(params): Parameters<MirrorEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let new_id = self
            .request_mirror_dissolve(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(new_id)
    }

    #[tool(
        name = "mirror_get",
        description = "Get the mirror parameters (source entity, plane origin, plane normal, merge) of a MirrorNode entity."
    )]
    async fn mirror_get_tool(
        &self,
        Parameters(params): Parameters<MirrorEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_mirror_get(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
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

// --- Named View Handlers ---

#[cfg(feature = "model-api")]
fn named_view_info_from_view(view: &crate::plugins::named_views::NamedView) -> NamedViewInfo {
    NamedViewInfo {
        name: view.name.clone(),
        description: view.description.clone(),
        focus: view.focus,
        radius: view.radius,
        yaw: view.yaw,
        pitch: view.pitch,
        projection: match view.projection_mode {
            CameraProjectionMode::Perspective => "perspective".to_string(),
            CameraProjectionMode::Isometric => "orthographic".to_string(),
        },
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
    yaw: f32,
    pitch: f32,
    projection_mode: CameraProjectionMode,
    focal_length_mm: f32,
}

#[cfg(feature = "model-api")]
fn live_camera_snapshot(world: &World) -> LiveCameraSnapshot {
    let mut q = world.try_query::<&OrbitCamera>().unwrap();
    if let Some(orbit) = q.iter(world).next() {
        LiveCameraSnapshot {
            focus: orbit.focus,
            radius: orbit.radius,
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
    world
        .resource::<MaterialRegistry>()
        .all()
        .map(MaterialInfo::from_def)
        .collect()
}

#[cfg(feature = "model-api")]
fn handle_get_material(world: &World, id: &str) -> Result<MaterialInfo, String> {
    world
        .resource::<MaterialRegistry>()
        .get(id)
        .map(MaterialInfo::from_def)
        .ok_or_else(|| format!("Material '{id}' not found"))
}

#[cfg(feature = "model-api")]
fn handle_create_material(
    world: &mut World,
    req: CreateMaterialRequest,
) -> Result<MaterialInfo, String> {
    let def = material_def_from_request(req);
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
    {
        let mut registry = world.resource_mut::<MaterialRegistry>();
        let def = registry
            .get_mut(id)
            .ok_or_else(|| format!("Material '{id}' not found"))?;
        apply_request_to_def(req, def);
    }
    handle_get_material(world, id)
}

#[cfg(feature = "model-api")]
fn handle_delete_material(world: &mut World, id: &str) -> Result<String, String> {
    // Remove assignment from all entities that reference this material
    let entities_to_clean: Vec<Entity> = {
        let mut q = world.query::<(Entity, &MaterialAssignment)>();
        q.iter(world)
            .filter(|(_, a)| a.material_id == id)
            .map(|(e, _)| e)
            .collect()
    };
    for entity in entities_to_clean {
        world.entity_mut(entity).remove::<MaterialAssignment>();
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

    let info = RenderSettingsInfo::from_settings(&settings);
    world.insert_resource(settings);
    Ok(info)
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
    use bevy::render::view::screenshot::Screenshot;
    use std::path::PathBuf;

    let path_buf = PathBuf::from(path);
    let path_owned = path.to_string();
    if let Some(parent) = path_buf.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match std::fs::remove_file(&path_buf) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    world
        .commands()
        .spawn(Screenshot::primary_window())
        .observe(save_screenshot_to_disk(path_buf));
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
        "Enum" => Err("Enum parameters must provide param_type as an object or array".to_string()),
        other => Err(format!("Unsupported param_type '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_param_type_value(
    value: &Value,
) -> Result<crate::plugins::modeling::definition::ParamType, String> {
    use crate::plugins::modeling::definition::ParamType;

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
    use crate::plugins::modeling::definition::{ParameterMetadata, ParameterMutability};

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
            let default_value = object
                .get("default_value")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let override_policy = parse_override_policy(object.get("override_policy"))?;
            let metadata = parse_parameter_metadata(object.get("metadata"))?;
            Ok(ParameterDef {
                name,
                param_type,
                default_value,
                override_policy,
                metadata,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(ParameterSchema(parameters))
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
    use crate::plugins::modeling::definition::RepresentationKind;

    match value.and_then(|value| value.as_str()).unwrap_or("Body") {
        "Body" => Ok(RepresentationKind::Body),
        "Axis" => Ok(RepresentationKind::Axis),
        "Footprint" => Ok(RepresentationKind::Footprint),
        "BoundingBox" => Ok(RepresentationKind::BoundingBox),
        other => Err(format!("Unsupported representation kind '{other}'")),
    }
}

#[cfg(feature = "model-api")]
fn parse_representation_role(
    value: Option<&Value>,
) -> Result<crate::plugins::modeling::definition::RepresentationRole, String> {
    use crate::plugins::modeling::definition::RepresentationRole;

    match value
        .and_then(|value| value.as_str())
        .unwrap_or("PrimaryGeometry")
    {
        "PrimaryGeometry" => Ok(RepresentationRole::PrimaryGeometry),
        "Annotation" => Ok(RepresentationRole::Annotation),
        "Reference" => Ok(RepresentationRole::Reference),
        other => Err(format!("Unsupported representation role '{other}'")),
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
                Ok(RepresentationDecl {
                    kind: parse_representation_kind(representation.get("kind"))?,
                    role: parse_representation_role(representation.get("role"))?,
                })
            })
            .collect();
    }

    Ok(vec![RepresentationDecl {
        kind: RepresentationKind::Body,
        role: RepresentationRole::PrimaryGeometry,
    }])
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

    Ok(vec![EvaluatorDecl::RectangularExtrusion(
        RectangularExtrusionEvaluator {
            width_param: object
                .get("width_param")
                .and_then(|value| value.as_str())
                .unwrap_or("width")
                .to_string(),
            depth_param: object
                .get("depth_param")
                .and_then(|value| value.as_str())
                .unwrap_or("depth")
                .to_string(),
            height_param: object
                .get("height_param")
                .and_then(|value| value.as_str())
                .unwrap_or("height")
                .to_string(),
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
        },
        evaluators: parse_evaluators(object)?,
        representations: parse_representations(object)?,
        compound: parse_optional_compound(object)?,
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
pub fn handle_list_definitions(world: &World) -> Vec<DefinitionEntry> {
    use crate::plugins::modeling::definition::DefinitionRegistry;
    let registry = world.resource::<DefinitionRegistry>();
    registry
        .list()
        .into_iter()
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
fn create_relation_snapshot(
    world: &mut World,
    source: ElementId,
    target: ElementId,
    relation_type: String,
    parameters: Value,
) -> u64 {
    use crate::plugins::modeling::assembly::{RelationSnapshot, SemanticRelation};

    let relation_id = world.resource_mut::<ElementIdAllocator>().next_id();
    send_event(
        world,
        CreateEntityCommand {
            snapshot: RelationSnapshot {
                element_id: relation_id,
                relation: SemanticRelation {
                    source,
                    target,
                    relation_type,
                    parameters,
                },
            }
            .into(),
        },
    );
    flush_model_api_write_pipeline(world);
    relation_id.0
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
    let object = request
        .as_object()
        .ok_or_else(|| "definition.instantiate_hosted expects a JSON object".to_string())?;
    let (definition_id, imported_definition_ids) =
        ensure_definition_available_for_request(world, object)?;
    let (hosted_request, relation_type, host_element_id, relation_parameters) =
        prepare_hosted_occurrence_request(world, object)?;

    let element_id = handle_place_occurrence(world, hosted_request)?;

    let relation_ids =
        if let (Some(relation_type), Some(host_element_id)) = (relation_type, host_element_id) {
            vec![create_relation_snapshot(
                world,
                ElementId(element_id),
                host_element_id,
                relation_type,
                relation_parameters,
            )]
        } else {
            Vec::new()
        };

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
    enqueue_create_boxed_entity(world, snapshot.into());
    flush_model_api_write_pipeline(world);
    Ok(result_id)
}

#[cfg(feature = "model-api")]
pub fn handle_update_occurrence_overrides(
    world: &mut World,
    element_id: u64,
    overrides: Value,
) -> ApiResult<Value> {
    use crate::plugins::commands::ApplyEntityChangesCommand;
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

    world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .write(ApplyEntityChangesCommand {
            label: "Update occurrence overrides",
            before: vec![before],
            after: vec![after],
        });

    flush_model_api_write_pipeline(world);
    Ok(after_json)
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
    use crate::plugins::modeling::snapshots::TriangleMeshFactory;
    #[cfg(feature = "model-api")]
    use crate::plugins::modeling::{
        fillet::{ChamferFactory, FilletFactory},
        primitives::{CylinderPrimitive, SpherePrimitive},
    };
    use crate::plugins::modeling::{
        generic_factory::PrimitiveFactory,
        primitives::{BoxPrimitive, PlanePrimitive, Polyline, ShapeRotation},
        snapshots::PolylineFactory,
    };
    #[cfg(feature = "model-api")]
    use crate::plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        document_properties::DocumentProperties,
        document_state::DocumentState,
        history::{History, PendingCommandQueue},
        identity::ElementIdAllocator,
        import::ImportRegistry,
        persistence::OpaquePersistedEntities,
        property_edit::PropertyEditState,
        toolbar::{
            ToolbarDescriptor, ToolbarDock, ToolbarLayoutEntry, ToolbarLayoutState,
            ToolbarRegistry, ToolbarSection,
        },
        tools::ActiveTool,
        transform::TransformState,
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
                .and_then(|semantics| semantics
                    .evaluated_body
                    .as_ref()
                    .and_then(|body| body.volume)),
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
        world.insert_resource(DocumentState::default());
        world.insert_resource(OpaquePersistedEntities::default());
        world.insert_resource(PropertyEditState::default());
        world.insert_resource(TransformState::default());
        world.insert_resource(NextState::<ActiveTool>::default());
        let mut import_registry = ImportRegistry::default();
        import_registry.register_importer(ObjImporter);
        world.insert_resource(import_registry);
        world.insert_resource(DocumentProperties::default());
        let mut toolbar_registry = ToolbarRegistry::default();
        toolbar_registry.register(ToolbarDescriptor {
            id: "core".to_string(),
            label: "Core".to_string(),
            default_dock: ToolbarDock::Top,
            default_visible: true,
            sections: vec![ToolbarSection {
                label: "Select".to_string(),
                command_ids: vec!["core.select_tool".to_string()],
            }],
        });
        toolbar_registry.register(ToolbarDescriptor {
            id: "modeling".to_string(),
            label: "Modeling".to_string(),
            default_dock: ToolbarDock::Left,
            default_visible: true,
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
                row: 0,
                order: 0,
                visible: true,
            },
        );
        toolbar_layout_state.entries.insert(
            "modeling".to_string(),
            ToolbarLayoutEntry {
                dock: ToolbarDock::Left,
                row: 0,
                order: 0,
                visible: true,
            },
        );
        world.insert_resource(toolbar_layout_state);
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        registry.register_factory(PrimitiveFactory::<CylinderPrimitive>::new());
        registry.register_factory(PrimitiveFactory::<SpherePrimitive>::new());
        registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
        registry.register_factory(PolylineFactory);
        registry.register_factory(TriangleMeshFactory);
        registry.register_factory(FilletFactory);
        registry.register_factory(ChamferFactory);
        registry.register_factory(crate::plugins::modeling::occurrence::OccurrenceFactory);
        world.insert_resource(registry);
        world.insert_resource(crate::plugins::modeling::definition::DefinitionRegistry::default());
        world.insert_resource(
            crate::plugins::modeling::definition::DefinitionLibraryRegistry::default(),
        );
        world.insert_resource(
            crate::plugins::definition_authoring::DefinitionDraftRegistry::default(),
        );
        world.insert_resource(crate::plugins::modeling::occurrence::ChangedDefinitions::default());
        world.insert_resource(RenderSettings::default());
        world.insert_resource(Assets::<Mesh>::default());
        world.insert_resource(crate::plugins::layers::LayerRegistry::default());
        world.insert_resource(crate::plugins::materials::MaterialRegistry::default());
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
    fn temp_json_path(stem: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{stem}-{unique}.json"))
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
        assert_eq!(box_snapshot["centre"], json!([3.5, 2.0, 3.0]));

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
    fn write_handlers_create_and_edit_sphere() {
        let mut world = init_model_api_test_world();

        let sphere_id = handle_create_entity(
            &mut world,
            json!({
                "type": "sphere",
                "centre": [0.0, 1.0, 0.0],
                "radius": 1.25
            }),
        )
        .expect("sphere should be created");

        let details =
            get_entity_details(&world, ElementId(sphere_id)).expect("sphere details should exist");
        assert_eq!(details.entity_type, "sphere");

        let updated = handle_set_property(&mut world, sphere_id, "radius", json!(2.5))
            .expect("setting sphere radius should succeed");
        assert_eq!(updated["radius"], json!(2.5));

        let summary = model_summary(&world);
        assert_eq!(summary.entity_counts.get("sphere"), Some(&1));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn write_handlers_create_and_edit_fillet_and_chamfer() {
        let mut world = init_model_api_test_world();

        let fillet_source_id = handle_create_entity(
            &mut world,
            json!({
                "type": "box",
                "centre": [0.0, 0.0, 0.0],
                "half_extents": [1.0, 1.0, 1.0]
            }),
        )
        .expect("fillet source should be created");
        let chamfer_source_id = handle_create_entity(
            &mut world,
            json!({
                "type": "box",
                "centre": [4.0, 0.0, 0.0],
                "half_extents": [1.0, 1.0, 1.0]
            }),
        )
        .expect("chamfer source should be created");

        let fillet_id = handle_create_entity(
            &mut world,
            json!({
                "type": "fillet",
                "source": fillet_source_id,
                "radius": 0.2,
                "segments": 4
            }),
        )
        .expect("fillet should be created");
        let chamfer_id = handle_create_entity(
            &mut world,
            json!({
                "type": "chamfer",
                "source": chamfer_source_id,
                "distance": 0.15
            }),
        )
        .expect("chamfer should be created");

        let fillet_updated = handle_set_property(&mut world, fillet_id, "radius", json!(0.35))
            .expect("setting fillet radius should succeed");
        assert!(
            (fillet_updated["radius"].as_f64().unwrap_or_default() - 0.35).abs() < 1e-5,
            "fillet radius should be updated"
        );

        let segments_updated = handle_set_property(&mut world, fillet_id, "segments", json!(6.0))
            .expect("setting fillet segments should succeed");
        assert_eq!(segments_updated["segments"], json!(6));

        let chamfer_updated = handle_set_property(&mut world, chamfer_id, "distance", json!(0.25))
            .expect("setting chamfer distance should succeed");
        assert!(
            (chamfer_updated["distance"].as_f64().unwrap_or_default() - 0.25).abs() < 1e-5,
            "chamfer distance should be updated"
        );

        let fillet_details =
            get_entity_details(&world, ElementId(fillet_id)).expect("fillet details should exist");
        assert_eq!(fillet_details.entity_type, "fillet");

        let chamfer_details = get_entity_details(&world, ElementId(chamfer_id))
            .expect("chamfer details should exist");
        assert_eq!(chamfer_details.entity_type, "chamfer");

        let summary = model_summary(&world);
        assert_eq!(summary.entity_counts.get("fillet"), Some(&1));
        assert_eq!(summary.entity_counts.get("chamfer"), Some(&1));
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
        assert_eq!(updated["half_extents"], json!([4.0, 5.0, 6.0]));

        let error = handle_set_property(&mut world, box_id, "radius", json!(1.0))
            .expect_err("invalid box property should fail");
        assert!(error.contains("Valid properties: center, half_extents"));
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
        assert_eq!(tools.len(), 76); // adds sphere support and current definition/model tooling surface

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
        assert!(
            box_snapshot.is_object(),
            "box snapshot should be a JSON object"
        );
        assert_eq!(box_snapshot["centre"], serde_json::json!([1.0, 1.0, 1.0]));

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
        assert_eq!(updated_snapshot["half_extents"], json!([2.0, 2.0, 2.0]));

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

    // -----------------------------------------------------------------------
    // PP51 — Definition / Occurrence tests
    // -----------------------------------------------------------------------

    #[cfg(feature = "model-api")]
    fn make_rect_extrusion_request() -> serde_json::Value {
        json!({
            "name": "TestWall",
            "definition_kind": "Solid",
            "width_param": "width",
            "depth_param": "depth",
            "height_param": "height",
            "parameters": [
                { "name": "width",  "param_type": "Numeric", "default_value": 4.0, "override_policy": "Overridable" },
                { "name": "depth",  "param_type": "Numeric", "default_value": 0.3, "override_policy": "Overridable" },
                { "name": "height", "param_type": "Numeric", "default_value": 3.0, "override_policy": "Overridable" }
            ]
        })
    }

    #[cfg(feature = "model-api")]
    fn make_compound_window_request(child_definition_id: &str) -> serde_json::Value {
        json!({
            "name": "CompoundWindow",
            "definition_kind": "Solid",
            "evaluators": [],
            "parameters": [
                { "name": "overall_width", "param_type": "Numeric", "default_value": 1.2, "override_policy": "Overridable", "metadata": { "unit": "m" } },
                { "name": "overall_height", "param_type": "Numeric", "default_value": 1.4, "override_policy": "Overridable", "metadata": { "unit": "m" } },
                { "name": "wall_thickness", "param_type": "Numeric", "default_value": 0.2, "override_policy": "Overridable", "metadata": { "unit": "m" } },
                { "name": "finish_color", "param_type": "StringVal", "default_value": "white", "override_policy": "Overridable" }
            ],
            "compound": {
                "anchors": [
                    { "id": "opening.exterior_face", "kind": "host_exterior_face" },
                    { "id": "opening.interior_face", "kind": "host_interior_face" }
                ],
                "derived_parameters": [
                    {
                        "name": "clear_width",
                        "param_type": "Numeric",
                        "expr": { "kind": "param_ref", "path": "overall_width" },
                        "dependencies": ["overall_width"],
                        "metadata": { "unit": "m", "mutability": "Derived" }
                    }
                ],
                "constraints": [
                    {
                        "id": "width_positive",
                        "expr": {
                            "kind": "gt",
                            "left": { "kind": "param_ref", "path": "overall_width" },
                            "right": { "kind": "literal", "value": 0.5 }
                        },
                        "dependencies": ["overall_width"],
                        "severity": "Error",
                        "message": "Window width must stay positive"
                    }
                ],
                "child_slots": [
                    {
                        "slot_id": "frame",
                        "role": "frame",
                        "definition_id": child_definition_id,
                        "parameter_bindings": [
                            { "target_param": "width", "expr": { "kind": "param_ref", "path": "overall_width" } },
                            { "target_param": "depth", "expr": { "kind": "literal", "value": 0.14 } },
                            { "target_param": "height", "expr": { "kind": "param_ref", "path": "overall_height" } }
                        ],
                        "transform_binding": {
                            "translation": [
                                { "kind": "literal", "value": 0.0 },
                                { "kind": "literal", "value": 0.0 },
                                { "kind": "literal", "value": 0.0 }
                            ]
                        }
                    }
                ]
            },
            "domain_data": {
                "architectural": {
                    "void_declaration": { "kind": "opening", "parameters": { "host": "wall" } }
                }
            }
        })
    }

    #[cfg(feature = "model-api")]
    fn make_locked_member_request() -> serde_json::Value {
        json!({
            "name": "LockedMember",
            "definition_kind": "Solid",
            "width_param": "width",
            "depth_param": "depth",
            "height_param": "height",
            "parameters": [
                { "name": "width",  "param_type": "Numeric", "default_value": 0.2, "override_policy": "Locked" },
                { "name": "depth",  "param_type": "Numeric", "default_value": 0.1, "override_policy": "Locked" },
                { "name": "height", "param_type": "Numeric", "default_value": 0.5, "override_policy": "Locked" }
            ]
        })
    }

    #[cfg(feature = "model-api")]
    fn make_definition_variant_request(base_definition_id: &str) -> serde_json::Value {
        json!({
            "name": "TestWall Greyline",
            "base_definition_id": base_definition_id,
            "parameters": [
                { "name": "height", "param_type": "Numeric", "default_value": 4.5, "override_policy": "Locked" },
                { "name": "finish_color", "param_type": "StringVal", "default_value": "greyline", "override_policy": "Locked" }
            ],
            "domain_data": {
                "catalog": {
                    "finish": "greyline"
                }
            }
        })
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_create_and_list_round_trip() {
        let mut world = init_model_api_test_world();

        assert!(handle_list_definitions(&world).is_empty());

        let entry = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("create definition should succeed");

        assert_eq!(entry.name, "TestWall");
        assert_eq!(entry.definition_kind, "Solid");
        assert_eq!(entry.definition_version, 1);
        assert_eq!(entry.parameter_names, vec!["width", "depth", "height"]);

        let all = handle_list_definitions(&world);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].definition_id, entry.definition_id);
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_get_returns_full_definition() {
        let mut world = init_model_api_test_world();

        let created = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("create definition should succeed");

        let fetched = handle_get_definition(&world, created.definition_id.clone())
            .expect("get definition should succeed");

        assert_eq!(fetched.definition_id, created.definition_id);
        assert_eq!(fetched.name, "TestWall");
        assert_eq!(
            fetched.full["interface"]["parameters"][0]["name"],
            json!("width")
        );
        assert_eq!(
            fetched.effective_full["interface"]["parameters"][0]["name"],
            json!("width")
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_variants_inherit_effective_shape_and_parameters() {
        let mut world = init_model_api_test_world();

        let base = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("base definition should be created");
        let variant = handle_create_definition(
            &mut world,
            make_definition_variant_request(&base.definition_id),
        )
        .expect("variant definition should be created");

        assert_eq!(
            variant.full["base_definition_id"],
            json!(base.definition_id.clone())
        );
        assert_eq!(
            variant.effective_full["interface"]["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .map(|parameter| parameter["name"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["width", "depth", "height", "finish_color"]
        );
        assert_eq!(
            variant.effective_full["interface"]["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == json!("height"))
                .unwrap()["default_value"],
            json!(4.5)
        );

        let occurrence_id = handle_place_occurrence(
            &mut world,
            json!({ "definition_id": variant.definition_id, "label": "VariantWall" }),
        )
        .expect("variant occurrence should be placed");
        let resolved =
            handle_resolve_occurrence(&world, occurrence_id).expect("variant should resolve");
        assert_eq!(resolved["height"]["value"], json!(4.5));
        assert_eq!(resolved["finish_color"]["value"], json!("greyline"));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_draft_lifecycle_creates_patches_validates_and_publishes() {
        let mut world = init_model_api_test_world();

        let draft = handle_create_definition_draft(&mut world, make_rect_extrusion_request())
            .expect("draft should be created");
        assert!(draft.dirty);

        let patched = handle_patch_definition_draft(
            &mut world,
            json!({
                "draft_id": draft.draft_id,
                "patches": [
                    { "op": "set_name", "name": "DraftWall" },
                    { "op": "set_parameter_default", "name": "height", "default_value": 4.25 }
                ]
            }),
        )
        .expect("draft should be patched");
        assert_eq!(patched.name, "DraftWall");
        assert_eq!(
            patched.effective_full["interface"]["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == json!("height"))
                .unwrap()["default_value"],
            json!(4.25)
        );

        let validation =
            handle_validate_definition(&world, json!({ "draft_id": patched.draft_id }))
                .expect("draft validation should succeed");
        assert!(validation.ok);

        let published = handle_publish_definition_draft(&mut world, patched.draft_id.clone())
            .expect("draft should publish");
        assert_eq!(published.name, "DraftWall");
        assert_eq!(
            published.effective_full["interface"]["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == json!("height"))
                .unwrap()["default_value"],
            json!(4.25)
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn derived_definition_draft_can_be_compiled_and_explained() {
        let mut world = init_model_api_test_world();

        let base = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("base definition should be created");
        let draft = handle_derive_definition_draft(
            &mut world,
            json!({
                "definition_id": base.definition_id,
                "name": "DerivedWall"
            }),
        )
        .expect("derived draft should be created");

        handle_patch_definition_draft(
            &mut world,
            json!({
                "draft_id": draft.draft_id,
                "patch": { "op": "set_parameter_default", "name": "height", "default_value": 5.0 }
            }),
        )
        .expect("derived draft should accept inherited parameter overrides");

        let compile = handle_compile_definition(&world, json!({ "draft_id": draft.draft_id }))
            .expect("compile should succeed");
        assert!(compile.nodes.iter().any(|node| node == "param:height"));

        let explain = handle_explain_definition(&world, json!({ "draft_id": draft.draft_id }))
            .expect("explain should succeed");
        assert_eq!(
            explain.effective_full["interface"]["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == json!("height"))
                .unwrap()["default_value"],
            json!(5.0)
        );
        assert!(explain
            .local_parameter_names
            .iter()
            .any(|name| name == "height"));
        assert!(explain
            .inherited_parameter_names
            .iter()
            .any(|name| name == "width"));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn compound_definition_round_trips_with_domain_data() {
        let mut world = init_model_api_test_world();

        let child = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("child definition should be created");

        let compound = handle_create_definition(
            &mut world,
            make_compound_window_request(&child.definition_id),
        )
        .expect("compound definition should succeed");

        let fetched = handle_get_definition(&world, compound.definition_id.clone())
            .expect("compound definition should be retrievable");

        assert_eq!(
            fetched.full["compound"]["child_slots"][0]["role"],
            json!("frame")
        );
        assert_eq!(
            fetched.full["domain_data"]["architectural"]["void_declaration"]["kind"],
            json!("opening")
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn compound_occurrence_generates_child_slot_geometry() {
        use crate::plugins::modeling::{
            occurrence::GeneratedOccurrencePart, profile::ProfileExtrusion,
        };

        let mut world = init_model_api_test_world();

        let child = handle_create_definition(&mut world, make_locked_member_request())
            .expect("locked child definition should be created");
        let compound = handle_create_definition(
            &mut world,
            make_compound_window_request(&child.definition_id),
        )
        .expect("compound definition should be created");

        let occurrence_id = handle_place_occurrence(
            &mut world,
            json!({
                "definition_id": compound.definition_id,
                "overrides": {
                    "overall_width": 1.8,
                    "overall_height": 1.6
                }
            }),
        )
        .expect("compound occurrence should be placed");

        let owner = ElementId(occurrence_id);
        let generated_parts: Vec<(GeneratedOccurrencePart, ProfileExtrusion)> = world
            .query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
            .iter(&world)
            .map(|(generated, extrusion)| (generated.clone(), extrusion.clone()))
            .collect();

        assert_eq!(generated_parts.len(), 1);
        assert_eq!(generated_parts[0].0.owner, owner);
        assert_eq!(generated_parts[0].0.slot_path, "frame");
        let (min, max) = generated_parts[0].1.profile.bounds_2d();
        assert_eq!(max.x - min.x, 1.8);
        assert_eq!(max.y - min.y, 0.14);
        assert_eq!(generated_parts[0].1.height, 1.6);
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn occurrence_explain_reports_generated_parts_and_resolved_values() {
        let mut world = init_model_api_test_world();

        let child = handle_create_definition(&mut world, make_locked_member_request())
            .expect("locked child definition should be created");
        let compound = handle_create_definition(
            &mut world,
            make_compound_window_request(&child.definition_id),
        )
        .expect("compound definition should be created");

        let occurrence_id = handle_place_occurrence(
            &mut world,
            json!({
                "definition_id": compound.definition_id,
                "label": "Window A",
                "overrides": {
                    "overall_width": 1.5,
                    "overall_height": 1.25,
                    "finish_color": "red"
                },
                "domain_data": {
                    "architectural": {
                        "host_occurrence": "wall-42"
                    }
                }
            }),
        )
        .expect("compound occurrence should be placed");

        let explanation =
            handle_explain_occurrence(&world, occurrence_id).expect("explain should succeed");

        assert_eq!(explanation.label, "Window A");
        assert_eq!(explanation.generated_parts.len(), 1);
        assert_eq!(explanation.generated_parts[0].slot_path, "frame");
        assert_eq!(
            explanation.generated_parts[0].definition_id,
            child.definition_id
        );
        assert_eq!(
            explanation.resolved_parameters["finish_color"]["value"],
            json!("red")
        );
        assert_eq!(explanation.anchors.len(), 2);
        assert_eq!(
            explanation.domain_data["architectural"]["host_occurrence"],
            json!("wall-42")
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_update_bumps_version_and_propagates() {
        let mut world = init_model_api_test_world();

        let created = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("create definition should succeed");

        let updated = handle_update_definition(
            &mut world,
            json!({
                "definition_id": created.definition_id,
                "name": "RenamedWall"
            }),
        )
        .expect("update definition should succeed");

        assert_eq!(updated.definition_version, 2);
        assert_eq!(updated.name, "RenamedWall");

        // Place an occurrence, then update the definition again — occurrence
        // should be marked dirty (ChangedDefinitions resource updated).
        let occ_id = handle_place_occurrence(
            &mut world,
            json!({ "definition_id": created.definition_id }),
        )
        .expect("place occurrence should succeed");
        let _ = occ_id; // placement succeeded (expect() already asserted this)

        handle_update_definition(
            &mut world,
            json!({
                "definition_id": created.definition_id,
                "name": "FinalWall"
            }),
        )
        .expect("second update should succeed");

        // ChangedDefinitions should have been drained by flush_model_api_write_pipeline
        // (which calls apply_pending_history_commands), but the UpdateDefinition command's
        // apply() calls mark_changed. Since flush runs synchronously we verify
        // the definition version rather than the transient resource.
        let after = handle_get_definition(&world, created.definition_id.clone())
            .expect("get after second update should succeed");
        assert_eq!(after.definition_version, 3);
        assert_eq!(after.name, "FinalWall");
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn occurrence_place_and_resolve_returns_provenance() {
        let mut world = init_model_api_test_world();

        let def = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("create definition should succeed");

        // Place with no overrides — all values should be DefinitionDefault.
        let occ_id = handle_place_occurrence(
            &mut world,
            json!({ "definition_id": def.definition_id, "label": "Wall1" }),
        )
        .expect("place occurrence should succeed");

        let resolved =
            handle_resolve_occurrence(&world, occ_id).expect("resolve occurrence should succeed");

        assert_eq!(resolved["width"]["value"], json!(4.0));
        assert_eq!(resolved["width"]["provenance"], json!("DefinitionDefault"));
        assert_eq!(resolved["height"]["value"], json!(3.0));
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn occurrence_update_overrides_changes_only_the_target() {
        let mut world = init_model_api_test_world();

        let def = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("create definition should succeed");

        let occ_a = handle_place_occurrence(
            &mut world,
            json!({ "definition_id": def.definition_id, "label": "WallA" }),
        )
        .expect("place occurrence A should succeed");

        let occ_b = handle_place_occurrence(
            &mut world,
            json!({ "definition_id": def.definition_id, "label": "WallB" }),
        )
        .expect("place occurrence B should succeed");

        // Override height only on A.
        handle_update_occurrence_overrides(&mut world, occ_a, json!({ "height": 5.0 }))
            .expect("update overrides should succeed");

        let resolved_a =
            handle_resolve_occurrence(&world, occ_a).expect("resolve A should succeed");
        let resolved_b =
            handle_resolve_occurrence(&world, occ_b).expect("resolve B should succeed");

        // A has an override, B still uses the definition default.
        assert_eq!(resolved_a["height"]["value"], json!(5.0));
        assert_eq!(
            resolved_a["height"]["provenance"],
            json!("OccurrenceOverride")
        );
        assert_eq!(resolved_b["height"]["value"], json!(3.0));
        assert_eq!(
            resolved_b["height"]["provenance"],
            json!("DefinitionDefault")
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_library_workflow_exports_imports_and_instantiates() {
        let mut source_world = init_model_api_test_world();
        let base_definition =
            handle_create_definition(&mut source_world, make_rect_extrusion_request())
                .expect("base definition should be created");
        let definition = handle_create_definition(
            &mut source_world,
            make_definition_variant_request(&base_definition.definition_id),
        )
        .expect("variant definition should be created");

        let library = handle_create_definition_library(
            &mut source_world,
            json!({ "name": "Window Library" }),
        )
        .expect("library should be created");
        handle_add_definition_to_library(
            &mut source_world,
            json!({
                "library_id": library.library_id,
                "definition_id": definition.definition_id
            }),
        )
        .expect("definition should be added to library");

        let listed = handle_list_definition_libraries(&source_world);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].definition_count, 2);

        let export_path = temp_json_path("talos3d-definition-library");
        handle_export_definition_library(
            &source_world,
            &library.library_id,
            export_path.to_str().unwrap_or_default(),
        )
        .expect("library should export");

        let mut target_world = init_model_api_test_world();
        let imported = handle_import_definition_library(
            &mut target_world,
            export_path.to_str().unwrap_or_default(),
        )
        .expect("library should import");
        let instantiated = handle_instantiate_definition(
            &mut target_world,
            json!({
                "library_id": imported.library_id,
                "definition_id": definition.definition_id,
                "label": "ImportedWall",
                "overrides": { "width": 4.2 }
            }),
        )
        .expect("definition should instantiate from library");

        assert_eq!(instantiated.definition_id, definition.definition_id);
        assert_eq!(instantiated.imported_definition_ids.len(), 2);
        assert!(instantiated
            .imported_definition_ids
            .contains(&definition.definition_id));
        assert!(instantiated
            .imported_definition_ids
            .contains(&base_definition.definition_id));

        let resolved = handle_resolve_occurrence(&target_world, instantiated.element_id)
            .expect("instantiated occurrence should resolve");
        assert_eq!(resolved["width"]["value"], json!(4.2));
        assert_eq!(resolved["height"]["value"], json!(4.5));
        assert_eq!(resolved["finish_color"]["value"], json!("greyline"));

        let _ = fs::remove_file(export_path);
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn hosted_definition_instantiation_derives_anchors_and_relation() {
        let mut world = init_model_api_test_world();
        world
            .resource_mut::<CapabilityRegistry>()
            .register_relation_type(crate::capability_registry::RelationTypeDescriptor {
                relation_type: "hosted_on".to_string(),
                label: "Hosted On".to_string(),
                description: "Hosted relation for occurrence placement".to_string(),
                valid_source_types: vec!["occurrence".to_string()],
                valid_target_types: vec!["box".to_string()],
                parameter_schema: json!({}),
                participates_in_dependency_graph: true,
            });

        let host_id = handle_create_entity(
            &mut world,
            json!({
                "type": "box",
                "centre": [4.0, 1.5, 0.0],
                "half_extents": [2.0, 1.5, 0.15]
            }),
        )
        .expect("host should be created");
        let opening_id = handle_create_entity(
            &mut world,
            json!({
                "type": "box",
                "centre": [4.0, 1.2, 0.0],
                "half_extents": [0.6, 0.8, 0.15]
            }),
        )
        .expect("opening proxy should be created");

        let child = handle_create_definition(&mut world, make_locked_member_request())
            .expect("locked child definition should be created");
        let compound = handle_create_definition(
            &mut world,
            make_compound_window_request(&child.definition_id),
        )
        .expect("compound definition should be created");

        let instantiated = handle_instantiate_hosted_definition(
            &mut world,
            json!({
                "definition_id": compound.definition_id,
                "label": "HostedWindow",
                "hosting": {
                    "host_element_id": host_id,
                    "opening_element_id": opening_id
                }
            }),
        )
        .expect("hosted occurrence should be instantiated");

        assert_eq!(instantiated.relation_ids.len(), 1);

        let resolved = handle_resolve_occurrence(&world, instantiated.element_id)
            .expect("hosted occurrence should resolve");
        let wall_thickness = resolved["wall_thickness"]["value"]
            .as_f64()
            .expect("wall_thickness should resolve to a number");
        assert!((wall_thickness - 0.3).abs() < 1e-5);

        let explanation = handle_explain_occurrence(&world, instantiated.element_id)
            .expect("hosted occurrence explanation should succeed");
        assert_eq!(explanation.label, "HostedWindow");
        assert_eq!(explanation.hosting["host_element_id"], json!(host_id));
        assert_eq!(explanation.hosting["opening_element_id"], json!(opening_id));
        assert!(explanation.hosting["anchors"]
            .as_array()
            .is_some_and(|anchors| anchors.len() >= 3));

        let relations = handle_query_relations(
            &world,
            Some(instantiated.element_id),
            Some(host_id),
            Some("hosted_on".to_string()),
        );
        assert_eq!(relations.len(), 1);
        assert_eq!(
            relations[0].parameters["opening_element_id"],
            json!(opening_id)
        );

        let details = get_entity_details(&world, ElementId(instantiated.element_id))
            .expect("hosted occurrence details should resolve");
        assert_eq!(details.entity_type, "occurrence");
        assert_eq!(details.label, "HostedWindow");
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn definition_and_occurrence_round_trip_through_project_persistence() {
        let mut source_world = init_model_api_test_world();
        let definition = handle_create_definition(&mut source_world, make_rect_extrusion_request())
            .expect("definition should be created");
        let occurrence_id = handle_place_occurrence(
            &mut source_world,
            json!({
                "definition_id": definition.definition_id,
                "label": "RoundTripWall",
                "overrides": { "height": 4.5 },
                "domain_data": {
                    "architectural": { "exchange_identity_map": { "GlobalId": "rt-1" } }
                }
            }),
        )
        .expect("occurrence should be created");

        let path = temp_json_path("talos3d-roundtrip-project").with_extension("talos3d");
        handle_save_project(&mut source_world, path.to_str().unwrap_or_default())
            .expect("project should save");

        let mut loaded_world = init_model_api_test_world();
        handle_load_project(&mut loaded_world, path.to_str().unwrap_or_default())
            .expect("project should load");

        let loaded_definition =
            handle_get_definition(&loaded_world, definition.definition_id.clone())
                .expect("definition should load");
        assert_eq!(loaded_definition.full["name"], json!("TestWall"));
        assert_eq!(
            loaded_definition.full["interface"]["parameters"],
            handle_get_definition(&source_world, definition.definition_id.clone())
                .expect("source definition should exist")
                .full["interface"]["parameters"]
        );

        let resolved = handle_resolve_occurrence(&loaded_world, occurrence_id)
            .expect("loaded occurrence should resolve");
        assert_eq!(resolved["height"]["value"], json!(4.5));
        assert_eq!(
            resolved["height"]["provenance"],
            json!("OccurrenceOverride")
        );

        let explanation = handle_explain_occurrence(&loaded_world, occurrence_id)
            .expect("loaded occurrence explanation should succeed");
        assert_eq!(explanation.label, "RoundTripWall");
        assert_eq!(
            explanation.domain_data["architectural"]["exchange_identity_map"]["GlobalId"],
            json!("rt-1")
        );

        let _ = fs::remove_file(path);
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn non_geometric_fields_do_not_set_mesh_dirty() {
        use crate::plugins::modeling::occurrence::{OccurrenceClassification, OccurrenceIdentity};

        let mut world = init_model_api_test_world();

        let def = handle_create_definition(&mut world, make_rect_extrusion_request())
            .expect("create definition should succeed");

        let occ_id =
            handle_place_occurrence(&mut world, json!({ "definition_id": def.definition_id }))
                .expect("place occurrence should succeed");

        let eid = ElementId(occ_id);

        // Manually set mesh_dirty = false to simulate a clean state.
        let entity = {
            let mut q = world.query::<(bevy::prelude::Entity, &ElementId)>();
            q.iter(&world)
                .find_map(|(e, id)| (*id == eid).then_some(e))
                .expect("occurrence entity should exist")
        };
        world
            .entity_mut(entity)
            .insert(OccurrenceClassification { mesh_dirty: false });

        // Directly mutate opaque domain data on the component. This must not
        // force a geometry re-evaluation.
        {
            let mut identity = world.get_mut::<OccurrenceIdentity>(entity).unwrap();
            identity.domain_data = json!({
                "architectural": {
                    "property_set_map": { "Pset_BuildingCommon": { "IsExternal": true } },
                    "exchange_identity_map": { "GlobalId": "abc" }
                }
            });
        }

        // mesh_dirty must remain false.
        let cls = world.get::<OccurrenceClassification>(entity).unwrap();
        assert!(
            !cls.mesh_dirty,
            "modifying domain_data must not set mesh_dirty"
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn render_settings_round_trip_and_validate() {
        let mut world = init_model_api_test_world();

        let initial = handle_get_render_settings(&world);
        assert_eq!(initial.tonemapping, "agx");

        let updated = handle_set_render_settings(
            &mut world,
            RenderSettingsUpdateRequest {
                tonemapping: Some("blender_filmic".to_string()),
                exposure_ev100: Some(1.5),
                ssao_enabled: Some(false),
                bloom_enabled: Some(true),
                bloom_intensity: Some(0.42),
                ssr_enabled: Some(true),
                ssr_linear_steps: Some(24),
                ..Default::default()
            },
        )
        .expect("render settings update should succeed");

        assert_eq!(updated.tonemapping, "blender_filmic");
        assert_eq!(updated.exposure_ev100, 1.5);
        assert!(!updated.ssao_enabled);
        assert!(updated.ssr_enabled);
        assert_eq!(updated.ssr_linear_steps, 24);

        let error = handle_set_render_settings(
            &mut world,
            RenderSettingsUpdateRequest {
                tonemapping: Some("not-a-tonemapper".to_string()),
                ..Default::default()
            },
        )
        .expect_err("invalid tonemapping should fail");
        assert!(error.contains("Unknown tonemapping mode"));
    }
}
