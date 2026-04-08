/// Material system for Talos3D.
///
/// Provides a `MaterialRegistry` (project-level resource), a `MaterialAssignment`
/// ECS component that links any authored entity to a named material, and systems
/// that keep Bevy's `StandardMaterial` handles in sync with the registry.
use std::collections::BTreeMap;

use base64::prelude::*;
use bevy::{
    asset::RenderAssetUsages,
    image::{CompressedImageFormats, ImageSampler, ImageType},
    math::Affine2,
    prelude::*,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Alpha mode ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MaterialAlphaMode {
    #[default]
    Opaque,
    Mask,
    Blend,
    Premultiplied,
    Add,
}

impl MaterialAlphaMode {
    pub fn to_bevy(&self, cutoff: f32) -> AlphaMode {
        match self {
            Self::Opaque => AlphaMode::Opaque,
            Self::Mask => AlphaMode::Mask(cutoff),
            Self::Blend => AlphaMode::Blend,
            Self::Premultiplied => AlphaMode::Premultiplied,
            Self::Add => AlphaMode::Add,
        }
    }
}

// ─── TextureRef ──────────────────────────────────────────────────────────────

/// A reference to a texture, either embedded in the project file or pointing to
/// a bundled app asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextureRef {
    /// User-uploaded texture embedded as base64 in the project file.
    /// `mime` is e.g. `"image/png"`, `"image/jpeg"`.
    Embedded { data: String, mime: String },
    /// Path to a bundled app asset, resolved via Bevy's `AssetServer`.
    AssetPath(String),
}

impl TextureRef {
    /// Load as a Bevy `Image` handle.  Embedded textures are decoded and inserted
    /// directly into `images`; asset paths go through the asset server.
    pub fn load(
        &self,
        asset_server: &AssetServer,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        match self {
            TextureRef::AssetPath(path) => Some(asset_server.load(path.clone())),
            TextureRef::Embedded { data, mime } => {
                let bytes = BASE64_STANDARD.decode(data).ok()?;
                let image_type = match mime.as_str() {
                    "image/png" => ImageType::MimeType("image/png"),
                    "image/jpeg" | "image/jpg" => ImageType::MimeType("image/jpeg"),
                    "image/webp" => ImageType::MimeType("image/webp"),
                    _ => ImageType::MimeType("image/png"),
                };
                let image = Image::from_buffer(
                    &bytes,
                    image_type,
                    CompressedImageFormats::NONE,
                    true,
                    ImageSampler::default(),
                    RenderAssetUsages::RENDER_WORLD,
                )
                .ok()?;
                Some(images.add(image))
            }
        }
    }

    /// Returns a short display label (filename or `"<embedded>"`).
    pub fn label(&self) -> &str {
        match self {
            TextureRef::AssetPath(p) => p.split('/').last().unwrap_or(p),
            TextureRef::Embedded { .. } => "<embedded>",
        }
    }
}

// ─── MaterialDef ─────────────────────────────────────────────────────────────

/// The data definition of a material.  Serialisable, stored in `MaterialRegistry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialDef {
    /// Stable unique identifier (UUID-like string).
    pub id: String,
    /// Human-readable name shown in the browser.
    pub name: String,

    // --- PBR base properties ---
    /// Base colour as linear RGBA.
    pub base_color: [f32; 4],
    pub perceptual_roughness: f32,
    pub metallic: f32,
    /// Specular reflectance at normal incidence (0–1).
    pub reflectance: f32,
    /// Emissive colour (RGB, HDR values > 1 allowed).
    pub emissive: [f32; 3],
    pub emissive_exposure_weight: f32,

    // --- Transparency ---
    pub alpha_mode: MaterialAlphaMode,
    /// Threshold used when `alpha_mode == Mask`.
    pub alpha_cutoff: f32,

    // --- Textures ---
    pub base_color_texture: Option<TextureRef>,
    pub normal_map_texture: Option<TextureRef>,
    pub metallic_roughness_texture: Option<TextureRef>,
    pub emissive_texture: Option<TextureRef>,
    pub occlusion_texture: Option<TextureRef>,

    // --- UV tiling / transform ---
    /// Multiply UV coords by this scale (creates tiling).  `[1,1]` = no tiling.
    pub uv_scale: [f32; 2],
    /// Additional UV rotation in radians.
    pub uv_rotation: f32,

    // --- Double-sided rendering ---
    pub double_sided: bool,
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            id: uuid_v4(),
            name: "New Material".to_string(),
            base_color: [0.78, 0.82, 0.88, 1.0],
            perceptual_roughness: 0.85,
            metallic: 0.0,
            reflectance: 0.5,
            emissive: [0.0, 0.0, 0.0],
            emissive_exposure_weight: 1.0,
            alpha_mode: MaterialAlphaMode::Opaque,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            normal_map_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            occlusion_texture: None,
            uv_scale: [1.0, 1.0],
            uv_rotation: 0.0,
            double_sided: false,
        }
    }
}

impl MaterialDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Build a Bevy `StandardMaterial` from this definition.
    ///
    /// `images` is required so that embedded textures can be decoded and
    /// inserted directly into the asset store without going through the file
    /// system.
    pub fn to_standard_material(
        &self,
        asset_server: &AssetServer,
        images: &mut Assets<Image>,
    ) -> StandardMaterial {
        let [r, g, b, a] = self.base_color;
        let [er, eg, eb] = self.emissive;

        let uv_transform = if self.uv_scale != [1.0f32, 1.0f32] || self.uv_rotation != 0.0 {
            Affine2::from_scale_angle_translation(
                Vec2::from(self.uv_scale),
                self.uv_rotation,
                Vec2::ZERO,
            )
        } else {
            Affine2::IDENTITY
        };

        StandardMaterial {
            base_color: Color::srgba(r, g, b, a),
            base_color_texture: self
                .base_color_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images)),
            emissive: LinearRgba::new(er, eg, eb, self.emissive_exposure_weight),
            emissive_texture: self
                .emissive_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images)),
            perceptual_roughness: self.perceptual_roughness,
            metallic: self.metallic,
            metallic_roughness_texture: self
                .metallic_roughness_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images)),
            reflectance: self.reflectance,
            normal_map_texture: self
                .normal_map_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images)),
            occlusion_texture: self
                .occlusion_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images)),
            alpha_mode: self.alpha_mode.to_bevy(self.alpha_cutoff),
            double_sided: self.double_sided,
            cull_mode: if self.double_sided {
                None
            } else {
                Some(bevy::render::render_resource::Face::Back)
            },
            uv_transform,
            ..default()
        }
    }

    /// Returns a compact summary string for UI display.
    pub fn summary(&self) -> String {
        let alpha = match self.alpha_mode {
            MaterialAlphaMode::Opaque => String::new(),
            MaterialAlphaMode::Blend => format!(", α={:.0}%", self.base_color[3] * 100.0),
            _ => format!(", {:?}", self.alpha_mode),
        };
        format!(
            "R={:.2} M={:.2}{}",
            self.perceptual_roughness, self.metallic, alpha
        )
    }
}

// ─── MaterialRegistry ─────────────────────────────────────────────────────────

/// Project-level catalogue of material definitions.  Serialisable so it is
/// saved with the project file.
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct MaterialRegistry {
    /// Ordered by insertion time (BTreeMap gives stable JSON output).
    materials: BTreeMap<String, MaterialDef>,
    /// Iteration order (insertion order).
    order: Vec<String>,
}

impl Default for MaterialRegistry {
    fn default() -> Self {
        Self {
            materials: BTreeMap::new(),
            order: Vec::new(),
        }
    }
}

impl MaterialRegistry {
    // --- Read access ---

    pub fn get(&self, id: &str) -> Option<&MaterialDef> {
        self.materials.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut MaterialDef> {
        self.materials.get_mut(id)
    }

    /// All materials in insertion order.
    pub fn all(&self) -> impl Iterator<Item = &MaterialDef> {
        self.order.iter().filter_map(|id| self.materials.get(id))
    }

    pub fn count(&self) -> usize {
        self.materials.len()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.materials.contains_key(id)
    }

    // --- Write access ---

    /// Insert or replace a material definition.  Returns the id.
    pub fn upsert(&mut self, def: MaterialDef) -> String {
        let id = def.id.clone();
        if !self.materials.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.materials.insert(id.clone(), def);
        id
    }

    /// Create a new material with a generated id.  Returns the new definition.
    pub fn create(&mut self, name: impl Into<String>) -> &MaterialDef {
        let def = MaterialDef::new(name);
        let id = def.id.clone();
        self.upsert(def);
        self.materials.get(&id).unwrap()
    }

    /// Remove a material by id.  Returns the removed definition if it existed.
    pub fn remove(&mut self, id: &str) -> Option<MaterialDef> {
        if let Some(def) = self.materials.remove(id) {
            self.order.retain(|i| i != id);
            Some(def)
        } else {
            None
        }
    }

    /// Generate a unique name like "Material 1", "Material 2", …
    pub fn generate_unique_name(&self) -> String {
        for i in 1.. {
            let candidate = format!("Material {i}");
            if !self.materials.values().any(|m| m.name == candidate) {
                return candidate;
            }
        }
        unreachable!()
    }
}

// ─── MaterialAssignment component ────────────────────────────────────────────

/// ECS component that links an authored entity to a named material in the
/// `MaterialRegistry`.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialAssignment {
    pub material_id: String,
}

impl MaterialAssignment {
    pub fn new(material_id: impl Into<String>) -> Self {
        Self {
            material_id: material_id.into(),
        }
    }
}

// ─── Bevy material handles cache ─────────────────────────────────────────────

/// Maps `material_id → Handle<StandardMaterial>`.
/// Populated lazily when materials are applied to entities.
#[derive(Resource, Default)]
pub struct MaterialHandleCache(pub BTreeMap<String, Handle<StandardMaterial>>);

// ─── Plugin ───────────────────────────────────────────────────────────────────

pub struct MaterialPlugin;

impl Plugin for MaterialPlugin {
    fn build(&self, app: &mut App) {
        use crate::plugins::command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        };

        app.init_resource::<MaterialRegistry>()
            .init_resource::<MaterialHandleCache>()
            .add_systems(
                Update,
                (
                    rebuild_changed_material_handles,
                    apply_material_assignments,
                    revert_removed_material_assignments,
                )
                    .chain(),
            )
            .register_command(
                CommandDescriptor {
                    id: "materials.apply_to_selection".to_string(),
                    label: "Apply Material".to_string(),
                    description: "Apply a material to all selected entities".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "material_id": { "type": "string" }
                        },
                        "required": ["material_id"]
                    })),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Apply the chosen material to selected entities".to_string()),
                    requires_selection: true,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("materials".to_string()),
                },
                execute_apply_material_to_selection,
            )
            .register_command(
                CommandDescriptor {
                    id: "materials.toggle_browser".to_string(),
                    label: "Materials".to_string(),
                    description: "Show or hide the Materials browser".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+Shift+M".to_string()),
                    icon: None,
                    hint: Some("Browse and edit project materials".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("materials".to_string()),
                },
                execute_toggle_materials_browser,
            );
    }
}

// ─── Command handlers ─────────────────────────────────────────────────────────

fn execute_apply_material_to_selection(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let material_id = params["material_id"]
        .as_str()
        .ok_or("material_id is required")?
        .to_string();

    if !world.resource::<MaterialRegistry>().contains(&material_id) {
        return Err(format!("Material '{material_id}' not found"));
    }

    let selected_entities: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<crate::plugins::selection::Selected>>();
        q.iter(world).collect()
    };
    let count = selected_entities.len();
    for entity in selected_entities {
        world
            .entity_mut(entity)
            .insert(MaterialAssignment::new(material_id.clone()));
    }
    Ok(crate::plugins::command_registry::CommandResult {
        output: Some(serde_json::json!({ "applied_to": count })),
        ..Default::default()
    })
}

fn execute_toggle_materials_browser(
    world: &mut World,
    _params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    if let Some(mut state) =
        world.get_resource_mut::<crate::plugins::material_browser::MaterialsWindowState>()
    {
        state.visible = !state.visible;
    }
    Ok(crate::plugins::command_registry::CommandResult::empty())
}

// ─── Systems ─────────────────────────────────────────────────────────────────

/// When the registry changes, invalidate cached handles so they are rebuilt.
fn rebuild_changed_material_handles(
    registry: Res<MaterialRegistry>,
    mut cache: ResMut<MaterialHandleCache>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    _images: ResMut<Assets<Image>>,
) {
    if !registry.is_changed() {
        return;
    }
    // Remove handles for definitions that have changed or no longer exist.
    let ids_to_remove: Vec<String> = cache
        .0
        .keys()
        .filter(|id| !registry.contains(id))
        .cloned()
        .collect();
    for id in ids_to_remove {
        if let Some(handle) = cache.0.remove(&id) {
            std_materials.remove(handle.id());
        }
    }
    // Invalidate (remove) existing handles so they are rebuilt fresh.
    // The apply system will recreate them when entities need them.
    for id in registry.all().map(|m| m.id.clone()).collect::<Vec<_>>() {
        if let Some(handle) = cache.0.remove(&id) {
            std_materials.remove(handle.id());
        }
    }
}

/// Assign the correct `StandardMaterial` handle to every entity that carries a
/// `MaterialAssignment` and whose assignment has changed.
///
/// When the registry itself changes (e.g. a material colour is edited), ALL
/// entities with a `MaterialAssignment` must be re-patched, not just those
/// whose component changed — otherwise entities with an unchanged assignment
/// would keep a now-stale (or removed) handle.
fn apply_material_assignments(
    mut commands: Commands,
    registry: Res<MaterialRegistry>,
    mut cache: ResMut<MaterialHandleCache>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    changed_query: Query<(Entity, &MaterialAssignment), Changed<MaterialAssignment>>,
    all_query: Query<(Entity, &MaterialAssignment)>,
) {
    // Choose the effective iterator: when the registry itself changed we must
    // visit every assigned entity; otherwise only those with a changed component.
    let registry_changed = registry.is_changed();

    let iter: Box<dyn Iterator<Item = (Entity, &MaterialAssignment)>> = if registry_changed {
        Box::new(all_query.iter())
    } else {
        Box::new(changed_query.iter())
    };

    for (entity, assignment) in iter {
        let Some(def) = registry.get(&assignment.material_id) else {
            continue;
        };
        let handle = cache
            .0
            .entry(assignment.material_id.clone())
            .or_insert_with(|| {
                std_materials.add(def.to_standard_material(&asset_server, &mut images))
            })
            .clone();
        commands
            .entity(entity)
            .insert(MeshMaterial3d::<StandardMaterial>(handle));
    }
}

/// When a `MaterialAssignment` is removed, revert the entity to the default
/// primitive material.
fn revert_removed_material_assignments(
    mut commands: Commands,
    primitive_material: Option<Res<crate::plugins::modeling::mesh_generation::PrimitiveMaterial>>,
    mut removed: RemovedComponents<MaterialAssignment>,
) {
    let Some(prim_mat) = primitive_material else {
        return;
    };
    for entity in removed.read() {
        commands
            .entity(entity)
            .insert(MeshMaterial3d::<StandardMaterial>(prim_mat.0.clone()));
    }
}

// ─── UUID helper ─────────────────────────────────────────────────────────────

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // Simple pseudo-random id based on memory address + time.
    let ptr = uuid_v4 as *const () as usize;
    format!("mat-{:x}-{:x}", ptr ^ (nanos as usize), nanos)
}
