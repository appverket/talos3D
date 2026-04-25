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

use crate::curation::{AssetId, ContentHash, EvidenceRef, MaterialSpecRegistry};

pub const BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO: &str =
    "builtin.maibec.red_cedar_light_h2bo";
pub const BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80: &str = "builtin.glass.blue_tint_glazing_80";
pub const MATERIAL_DEF_KIND: &str = "material_def.v1";

pub fn is_builtin_material_id(id: &str) -> bool {
    id.starts_with("builtin.")
}

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

// ─── Texture assets ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct TextureAssetId(pub String);

impl TextureAssetId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TextureSourceFormat {
    Png,
    Jpeg,
    Webp,
    Hdr,
    Exr,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TextureColorSpace {
    #[default]
    Srgb,
    Linear,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TextureChannelIntent {
    #[default]
    BaseColor,
    Normal,
    MetallicRoughness,
    Emissive,
    Occlusion,
    Generic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TexturePayload {
    Embedded { data: String, mime: String },
    AssetPath(String),
}

impl TexturePayload {
    fn load(
        &self,
        asset_server: &AssetServer,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        match self {
            Self::AssetPath(path) => Some(asset_server.load(path.clone())),
            Self::Embedded { data, mime } => {
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

    fn label(&self) -> &str {
        match self {
            Self::AssetPath(path) => path.split('/').last().unwrap_or(path),
            Self::Embedded { .. } => "<embedded>",
        }
    }

    fn source_format(&self) -> TextureSourceFormat {
        match self {
            Self::Embedded { mime, .. } => match mime.as_str() {
                "image/png" => TextureSourceFormat::Png,
                "image/jpeg" | "image/jpg" => TextureSourceFormat::Jpeg,
                "image/webp" => TextureSourceFormat::Webp,
                _ => TextureSourceFormat::Unknown,
            },
            Self::AssetPath(path) => {
                let lower = path.to_lowercase();
                if lower.ends_with(".png") {
                    TextureSourceFormat::Png
                } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                    TextureSourceFormat::Jpeg
                } else if lower.ends_with(".webp") {
                    TextureSourceFormat::Webp
                } else if lower.ends_with(".hdr") {
                    TextureSourceFormat::Hdr
                } else if lower.ends_with(".exr") {
                    TextureSourceFormat::Exr
                } else {
                    TextureSourceFormat::Unknown
                }
            }
        }
    }

    fn content_hash(&self) -> ContentHash {
        let mut hasher = blake3::Hasher::new();
        match self {
            Self::Embedded { data, mime } => {
                hasher.update(b"embedded:");
                hasher.update(mime.as_bytes());
                hasher.update(b":");
                hasher.update(data.as_bytes());
            }
            Self::AssetPath(path) => {
                hasher.update(b"asset_path:");
                hasher.update(path.as_bytes());
            }
        }
        ContentHash::new(format!("blake3:{}", hasher.finalize().to_hex()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureAsset {
    pub id: TextureAssetId,
    pub content_hash: ContentHash,
    pub source_format: TextureSourceFormat,
    pub color_space: TextureColorSpace,
    pub channel_intent: TextureChannelIntent,
    pub dimensions: Option<[u32; 2]>,
    pub payload: TexturePayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EvidenceRef>,
}

#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TextureRegistry {
    entries: BTreeMap<TextureAssetId, TextureAsset>,
    by_fingerprint:
        BTreeMap<(ContentHash, TextureColorSpace, TextureChannelIntent), TextureAssetId>,
}

impl TextureRegistry {
    pub fn get(&self, id: &TextureAssetId) -> Option<&TextureAsset> {
        self.entries.get(id)
    }

    pub fn insert(&mut self, asset: TextureAsset) -> TextureAssetId {
        let id = asset.id.clone();
        self.by_fingerprint.insert(
            (
                asset.content_hash.clone(),
                asset.color_space,
                asset.channel_intent,
            ),
            id.clone(),
        );
        self.entries.insert(id.clone(), asset);
        id
    }

    pub fn referenced_subset(&self, ids: &std::collections::BTreeSet<TextureAssetId>) -> Self {
        let mut subset = Self::default();
        for id in ids {
            if let Some(asset) = self.entries.get(id) {
                subset.insert(asset.clone());
            }
        }
        subset
    }

    pub fn intern_embedded(
        &mut self,
        data: String,
        mime: String,
        color_space: TextureColorSpace,
        channel_intent: TextureChannelIntent,
    ) -> TextureAssetId {
        self.intern_payload(
            TexturePayload::Embedded { data, mime },
            color_space,
            channel_intent,
        )
    }

    pub fn intern_payload(
        &mut self,
        payload: TexturePayload,
        color_space: TextureColorSpace,
        channel_intent: TextureChannelIntent,
    ) -> TextureAssetId {
        fn color_space_key(color_space: TextureColorSpace) -> &'static str {
            match color_space {
                TextureColorSpace::Srgb => "srgb",
                TextureColorSpace::Linear => "linear",
                TextureColorSpace::Data => "data",
            }
        }

        fn channel_intent_key(channel_intent: TextureChannelIntent) -> &'static str {
            match channel_intent {
                TextureChannelIntent::BaseColor => "base_color",
                TextureChannelIntent::Normal => "normal",
                TextureChannelIntent::MetallicRoughness => "metallic_roughness",
                TextureChannelIntent::Emissive => "emissive",
                TextureChannelIntent::Occlusion => "occlusion",
                TextureChannelIntent::Generic => "generic",
            }
        }

        let hash = payload.content_hash();
        let fingerprint = (hash.clone(), color_space, channel_intent);
        if let Some(existing) = self.by_fingerprint.get(&fingerprint) {
            return existing.clone();
        }
        let id = TextureAssetId::new(format!(
            "texture.v1/{}/{}/{}",
            color_space_key(color_space),
            channel_intent_key(channel_intent),
            hash.as_str()
        ));
        self.insert(TextureAsset {
            id: id.clone(),
            content_hash: hash,
            source_format: payload.source_format(),
            color_space,
            channel_intent,
            dimensions: None,
            payload,
            provenance: None,
        });
        id
    }
}

// ─── TextureRef ──────────────────────────────────────────────────────────────

/// A reference to a texture, either embedded in the project file or pointing to
/// a bundled app asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextureRef {
    /// Reference into the shared `TextureRegistry`.
    TextureAsset { id: TextureAssetId },
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
        registry: Option<&TextureRegistry>,
    ) -> Option<Handle<Image>> {
        match self {
            TextureRef::TextureAsset { id } => registry
                .and_then(|registry| registry.get(id))
                .and_then(|asset| asset.payload.load(asset_server, images)),
            TextureRef::AssetPath(path) => {
                TexturePayload::AssetPath(path.clone()).load(asset_server, images)
            }
            TextureRef::Embedded { data, mime } => TexturePayload::Embedded {
                data: data.clone(),
                mime: mime.clone(),
            }
            .load(asset_server, images),
        }
    }

    /// Returns a short display label (filename or `"<embedded>"`).
    pub fn label<'a>(&'a self, registry: Option<&'a TextureRegistry>) -> &'a str {
        match self {
            TextureRef::TextureAsset { id } => registry
                .and_then(|registry| registry.get(id))
                .map(|asset| asset.payload.label())
                .unwrap_or(id.as_str()),
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
    /// Specular tint for non-metallic highlights.
    pub specular_tint: [f32; 3],
    /// Emissive colour (RGB, HDR values > 1 allowed).
    pub emissive: [f32; 3],
    pub emissive_exposure_weight: f32,
    /// Thin-surface diffuse transmission (translucency).
    pub diffuse_transmission: f32,
    /// Specular transmission / refraction.
    pub specular_transmission: f32,
    /// Thickness of the transmissive volume in metres.
    pub thickness: f32,
    /// Index of refraction.
    pub ior: f32,
    /// Average absorption distance through the transmissive volume.
    pub attenuation_distance: f32,
    /// Resulting color after travelling `attenuation_distance`.
    pub attenuation_color: [f32; 3],
    /// Strength of the clearcoat layer.
    pub clearcoat: f32,
    /// Roughness of the clearcoat layer.
    pub clearcoat_perceptual_roughness: f32,
    /// Strength of anisotropy in tangent space.
    pub anisotropy_strength: f32,
    /// Optional curated construction-material link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<AssetId>,

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
    /// Anisotropy direction in radians relative to the mesh tangent.
    pub anisotropy_rotation: f32,

    // --- Double-sided rendering ---
    pub double_sided: bool,
    /// Ignore lighting and render as pure base color/emissive.
    pub unlit: bool,
    /// Whether this material participates in scene fog.
    pub fog_enabled: bool,
    /// Bias applied to material depth to help with overlays and z-fighting.
    pub depth_bias: f32,
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
            specular_tint: [1.0, 1.0, 1.0],
            emissive: [0.0, 0.0, 0.0],
            emissive_exposure_weight: 1.0,
            diffuse_transmission: 0.0,
            specular_transmission: 0.0,
            thickness: 0.0,
            ior: 1.5,
            attenuation_distance: f32::INFINITY,
            attenuation_color: [1.0, 1.0, 1.0],
            clearcoat: 0.0,
            clearcoat_perceptual_roughness: 0.5,
            anisotropy_strength: 0.0,
            spec_ref: None,
            alpha_mode: MaterialAlphaMode::Opaque,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            normal_map_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            occlusion_texture: None,
            uv_scale: [1.0, 1.0],
            uv_rotation: 0.0,
            anisotropy_rotation: 0.0,
            double_sided: false,
            unlit: false,
            fog_enabled: true,
            depth_bias: 0.0,
        }
    }
}

impl MaterialDef {
    pub fn asset_id(&self) -> AssetId {
        material_def_asset_id(&self.id)
    }

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
        texture_registry: Option<&TextureRegistry>,
    ) -> StandardMaterial {
        let [r, g, b, a] = self.base_color;
        let [sr, sg, sb] = self.specular_tint;
        let [er, eg, eb] = self.emissive;
        let [ar, ag, ab] = self.attenuation_color;

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
                .and_then(|t| t.load(asset_server, images, texture_registry)),
            emissive: LinearRgba::new(er, eg, eb, self.emissive_exposure_weight),
            emissive_texture: self
                .emissive_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images, texture_registry)),
            perceptual_roughness: self.perceptual_roughness,
            metallic: self.metallic,
            metallic_roughness_texture: self
                .metallic_roughness_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images, texture_registry)),
            reflectance: self.reflectance,
            specular_tint: Color::srgb(sr, sg, sb),
            normal_map_texture: self
                .normal_map_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images, texture_registry)),
            occlusion_texture: self
                .occlusion_texture
                .as_ref()
                .and_then(|t| t.load(asset_server, images, texture_registry)),
            diffuse_transmission: self.diffuse_transmission,
            specular_transmission: self.specular_transmission,
            thickness: self.thickness,
            ior: self.ior,
            attenuation_distance: self.attenuation_distance,
            attenuation_color: Color::srgb(ar, ag, ab),
            clearcoat: self.clearcoat,
            clearcoat_perceptual_roughness: self.clearcoat_perceptual_roughness,
            anisotropy_strength: self.anisotropy_strength,
            anisotropy_rotation: self.anisotropy_rotation,
            alpha_mode: self.alpha_mode.to_bevy(self.alpha_cutoff),
            double_sided: self.double_sided,
            cull_mode: if self.double_sided {
                None
            } else {
                Some(bevy::render::render_resource::Face::Back)
            },
            unlit: self.unlit,
            fog_enabled: self.fog_enabled,
            depth_bias: self.depth_bias,
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

pub fn material_texture_asset_ids(
    material: &MaterialDef,
) -> std::collections::BTreeSet<TextureAssetId> {
    [
        material.base_color_texture.as_ref(),
        material.normal_map_texture.as_ref(),
        material.metallic_roughness_texture.as_ref(),
        material.emissive_texture.as_ref(),
        material.occlusion_texture.as_ref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|texture| match texture {
        TextureRef::TextureAsset { id } => Some(id.clone()),
        TextureRef::Embedded { .. } | TextureRef::AssetPath(_) => None,
    })
    .collect()
}

pub fn normalize_material_textures(material: &mut MaterialDef, registry: &mut TextureRegistry) {
    fn normalize_slot(
        slot: &mut Option<TextureRef>,
        registry: &mut TextureRegistry,
        color_space: TextureColorSpace,
        intent: TextureChannelIntent,
    ) {
        let Some(texture) = slot.take() else {
            return;
        };
        *slot = Some(match texture {
            TextureRef::TextureAsset { id } => TextureRef::TextureAsset { id },
            TextureRef::AssetPath(path) => TextureRef::AssetPath(path),
            TextureRef::Embedded { data, mime } => TextureRef::TextureAsset {
                id: registry.intern_embedded(data, mime, color_space, intent),
            },
        });
    }

    normalize_slot(
        &mut material.base_color_texture,
        registry,
        TextureColorSpace::Srgb,
        TextureChannelIntent::BaseColor,
    );
    normalize_slot(
        &mut material.normal_map_texture,
        registry,
        TextureColorSpace::Data,
        TextureChannelIntent::Normal,
    );
    normalize_slot(
        &mut material.metallic_roughness_texture,
        registry,
        TextureColorSpace::Data,
        TextureChannelIntent::MetallicRoughness,
    );
    normalize_slot(
        &mut material.emissive_texture,
        registry,
        TextureColorSpace::Srgb,
        TextureChannelIntent::Emissive,
    );
    normalize_slot(
        &mut material.occlusion_texture,
        registry,
        TextureColorSpace::Data,
        TextureChannelIntent::Occlusion,
    );
}

// ─── MaterialAssignment component ────────────────────────────────────────────

pub fn material_def_asset_id(material_id: &str) -> AssetId {
    AssetId::new(format!("{MATERIAL_DEF_KIND}/{material_id}"))
}

pub fn material_id_from_asset_id(asset_id: &AssetId) -> Option<&str> {
    asset_id
        .as_str()
        .strip_prefix(concat!("material_def.v1", "/"))
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MaterialBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<AssetId>,
}

impl MaterialBinding {
    pub fn render_only(material_id: impl AsRef<str>) -> Self {
        Self {
            spec: None,
            render: Some(material_def_asset_id(material_id.as_ref())),
        }
    }

    pub fn render_material_id(&self, specs: Option<&MaterialSpecRegistry>) -> Option<String> {
        if let Some(render) = &self.render {
            return material_id_from_asset_id(render).map(str::to_string);
        }
        let spec_id = self.spec.as_ref()?;
        let spec = specs?.get(spec_id)?;
        spec.body
            .default_rendering_hint
            .as_ref()
            .and_then(material_id_from_asset_id)
            .map(str::to_string)
    }

    pub fn explicit_render_material_id(&self) -> Option<String> {
        self.render
            .as_ref()
            .and_then(material_id_from_asset_id)
            .map(str::to_string)
    }

    pub fn contains_explicit_render_material_id(&self, material_id: &str) -> bool {
        self.render
            .as_ref()
            .and_then(material_id_from_asset_id)
            .map(|candidate| candidate == material_id)
            .unwrap_or(false)
    }

    pub fn without_explicit_render_material_id(&self, material_id: &str) -> Option<Self> {
        let mut next = self.clone();
        if next.contains_explicit_render_material_id(material_id) {
            next.render = None;
        }
        if next.spec.is_none() && next.render.is_none() {
            None
        } else {
            Some(next)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.spec.is_none() && self.render.is_none()
    }
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MaterialLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness_mm: Option<f32>,
    #[serde(default)]
    pub binding: MaterialBinding,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MaterialLayerSet {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<MaterialLayer>,
}

/// ECS component carrying authored render/material semantics for an entity.
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MaterialAssignment {
    Single(MaterialBinding),
    LayerSet(MaterialLayerSet),
}

impl MaterialAssignment {
    pub fn new(material_id: impl Into<String>) -> Self {
        Self::Single(MaterialBinding::render_only(material_id.into()))
    }

    pub fn render_material_id(&self, specs: Option<&MaterialSpecRegistry>) -> Option<String> {
        match self {
            Self::Single(binding) => binding.render_material_id(specs),
            Self::LayerSet(layer_set) => layer_set
                .layers
                .iter()
                .find_map(|layer| layer.binding.render_material_id(specs)),
        }
    }

    pub fn contains_explicit_render_material_id(&self, material_id: &str) -> bool {
        match self {
            Self::Single(binding) => binding.contains_explicit_render_material_id(material_id),
            Self::LayerSet(layer_set) => layer_set.layers.iter().any(|layer| {
                layer
                    .binding
                    .contains_explicit_render_material_id(material_id)
            }),
        }
    }

    pub fn explicit_render_material_ids(&self) -> Vec<String> {
        match self {
            Self::Single(binding) => binding.explicit_render_material_id().into_iter().collect(),
            Self::LayerSet(layer_set) => layer_set
                .layers
                .iter()
                .filter_map(|layer| layer.binding.explicit_render_material_id())
                .collect(),
        }
    }

    pub fn referenced_spec_ids(&self) -> Vec<AssetId> {
        match self {
            Self::Single(binding) => binding.spec.iter().cloned().collect(),
            Self::LayerSet(layer_set) => layer_set
                .layers
                .iter()
                .filter_map(|layer| layer.binding.spec.clone())
                .collect(),
        }
    }

    pub fn without_explicit_render_material_id(&self, material_id: &str) -> Option<Self> {
        match self {
            Self::Single(binding) => binding
                .without_explicit_render_material_id(material_id)
                .map(Self::Single),
            Self::LayerSet(layer_set) => {
                let layers = layer_set
                    .layers
                    .iter()
                    .filter_map(|layer| {
                        layer
                            .binding
                            .without_explicit_render_material_id(material_id)
                            .map(|binding| MaterialLayer {
                                name: layer.name.clone(),
                                thickness_mm: layer.thickness_mm,
                                binding,
                            })
                    })
                    .collect::<Vec<_>>();
                if layers.is_empty() {
                    None
                } else {
                    Some(Self::LayerSet(MaterialLayerSet { layers }))
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Single(binding) => binding.is_empty(),
            Self::LayerSet(layer_set) => {
                layer_set.layers.is_empty()
                    || layer_set
                        .layers
                        .iter()
                        .all(|layer| layer.binding.is_empty())
            }
        }
    }
}

pub fn material_assignment_from_value(value: &Value) -> Option<MaterialAssignment> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value::<MaterialAssignment>(value.clone())
        .ok()
        .or_else(|| {
            value
                .get("material_id")
                .and_then(Value::as_str)
                .map(MaterialAssignment::new)
        })
}

pub fn material_assignment_to_value(assignment: &MaterialAssignment) -> Value {
    serde_json::to_value(assignment).unwrap_or(Value::Null)
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
            .init_resource::<TextureRegistry>()
            .init_resource::<MaterialHandleCache>()
            .add_systems(Startup, seed_builtin_materials)
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
                    id: "materials.set_assignment_on_selection".to_string(),
                    label: "Set Material Assignment".to_string(),
                    description: "Apply a typed material assignment to all selected entities"
                        .to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "assignment": { "type": "object" }
                        },
                        "required": ["assignment"]
                    })),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Apply a single-material or layered assignment".to_string()),
                    requires_selection: true,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("materials".to_string()),
                },
                execute_set_material_assignment_to_selection,
            )
            .register_command(
                CommandDescriptor {
                    id: "materials.clear_assignment_on_selection".to_string(),
                    label: "Clear Material Assignment".to_string(),
                    description: "Remove material assignments from all selected entities"
                        .to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Clear layered or single material assignments".to_string()),
                    requires_selection: true,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("materials".to_string()),
                },
                execute_clear_material_assignment_on_selection,
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

pub fn ensure_builtin_materials(registry: &mut MaterialRegistry) {
    registry.upsert(MaterialDef {
        id: BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO.to_string(),
        name: "Maibec Red Cedar Light H2BO".to_string(),
        base_color: [0.72, 0.57, 0.44, 1.0],
        perceptual_roughness: 0.92,
        metallic: 0.0,
        reflectance: 0.32,
        specular_tint: [0.98, 0.96, 0.93],
        emissive: [0.0, 0.0, 0.0],
        emissive_exposure_weight: 1.0,
        diffuse_transmission: 0.0,
        specular_transmission: 0.0,
        thickness: 0.0,
        ior: 1.45,
        attenuation_distance: f32::INFINITY,
        attenuation_color: [1.0, 1.0, 1.0],
        clearcoat: 0.08,
        clearcoat_perceptual_roughness: 0.34,
        anisotropy_strength: 0.0,
        spec_ref: None,
        alpha_mode: MaterialAlphaMode::Opaque,
        alpha_cutoff: 0.5,
        base_color_texture: Some(TextureRef::AssetPath(
            "materials/maibec_red_cedar_light_h2bo/diffuse.png".to_string(),
        )),
        normal_map_texture: Some(TextureRef::AssetPath(
            "materials/maibec_red_cedar_light_h2bo/normal_opengl.png".to_string(),
        )),
        metallic_roughness_texture: Some(TextureRef::AssetPath(
            "materials/maibec_red_cedar_light_h2bo/roughness.png".to_string(),
        )),
        emissive_texture: None,
        occlusion_texture: None,
        uv_scale: [1.0, 1.0],
        uv_rotation: 0.0,
        anisotropy_rotation: 0.0,
        double_sided: false,
        unlit: false,
        fog_enabled: true,
        depth_bias: 0.0,
    });

    registry.upsert(MaterialDef {
        id: BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80.to_string(),
        name: "Blue Tint Glazing 80%".to_string(),
        base_color: [0.70, 0.84, 1.0, 0.8],
        perceptual_roughness: 0.08,
        metallic: 0.0,
        reflectance: 0.18,
        specular_tint: [0.88, 0.94, 1.0],
        emissive: [0.0, 0.0, 0.0],
        emissive_exposure_weight: 1.0,
        diffuse_transmission: 0.0,
        specular_transmission: 0.85,
        thickness: 0.024,
        ior: 1.52,
        attenuation_distance: 0.5,
        attenuation_color: [0.72, 0.88, 1.0],
        clearcoat: 0.0,
        clearcoat_perceptual_roughness: 0.03,
        anisotropy_strength: 0.0,
        spec_ref: None,
        alpha_mode: MaterialAlphaMode::Blend,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        normal_map_texture: None,
        metallic_roughness_texture: None,
        emissive_texture: None,
        occlusion_texture: None,
        uv_scale: [1.0, 1.0],
        uv_rotation: 0.0,
        anisotropy_rotation: 0.0,
        double_sided: true,
        unlit: false,
        fog_enabled: true,
        depth_bias: 0.0,
    });
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

fn execute_set_material_assignment_to_selection(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let assignment = params
        .get("assignment")
        .and_then(material_assignment_from_value)
        .ok_or("assignment is required")?;
    validate_material_assignment(world, &assignment)?;

    let selected_entities: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<crate::plugins::selection::Selected>>();
        q.iter(world).collect()
    };
    let count = selected_entities.len();
    for entity in selected_entities {
        world.entity_mut(entity).insert(assignment.clone());
    }
    Ok(crate::plugins::command_registry::CommandResult {
        output: Some(serde_json::json!({ "applied_to": count })),
        ..Default::default()
    })
}

fn execute_clear_material_assignment_on_selection(
    world: &mut World,
    _params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let selected_entities: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<crate::plugins::selection::Selected>>();
        q.iter(world).collect()
    };
    let count = selected_entities.len();
    for entity in selected_entities {
        world.entity_mut(entity).remove::<MaterialAssignment>();
    }
    Ok(crate::plugins::command_registry::CommandResult {
        output: Some(serde_json::json!({ "cleared": count })),
        ..Default::default()
    })
}

pub fn validate_material_assignment(
    world: &World,
    assignment: &MaterialAssignment,
) -> Result<(), String> {
    if assignment.is_empty() {
        return Err("material assignment is empty; clear the assignment instead".to_string());
    }
    for material_id in assignment.explicit_render_material_ids() {
        if !world.resource::<MaterialRegistry>().contains(&material_id) {
            return Err(format!("Material '{material_id}' not found"));
        }
    }
    let spec_ids = assignment.referenced_spec_ids();
    if !spec_ids.is_empty() {
        let registry = world
            .get_resource::<MaterialSpecRegistry>()
            .ok_or_else(|| "MaterialSpecRegistry not installed".to_string())?;
        for spec_id in spec_ids {
            if registry.get(&spec_id).is_none() {
                return Err(format!("MaterialSpec '{}' not found", spec_id));
            }
        }
    }
    Ok(())
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

fn seed_builtin_materials(mut registry: ResMut<MaterialRegistry>) {
    ensure_builtin_materials(&mut registry);
}

/// When the registry changes, invalidate cached handles so they are rebuilt.
fn rebuild_changed_material_handles(
    registry: Res<MaterialRegistry>,
    texture_registry: Res<TextureRegistry>,
    spec_registry: Option<Res<MaterialSpecRegistry>>,
    mut cache: ResMut<MaterialHandleCache>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    _images: ResMut<Assets<Image>>,
) {
    if !registry.is_changed()
        && !texture_registry.is_changed()
        && !spec_registry
            .as_ref()
            .is_some_and(|registry| registry.is_changed())
    {
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
    texture_registry: Res<TextureRegistry>,
    spec_registry: Option<Res<MaterialSpecRegistry>>,
    mut cache: ResMut<MaterialHandleCache>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    primitive_material: Option<Res<crate::plugins::modeling::mesh_generation::PrimitiveMaterial>>,
    changed_query: Query<(Entity, &MaterialAssignment), Changed<MaterialAssignment>>,
    all_query: Query<(Entity, &MaterialAssignment)>,
) {
    // Choose the effective iterator: when the registry itself changed we must
    // visit every assigned entity; otherwise only those with a changed component.
    let registry_changed = registry.is_changed()
        || texture_registry.is_changed()
        || spec_registry
            .as_ref()
            .is_some_and(|spec_registry| spec_registry.is_changed());

    let iter: Box<dyn Iterator<Item = (Entity, &MaterialAssignment)>> = if registry_changed {
        Box::new(all_query.iter())
    } else {
        Box::new(changed_query.iter())
    };

    for (entity, assignment) in iter {
        let handle = assignment
            .render_material_id(spec_registry.as_deref())
            .and_then(|material_id| {
                let def = registry.get(&material_id)?;
                let handle = cache
                    .0
                    .entry(material_id.clone())
                    .or_insert_with(|| {
                        std_materials.add(def.to_standard_material(
                            &asset_server,
                            &mut images,
                            Some(&texture_registry),
                        ))
                    })
                    .clone();
                Some(handle)
            })
            .or_else(|| {
                primitive_material
                    .as_ref()
                    .map(|material| material.0.clone())
            });
        commands.queue(move |world: &mut World| {
            if let (Some(handle), Ok(mut entity_mut)) =
                (handle.clone(), world.get_entity_mut(entity))
            {
                entity_mut.insert(MeshMaterial3d::<StandardMaterial>(handle));
            }
        });
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
        let handle = prim_mat.0.clone();
        commands.queue(move |world: &mut World| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(MeshMaterial3d::<StandardMaterial>(handle));
            }
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_materials_are_seeded_with_stable_ids() {
        let mut registry = MaterialRegistry::default();
        ensure_builtin_materials(&mut registry);

        assert!(registry.contains(BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO));
        assert!(registry.contains(BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80));

        let cedar = registry
            .get(BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO)
            .expect("cedar builtin should exist");
        assert_eq!(cedar.anisotropy_strength, 0.0);
        assert_eq!(cedar.anisotropy_rotation, 0.0);
    }

    #[test]
    fn material_assignment_prefers_explicit_render_then_spec_hint() {
        let mut specs = MaterialSpecRegistry::default();
        let spec_id = crate::curation::MaterialSpec::asset_id_for("c24");
        specs.insert(crate::curation::MaterialSpec::draft(
            spec_id.clone(),
            crate::curation::MaterialSpecBody {
                display_name: "C24".into(),
                default_rendering_hint: Some(material_def_asset_id("oak_finish")),
                ..Default::default()
            },
            crate::plugins::refinement::AgentId("codex".into()),
            None,
        ));

        let hinted = MaterialAssignment::Single(MaterialBinding {
            spec: Some(spec_id),
            render: None,
        });
        assert_eq!(
            hinted.render_material_id(Some(&specs)).as_deref(),
            Some("oak_finish")
        );

        let explicit = MaterialAssignment::Single(MaterialBinding {
            spec: None,
            render: Some(material_def_asset_id("paint_white")),
        });
        assert_eq!(
            explicit.render_material_id(Some(&specs)).as_deref(),
            Some("paint_white")
        );
    }

    #[test]
    fn removing_explicit_render_keeps_spec_binding_when_present() {
        let assignment = MaterialAssignment::Single(MaterialBinding {
            spec: Some(crate::curation::MaterialSpec::asset_id_for("gypsum_board")),
            render: Some(material_def_asset_id("paint_white")),
        });

        let stripped = assignment
            .without_explicit_render_material_id("paint_white")
            .expect("spec binding should remain");

        match stripped {
            MaterialAssignment::Single(binding) => {
                assert!(binding.spec.is_some());
                assert!(binding.render.is_none());
            }
            other => panic!("expected single binding, got {other:?}"),
        }
    }

    #[test]
    fn material_assignment_json_round_trips_single_and_layer_set_forms() {
        let single = MaterialAssignment::Single(MaterialBinding {
            spec: Some(crate::curation::MaterialSpec::asset_id_for("c24")),
            render: Some(material_def_asset_id("oak_finish")),
        });
        let single_value = material_assignment_to_value(&single);
        assert_eq!(material_assignment_from_value(&single_value), Some(single));

        let layered = MaterialAssignment::LayerSet(MaterialLayerSet {
            layers: vec![
                MaterialLayer {
                    name: Some("structure".to_string()),
                    thickness_mm: Some(45.0),
                    binding: MaterialBinding {
                        spec: Some(crate::curation::MaterialSpec::asset_id_for("c24")),
                        render: Some(material_def_asset_id("oak_finish")),
                    },
                },
                MaterialLayer {
                    name: Some("finish".to_string()),
                    thickness_mm: Some(12.5),
                    binding: MaterialBinding {
                        spec: None,
                        render: Some(material_def_asset_id("paint_white")),
                    },
                },
            ],
        });
        let layered_value = material_assignment_to_value(&layered);
        assert_eq!(
            material_assignment_from_value(&layered_value),
            Some(layered)
        );
    }

    #[test]
    fn normalize_material_textures_reuses_matching_payload_and_binding_metadata() {
        let mut texture_registry = TextureRegistry::default();

        let mut first = MaterialDef::new("First");
        first.base_color_texture = Some(TextureRef::Embedded {
            data: "abc123".to_string(),
            mime: "image/png".to_string(),
        });
        normalize_material_textures(&mut first, &mut texture_registry);

        let mut second = MaterialDef::new("Second");
        second.base_color_texture = Some(TextureRef::Embedded {
            data: "abc123".to_string(),
            mime: "image/png".to_string(),
        });
        normalize_material_textures(&mut second, &mut texture_registry);

        let first_id = match first.base_color_texture.as_ref() {
            Some(TextureRef::TextureAsset { id }) => id.clone(),
            other => panic!("expected normalized texture asset, got {other:?}"),
        };
        let second_id = match second.base_color_texture.as_ref() {
            Some(TextureRef::TextureAsset { id }) => id.clone(),
            other => panic!("expected normalized texture asset, got {other:?}"),
        };

        assert_eq!(first_id, second_id);
        assert_eq!(texture_registry.entries.len(), 1);
    }

    #[test]
    fn normalize_material_textures_keeps_distinct_binding_semantics() {
        let mut texture_registry = TextureRegistry::default();
        let shared_texture = TextureRef::Embedded {
            data: "same-bytes".to_string(),
            mime: "image/png".to_string(),
        };
        let mut material = MaterialDef::new("Semantic Split");
        material.base_color_texture = Some(shared_texture.clone());
        material.normal_map_texture = Some(shared_texture);

        normalize_material_textures(&mut material, &mut texture_registry);

        let base_id = match material.base_color_texture.as_ref() {
            Some(TextureRef::TextureAsset { id }) => id.clone(),
            other => panic!("expected base color texture asset, got {other:?}"),
        };
        let normal_id = match material.normal_map_texture.as_ref() {
            Some(TextureRef::TextureAsset { id }) => id.clone(),
            other => panic!("expected normal texture asset, got {other:?}"),
        };

        assert_ne!(base_id, normal_id);
        assert_eq!(texture_registry.entries.len(), 2);
        assert_eq!(
            texture_registry
                .get(&base_id)
                .expect("base texture should exist")
                .channel_intent,
            TextureChannelIntent::BaseColor
        );
        assert_eq!(
            texture_registry
                .get(&normal_id)
                .expect("normal texture should exist")
                .color_space,
            TextureColorSpace::Data
        );
    }
}
