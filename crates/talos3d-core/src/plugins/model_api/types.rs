use super::*;

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
    pub semantic: Option<EntitySemanticDetails>,
    pub properties: Vec<EntityPropertyDetails>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntitySemanticDetails {
    pub element_class: Option<String>,
    pub semantic_roles: Vec<String>,
    pub refinement_state: Option<String>,
    pub parameters: serde_json::Value,
    pub unresolved_decisions: Vec<crate::plugins::refinement::UnresolvedDecisionRecord>,
    pub source_refs: Vec<crate::plugins::refinement::SemanticSourceRef>,
    pub authoring_rationale: Option<String>,
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
    /// Length unit of the world/scene coordinate system. Geometry primitives
    /// (`create_box` center/size, occurrence offsets, frame/bbox values) are in
    /// this unit (metres). Architectural recipe/parametric drivers named `*_mm`
    /// are millimetres and converted internally. Stated here so agents never
    /// have to calibrate by trial.
    pub world_length_unit: String,
    /// One-line units rule for authoring (geometry in metres; `*_mm` drivers in mm).
    pub units_note: String,
}

#[cfg(feature = "model-api")]
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
            world_length_unit: "m".to_string(),
            units_note: "World/scene geometry is in METRES (create_box, occurrence offsets, frame bboxes). Recipe/parametric drivers named *_mm are MILLIMETRES, converted internally.".to_string(),
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
    pub orthographic_scale: f32,
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
    pub orthographic_scale: Option<f32>,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    /// `"perspective"` or `"orthographic"`.
    pub projection: Option<String>,
    pub focal_length_mm: Option<f32>,
}

/// Live camera state for the active orbit camera.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CameraStateInfo {
    pub focus: [f32; 3],
    pub radius: f32,
    pub orthographic_scale: f32,
    pub yaw: f32,
    pub pitch: f32,
    /// `"perspective"` or `"orthographic"`.
    pub projection: String,
    pub focal_length_mm: f32,
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
    pub asset_id: String,
    pub name: String,
    pub spec_ref: Option<String>,
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
    #[cfg(feature = "model-api")]
    pub(super) fn from_def(def: &MaterialDef, texture_registry: &TextureRegistry) -> Self {
        fn tex_data(t: &Option<TextureRef>, texture_registry: &TextureRegistry) -> Option<String> {
            match t {
                Some(TextureRef::TextureAsset { id }) => {
                    texture_registry.get(id).map(|asset| match &asset.payload {
                        crate::plugins::materials::TexturePayload::Embedded { data, .. } => {
                            data.clone()
                        }
                        crate::plugins::materials::TexturePayload::AssetPath(path) => path.clone(),
                    })
                }
                Some(TextureRef::Embedded { data, .. }) => Some(data.clone()),
                Some(TextureRef::AssetPath { path: p }) => Some(p.clone()),
                None => None,
            }
        }
        fn tex_mime(t: &Option<TextureRef>, texture_registry: &TextureRegistry) -> Option<String> {
            match t {
                Some(TextureRef::TextureAsset { id }) => {
                    texture_registry
                        .get(id)
                        .and_then(|asset| match &asset.payload {
                            crate::plugins::materials::TexturePayload::Embedded {
                                mime, ..
                            } => Some(mime.clone()),
                            crate::plugins::materials::TexturePayload::AssetPath(_) => None,
                        })
                }
                Some(TextureRef::Embedded { mime, .. }) => Some(mime.clone()),
                Some(TextureRef::AssetPath { .. }) => None,
                None => None,
            }
        }

        Self {
            id: def.id.clone(),
            asset_id: def.asset_id().to_string(),
            name: def.name.clone(),
            spec_ref: def.spec_ref.as_ref().map(|id| id.as_str().to_string()),
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
            base_color_texture: tex_data(&def.base_color_texture, texture_registry),
            base_color_texture_mime: tex_mime(&def.base_color_texture, texture_registry),
            normal_map_texture: tex_data(&def.normal_map_texture, texture_registry),
            normal_map_texture_mime: tex_mime(&def.normal_map_texture, texture_registry),
            metallic_roughness_texture: tex_data(&def.metallic_roughness_texture, texture_registry),
            metallic_roughness_texture_mime: tex_mime(
                &def.metallic_roughness_texture,
                texture_registry,
            ),
            emissive_texture: tex_data(&def.emissive_texture, texture_registry),
            emissive_texture_mime: tex_mime(&def.emissive_texture, texture_registry),
            occlusion_texture: tex_data(&def.occlusion_texture, texture_registry),
            occlusion_texture_mime: tex_mime(&def.occlusion_texture, texture_registry),
        }
    }
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateMaterialRequest {
    pub name: String,
    #[serde(default)]
    pub spec_ref: Option<String>,
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

fn default_true() -> bool {
    true
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
pub struct AssignMaterialRequest {
    pub element_ids: Vec<u64>,
    #[serde(default)]
    pub material_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_color: Option<[f32; 4]>,
    #[serde(default)]
    pub perceptual_roughness: Option<f32>,
    #[serde(default)]
    pub metallic: Option<f32>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssignMaterialResponse {
    pub material_id: String,
    pub created_material: bool,
    pub assignments: Vec<EntityMaterialAssignmentInfo>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetMaterialAssignmentRequest {
    pub element_id: u64,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMaterialAssignmentRequest {
    pub element_ids: Vec<u64>,
    pub assignment: MaterialAssignment,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityMaterialAssignmentInfo {
    pub element_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<MaterialAssignment>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BimMaterialLayerInput {
    pub material: String,
    pub thickness_m: f64,
    pub function: Option<String>,
    pub is_ventilated: Option<bool>,
    pub label: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BimMaterialConstituentInput {
    pub material: String,
    pub fraction: f64,
    pub label: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BimMaterialAssignLayeredRequest {
    pub definition_id: Option<String>,
    pub element_id: Option<u64>,
    pub layers: Vec<BimMaterialLayerInput>,
    pub total_thickness_param: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BimMaterialAssignConstituentsRequest {
    pub definition_id: Option<String>,
    pub element_id: Option<u64>,
    pub constituents: Vec<BimMaterialConstituentInput>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BimMaterialGetEffectiveRequest {
    pub definition_id: Option<String>,
    pub element_id: Option<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantityProvenanceInput {
    pub kind: String,
    pub parameter: Option<String>,
    pub node: Option<String>,
    pub source: Option<String>,
    pub rationale: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantitySetRequest {
    pub element_id: u64,
    pub field: String,
    pub value: Value,
    pub material: Option<String>,
    pub provenance: QuantityProvenanceInput,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantityGetRequest {
    pub element_id: u64,
    pub field: Option<String>,
    pub material: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantityListProvenanceRequest {
    pub element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantityCheckInvariantsRequest {
    pub element_id: u64,
    pub tolerance: Option<f64>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetMaterialSpecRequest {
    pub asset_id: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateMaterialSpecRequest {
    pub asset_id: String,
    pub body: MaterialSpecBody,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SaveMaterialSpecRequest {
    pub asset_id: String,
    pub scope: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteMaterialSpecRequest {
    pub asset_id: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AmbientLightInfo {
    pub color: [f32; 3],
    pub brightness: f32,
    pub affects_lightmapped_meshes: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SceneLightInfo {
    pub element_id: u64,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub color: [f32; 3],
    pub intensity: f32,
    pub shadows_enabled: bool,
    pub position: [f32; 3],
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub range: f32,
    pub radius: f32,
    pub inner_angle_deg: f32,
    pub outer_angle_deg: f32,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LightingSceneInfo {
    pub ambient: AmbientLightInfo,
    pub lights: Vec<SceneLightInfo>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateLightRequest {
    pub kind: String,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub color: Option<[f32; 3]>,
    pub intensity: Option<f32>,
    pub shadows_enabled: Option<bool>,
    pub position: Option<[f32; 3]>,
    pub yaw_deg: Option<f32>,
    pub pitch_deg: Option<f32>,
    pub range: Option<f32>,
    pub radius: Option<f32>,
    pub inner_angle_deg: Option<f32>,
    pub outer_angle_deg: Option<f32>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateLightRequest {
    pub element_id: u64,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub enabled: Option<bool>,
    pub color: Option<[f32; 3]>,
    pub intensity: Option<f32>,
    pub shadows_enabled: Option<bool>,
    pub position: Option<[f32; 3]>,
    pub yaw_deg: Option<f32>,
    pub pitch_deg: Option<f32>,
    pub range: Option<f32>,
    pub radius: Option<f32>,
    pub inner_angle_deg: Option<f32>,
    pub outer_angle_deg: Option<f32>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteLightRequest {
    pub element_id: u64,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaceGuideLineRequest {
    pub anchor: [f32; 3],
    pub direction: Option<[f32; 3]>,
    pub through: Option<[f32; 3]>,
    pub reference_direction: Option<[f32; 3]>,
    pub angle_degrees: Option<f32>,
    pub plane_normal: Option<[f32; 3]>,
    pub finite_length: Option<f32>,
    pub visible: Option<bool>,
    pub label: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaceDimensionLineRequest {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub line_point: Option<[f32; 3]>,
    pub offset: Option<f32>,
    pub extension: Option<f32>,
    pub visible: Option<bool>,
    pub label: Option<String>,
    pub display_unit: Option<String>,
    pub precision: Option<u8>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateBoxRequest {
    #[serde(default, alias = "centre")]
    pub center: Option<[f32; 3]>,
    #[serde(default)]
    pub half_extents: Option<[f32; 3]>,
    #[serde(default)]
    pub size: Option<[f32; 3]>,
    /// Optional quaternion as `[x, y, z, w]`.
    #[serde(default)]
    pub rotation: Option<[f32; 4]>,
    /// Optional semantic annotation attached after geometric creation.
    #[serde(default)]
    pub semantic: Option<SemanticEntityAnnotationRequest>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SemanticEntityAnnotationRequest {
    #[serde(default)]
    pub element_class: Option<String>,
    #[serde(default)]
    pub refinement_state: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub unresolved_decisions: Vec<crate::plugins::refinement::UnresolvedDecisionRecord>,
    #[serde(default)]
    pub source_refs: Vec<crate::plugins::refinement::SemanticSourceRef>,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaceDimensionBetweenHandlesRequest {
    pub start_element_id: u64,
    pub start_handle_id: String,
    pub end_element_id: u64,
    pub end_handle_id: String,
    pub line_point: Option<[f32; 3]>,
    pub offset: Option<f32>,
    pub extension: Option<f32>,
    pub visible: Option<bool>,
    pub label: Option<String>,
    pub display_unit: Option<String>,
    pub precision: Option<u8>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BooleanOperationRequest {
    /// Element ID of the base solid (the body that remains / is modified).
    pub base: u64,
    /// Element ID of the tool solid (the body that cuts, adds, or intersects).
    pub tool: u64,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AmbientLightUpdateRequest {
    pub color: Option<[f32; 3]>,
    pub brightness: Option<f32>,
    pub affects_lightmapped_meshes: Option<bool>,
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
    pub wireframe_overlay_enabled: bool,
    pub contour_overlay_enabled: bool,
    pub visible_edge_overlay_enabled: bool,
    pub grid_enabled: bool,
    pub background_rgb: [f32; 3],
    pub paper_fill_enabled: bool,
}

impl RenderSettingsInfo {
    #[cfg(feature = "model-api")]
    pub(super) fn from_settings(settings: &RenderSettings) -> Self {
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
            wireframe_overlay_enabled: settings.wireframe_overlay_enabled,
            contour_overlay_enabled: settings.contour_overlay_enabled,
            visible_edge_overlay_enabled: settings.visible_edge_overlay_enabled,
            grid_enabled: settings.grid_enabled,
            background_rgb: settings.background_rgb,
            paper_fill_enabled: settings.paper_fill_enabled,
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
    pub wireframe_overlay_enabled: Option<bool>,
    pub contour_overlay_enabled: Option<bool>,
    pub visible_edge_overlay_enabled: Option<bool>,
    pub grid_enabled: Option<bool>,
    pub background_rgb: Option<[f32; 3]>,
    pub paper_fill_enabled: Option<bool>,
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

/// Optional parameters for the `definition.list` MCP tool.
///
/// All fields default to `false` / `None` so the tool can be called with no
/// arguments (matching the previous no-argument signature).
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DefinitionListParams {
    /// When `true`, `InternalPart` definitions (implementation parts such as
    /// truss members and window parts) are included in the result. Defaults
    /// to `false` so the default listing only shows user-facing public families.
    #[serde(default)]
    pub include_internal: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepresentationDeclareRequest {
    pub definition_id: String,
    pub kind: String,
    pub role: Option<String>,
    pub lod: Option<String>,
    pub update_policy: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepresentationSetLodRequest {
    pub definition_id: String,
    pub kind: String,
    pub role: Option<String>,
    pub lod: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepresentationSetUpdatePolicyRequest {
    pub definition_id: String,
    pub kind: String,
    pub role: Option<String>,
    pub update_policy: String,
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
pub struct MakeOccurrenceUniqueResult {
    pub element_id: u64,
    pub previous_definition_id: String,
    pub new_definition_id: String,
    pub copied_definition_ids: Vec<String>,
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
    #[serde(default)]
    pub collection_slots: Vec<DefinitionCollectionSlotResult>,
    pub derived_parameter_count: usize,
    pub constraint_count: usize,
    pub anchor_count: usize,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionCollectionSlotResult {
    pub slot_id: String,
    pub count: Value,
    pub layout: Value,
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
    #[serde(default)]
    pub resolved_collection_slots: Vec<Value>,
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
pub struct AssemblyPatternLayerInfo {
    pub layer_id: String,
    pub label: String,
    pub role: String,
    pub material_hint: Option<String>,
    pub optional: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssemblyPatternRelationRuleInfo {
    pub relation_type: String,
    pub source_layer_id: String,
    pub target_layer_id: String,
    pub required: bool,
    pub rationale: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssemblyPatternInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub target_types: Vec<String>,
    pub axis: String,
    pub layers: Vec<AssemblyPatternLayerInfo>,
    pub relation_rules: Vec<AssemblyPatternRelationRuleInfo>,
    pub root_layer_ids: Vec<String>,
    pub requires_support_path: bool,
    pub tags: Vec<String>,
    pub parameter_schema: serde_json::Value,
    #[serde(default)]
    pub is_session_draft: bool,
    pub status: Option<String>,
    pub consultable: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurationAssetInfo {
    pub asset_id: String,
    pub kind: String,
    pub scope: String,
    pub trust: String,
    pub validation: String,
    pub residency: crate::plugins::knowledge_assets::KnowledgeResidency,
    pub evidence_ref_count: usize,
    pub evidence_slot_count: usize,
    pub runtime_claim_count: usize,
    pub evidence_backed_runtime_claim_count: usize,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssemblyPatternDraftInfo {
    pub id: String,
    pub curation: CurationAssetInfo,
    pub label: String,
    pub description: String,
    pub target_types: Vec<String>,
    pub axis: String,
    pub layers: Vec<AssemblyPatternLayerInfo>,
    pub relation_rules: Vec<AssemblyPatternRelationRuleInfo>,
    pub root_layer_ids: Vec<String>,
    pub requires_support_path: bool,
    pub tags: Vec<String>,
    pub parameter_schema: serde_json::Value,
    pub jurisdiction: Option<String>,
    pub gap_id: Option<String>,
    pub source_passage_refs: Vec<String>,
    #[serde(default)]
    pub evidence_slots: Vec<crate::plugins::knowledge_assets::EvidenceSlot>,
    #[serde(default)]
    pub runtime_claims: Vec<crate::plugins::knowledge_assets::RuntimeCapabilityClaim>,
    pub acquisition_context: serde_json::Value,
    pub notes: Vec<String>,
    pub status: String,
    pub consultable: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VocabularyInfo {
    pub assembly_types: Vec<crate::capability_registry::AssemblyTypeDescriptor>,
    pub assembly_patterns: Vec<AssemblyPatternInfo>,
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
