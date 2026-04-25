/// Material browser and editor panel.
///
/// Follows the same floating-window pattern as `definition_browser`.
/// Exposed via `draw_materials_window` which is called from `egui_chrome`.
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::{
    asset::AssetServer,
    prelude::{Assets, Handle, Image},
};
use bevy_egui::{egui, EguiContexts, EguiTextureHandle};

use crate::curation::{
    NominationId, NominationKind, NominationQueue, SourceId, SourceLicense, SourceRegistry,
    SourceRegistryEntry, SourceRevision, SourceTier,
};
use crate::plugins::{
    command_registry::{queue_command_invocation_resource, PendingCommandInvocations},
    materials::{
        material_assignment_from_value, material_assignment_to_value, normalize_material_textures,
        MaterialAlphaMode, MaterialAssignment, MaterialDef, MaterialRegistry, TextureRef,
        TextureRegistry,
    },
    ui::{tool_window_max_size, tool_window_rect},
};

const MATERIALS_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(480.0, 560.0);
const MATERIALS_WINDOW_MIN_SIZE: egui::Vec2 = egui::vec2(340.0, 280.0);
const MATERIALS_WINDOW_MAX_SIZE: egui::Vec2 = egui::vec2(640.0, 760.0);
const MATERIAL_LIST_THUMBNAIL_SIZE: f32 = 30.0;
const TEXTURE_SLOT_THUMBNAIL_SIZE: f32 = 52.0;
const SOURCE_TIER_OPTIONS: [(&str, SourceTier); 5] = [
    ("canonical", SourceTier::Canonical),
    ("jurisdictional", SourceTier::Jurisdictional),
    ("organizational", SourceTier::Organizational),
    ("project", SourceTier::Project),
    ("ad_hoc", SourceTier::AdHoc),
];
const SOURCE_LICENSE_OPTIONS: [(&str, SourceLicense); 5] = [
    ("public_domain", SourceLicense::PublicDomain),
    (
        "official_government_publication",
        SourceLicense::OfficialGovernmentPublication,
    ),
    ("permissive_cite", SourceLicense::PermissiveCite),
    ("licensed_excerpt", SourceLicense::LicensedExcerpt),
    ("user_attached_private", SourceLicense::UserAttachedPrivate),
];
const SOURCE_PRESET_NAMES: [&str; 3] = ["Custom", "ambientCG", "Poly Haven"];

// ─── Window state ─────────────────────────────────────────────────────────────

#[derive(bevy::prelude::Resource, Default, Debug, Clone)]
pub struct MaterialsWindowState {
    pub visible: bool,
    pub search: String,
    pub selected_id: Option<String>,
    // Inline editor buffers (so we don't mutate the registry on every keypress)
    pub name_buf: String,
    pub spec_ref_buf: String,
    pub base_color: [f32; 4],
    pub roughness: f32,
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
    pub alpha_mode_idx: usize,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
    pub unlit: bool,
    pub fog_enabled: bool,
    pub depth_bias: f32,
    pub uv_scale: [f32; 2],
    pub uv_rotation_deg: f32,
    pub anisotropy_rotation_deg: f32,
    pub base_color_tex: Option<TextureRef>,
    pub normal_map_tex: Option<TextureRef>,
    pub metallic_roughness_tex: Option<TextureRef>,
    pub emissive_tex: Option<TextureRef>,
    pub occlusion_tex: Option<TextureRef>,
    preview_texture_handles: HashMap<String, Handle<Image>>,
    pub editor_tab: EditorTab,
    pub dirty: bool,
    pub selection_assignment_json: String,
    pub selection_assignment_signature: u64,
    pub selection_assignment_mixed: bool,
    pub selection_status: Option<String>,
    pub source_search: String,
    pub selected_source_key: Option<String>,
    pub source_draft: SourceDraftState,
    pub source_status_message: Option<String>,
    pub source_status_is_error: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum EditorTab {
    #[default]
    Properties,
    Textures,
    Selection,
    Sources,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedMaterialContext {
    name: String,
    spec_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDraftState {
    pub preset_idx: usize,
    pub source_id: String,
    pub revision: String,
    pub title: String,
    pub publisher: String,
    pub tier_idx: usize,
    pub license_idx: usize,
    pub jurisdiction: String,
    pub canonical_url: String,
    pub metadata_json: String,
    pub justification: String,
}

impl Default for SourceDraftState {
    fn default() -> Self {
        Self {
            preset_idx: 0,
            source_id: String::new(),
            revision: String::new(),
            title: String::new(),
            publisher: String::new(),
            tier_idx: source_tier_to_idx(SourceTier::Project),
            license_idx: source_license_to_idx(SourceLicense::UserAttachedPrivate),
            jurisdiction: String::new(),
            canonical_url: String::new(),
            metadata_json: String::new(),
            justification: String::new(),
        }
    }
}

impl SourceDraftState {
    fn load_from_entry(&mut self, entry: &SourceRegistryEntry) {
        self.source_id = entry.source_id.0.clone();
        self.revision = entry.revision.0.clone();
        self.title = entry.title.clone();
        self.publisher = entry.publisher.clone();
        self.tier_idx = source_tier_to_idx(entry.tier);
        self.license_idx = source_license_to_idx(entry.license);
        self.jurisdiction = entry
            .jurisdiction
            .as_ref()
            .map(|j| j.0.clone())
            .unwrap_or_default();
        self.canonical_url = entry.canonical_url.clone().unwrap_or_default();
        self.metadata_json = if entry.metadata.is_null() {
            String::new()
        } else {
            serde_json::to_string_pretty(&entry.metadata).unwrap_or_else(|_| "{}".to_string())
        };
        self.justification.clear();
        self.preset_idx = 0;
    }

    fn apply_preset(&mut self) {
        match self.preset_idx {
            1 => {
                self.publisher = "ambientCG".to_string();
                self.tier_idx = source_tier_to_idx(SourceTier::AdHoc);
                self.license_idx = source_license_to_idx(SourceLicense::PublicDomain);
                self.canonical_url = "https://ambientcg.com/".to_string();
                self.metadata_json =
                    "{\n  \"provider\": \"ambientcg\",\n  \"asset_id\": \"\",\n  \"asset_kind\": \"material\"\n}"
                        .to_string();
            }
            2 => {
                self.publisher = "Poly Haven".to_string();
                self.tier_idx = source_tier_to_idx(SourceTier::AdHoc);
                self.license_idx = source_license_to_idx(SourceLicense::PublicDomain);
                self.canonical_url = "https://polyhaven.com/".to_string();
                self.metadata_json =
                    "{\n  \"provider\": \"poly_haven\",\n  \"asset_slug\": \"\",\n  \"asset_kind\": \"material\"\n}"
                        .to_string();
            }
            _ => {}
        }
    }

    fn build_entry(&self) -> Result<SourceRegistryEntry, String> {
        let source_id = self.source_id.trim();
        if source_id.is_empty() {
            return Err("Source ID is required.".to_string());
        }
        let revision = self.revision.trim();
        if revision.is_empty() {
            return Err("Revision is required.".to_string());
        }
        let title = self.title.trim();
        if title.is_empty() {
            return Err("Title is required.".to_string());
        }
        let publisher = self.publisher.trim();
        if publisher.is_empty() {
            return Err("Publisher is required.".to_string());
        }
        let metadata = if self.metadata_json.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&self.metadata_json)
                .map_err(|error| format!("Metadata JSON is invalid: {error}"))?
        };
        let mut entry = SourceRegistryEntry::new(
            SourceId::new(source_id.to_string()),
            SourceRevision::new(revision.to_string()),
            title.to_string(),
            publisher.to_string(),
            source_tier_by_idx(self.tier_idx),
            source_license_by_idx(self.license_idx),
        );
        if let Some(jurisdiction) = trimmed_or_none(&self.jurisdiction) {
            entry.jurisdiction = Some(crate::curation::JurisdictionTag::new(jurisdiction));
        }
        entry.canonical_url = trimmed_or_none(&self.canonical_url);
        entry.metadata = metadata;
        Ok(entry)
    }

    fn key(&self) -> Option<String> {
        let source_id = self.source_id.trim();
        let revision = self.revision.trim();
        (!source_id.is_empty() && !revision.is_empty())
            .then(|| source_key_parts(source_id, revision))
    }
}

impl MaterialsWindowState {
    /// Load editor buffers from a `MaterialDef`.
    pub fn load_def(&mut self, def: &MaterialDef) {
        self.name_buf = def.name.clone();
        self.spec_ref_buf = def
            .spec_ref
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default();
        self.base_color = def.base_color;
        self.roughness = def.perceptual_roughness;
        self.metallic = def.metallic;
        self.reflectance = def.reflectance;
        self.specular_tint = def.specular_tint;
        self.emissive = def.emissive;
        self.emissive_exposure_weight = def.emissive_exposure_weight;
        self.diffuse_transmission = def.diffuse_transmission;
        self.specular_transmission = def.specular_transmission;
        self.thickness = def.thickness;
        self.ior = def.ior;
        self.attenuation_distance = if def.attenuation_distance.is_finite() {
            def.attenuation_distance
        } else {
            1_000_000.0
        };
        self.attenuation_color = def.attenuation_color;
        self.clearcoat = def.clearcoat;
        self.clearcoat_perceptual_roughness = def.clearcoat_perceptual_roughness;
        self.anisotropy_strength = def.anisotropy_strength;
        self.alpha_mode_idx = alpha_mode_to_idx(&def.alpha_mode);
        self.alpha_cutoff = def.alpha_cutoff;
        self.double_sided = def.double_sided;
        self.unlit = def.unlit;
        self.fog_enabled = def.fog_enabled;
        self.depth_bias = def.depth_bias;
        self.uv_scale = def.uv_scale;
        self.uv_rotation_deg = def.uv_rotation.to_degrees();
        self.anisotropy_rotation_deg = def.anisotropy_rotation.to_degrees();
        self.base_color_tex = def.base_color_texture.clone();
        self.normal_map_tex = def.normal_map_texture.clone();
        self.metallic_roughness_tex = def.metallic_roughness_texture.clone();
        self.emissive_tex = def.emissive_texture.clone();
        self.occlusion_tex = def.occlusion_texture.clone();
        self.dirty = false;
    }

    /// Write editor buffers back into a `MaterialDef`.
    pub fn flush_to_def(&self, def: &mut MaterialDef) {
        def.name = self.name_buf.clone();
        def.spec_ref = (!self.spec_ref_buf.trim().is_empty())
            .then(|| crate::curation::AssetId::new(self.spec_ref_buf.trim().to_string()));
        def.base_color = self.base_color;
        def.perceptual_roughness = self.roughness;
        def.metallic = self.metallic;
        def.reflectance = self.reflectance;
        def.specular_tint = self.specular_tint;
        def.emissive = self.emissive;
        def.emissive_exposure_weight = self.emissive_exposure_weight;
        def.diffuse_transmission = self.diffuse_transmission;
        def.specular_transmission = self.specular_transmission;
        def.thickness = self.thickness;
        def.ior = self.ior;
        def.attenuation_distance = if self.attenuation_distance >= 999_999.0 {
            f32::INFINITY
        } else {
            self.attenuation_distance
        };
        def.attenuation_color = self.attenuation_color;
        def.clearcoat = self.clearcoat;
        def.clearcoat_perceptual_roughness = self.clearcoat_perceptual_roughness;
        def.anisotropy_strength = self.anisotropy_strength;
        def.alpha_mode = idx_to_alpha_mode(self.alpha_mode_idx);
        def.alpha_cutoff = self.alpha_cutoff;
        def.double_sided = self.double_sided;
        def.unlit = self.unlit;
        def.fog_enabled = self.fog_enabled;
        def.depth_bias = self.depth_bias;
        def.uv_scale = self.uv_scale;
        def.uv_rotation = self.uv_rotation_deg.to_radians();
        def.anisotropy_rotation = self.anisotropy_rotation_deg.to_radians();
        def.base_color_texture = self.base_color_tex.clone();
        def.normal_map_texture = self.normal_map_tex.clone();
        def.metallic_roughness_texture = self.metallic_roughness_tex.clone();
        def.emissive_texture = self.emissive_tex.clone();
        def.occlusion_texture = self.occlusion_tex.clone();
    }
}

// ─── Main entry point ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_materials_window(
    contexts: &mut EguiContexts,
    ctx: &egui::Context,
    state: &mut MaterialsWindowState,
    registry: &mut MaterialRegistry,
    texture_registry: &mut TextureRegistry,
    source_registry: &mut SourceRegistry,
    nomination_queue: &mut NominationQueue,
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    pending: &mut PendingCommandInvocations,
    selected_assignments: &[(u64, Option<MaterialAssignment>)],
) {
    if !state.visible {
        return;
    }

    let default_rect = tool_window_rect(ctx, egui::pos2(24.0, 88.0), MATERIALS_WINDOW_DEFAULT_SIZE);
    let mut open = state.visible;

    egui::Window::new("Materials")
        .id(egui::Id::new("materials_browser"))
        .default_rect(default_rect)
        .min_size(MATERIALS_WINDOW_MIN_SIZE)
        .max_size(tool_window_max_size(ctx, MATERIALS_WINDOW_MAX_SIZE))
        .constrain_to(ctx.content_rect())
        .open(&mut open)
        .show(ctx, |ui| {
            draw_browser_panel(
                ui,
                contexts,
                state,
                registry,
                texture_registry,
                source_registry,
                nomination_queue,
                asset_server,
                images,
                pending,
                selected_assignments,
            );
        });

    state.visible = open;
}

// ─── Browser panel (left list + right editor) ─────────────────────────────────

fn draw_browser_panel(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    state: &mut MaterialsWindowState,
    registry: &mut MaterialRegistry,
    texture_registry: &mut TextureRegistry,
    source_registry: &mut SourceRegistry,
    nomination_queue: &mut NominationQueue,
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    pending: &mut PendingCommandInvocations,
    selected_assignments: &[(u64, Option<MaterialAssignment>)],
) {
    ui.horizontal(|ui| {
        // --- Left: list of materials ---
        ui.vertical(|ui| {
            ui.set_min_width(160.0);
            ui.set_max_width(180.0);

            // Search bar
            ui.horizontal(|ui| {
                ui.label("🔍");
                ui.text_edit_singleline(&mut state.search);
            });
            ui.separator();

            // New material button
            if ui.button("+ New").clicked() {
                let name = registry.generate_unique_name();
                let def = registry.create(name);
                let id = def.id.clone();
                state.selected_id = Some(id.clone());
                if let Some(def) = registry.get(&id) {
                    state.load_def(def);
                }
            }

            ui.add_space(4.0);

            // Material list
            egui::ScrollArea::vertical()
                .id_salt("mat_list")
                .show(ui, |ui| {
                    let search_lower = state.search.to_lowercase();
                    let ids: Vec<String> = registry
                        .all()
                        .filter(|m| {
                            search_lower.is_empty() || m.name.to_lowercase().contains(&search_lower)
                        })
                        .map(|m| m.id.clone())
                        .collect();

                    let mut delete_id: Option<String> = None;

                    for id in ids {
                        let Some(def) = registry.get(&id) else {
                            continue;
                        };
                        let selected = state.selected_id.as_deref() == Some(id.as_str());

                        ui.horizontal(|ui| {
                            draw_material_list_thumbnail(
                                ui,
                                contexts,
                                &mut state.preview_texture_handles,
                                asset_server,
                                texture_registry,
                                images,
                                def,
                            );

                            let label = egui::Button::new(format!(
                                "{}\n{}",
                                def.name,
                                material_meta_label(def)
                            ))
                            .selected(selected)
                            .wrap();
                            if ui.add_sized([ui.available_width(), 36.0], label).clicked() {
                                if state.selected_id.as_deref() != Some(id.as_str()) {
                                    state.selected_id = Some(id.clone());
                                    if let Some(def) = registry.get(&id) {
                                        state.load_def(def);
                                    }
                                }
                            }
                        });

                        if selected {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                if ui
                                    .small_button("Apply to selection")
                                    .on_hover_text("Apply this material to all selected entities")
                                    .clicked()
                                {
                                    queue_command_invocation_resource(
                                        pending,
                                        "materials.apply_to_selection".to_string(),
                                        serde_json::json!({ "material_id": id }),
                                    );
                                }
                                if ui
                                    .small_button("🗑")
                                    .on_hover_text("Delete material")
                                    .clicked()
                                {
                                    delete_id = Some(id.clone());
                                }
                            });
                        }
                    }

                    if let Some(id) = delete_id {
                        registry.remove(&id);
                        if state.selected_id.as_deref() == Some(id.as_str()) {
                            state.selected_id = None;
                        }
                    }
                });
        });

        ui.separator();

        // --- Right: editor ---
        ui.vertical(|ui| {
            ui.set_min_width(240.0);

            draw_editor_tabs(ui, state);
            ui.separator();

            if state.editor_tab == EditorTab::Selection {
                let selected_material_id = state.selected_id.clone();
                sync_selection_assignment_buffer(state, selected_assignments);
                draw_selection_tab(
                    ui,
                    state,
                    pending,
                    selected_assignments,
                    selected_material_id.as_deref(),
                );
            } else if state.editor_tab == EditorTab::Sources {
                let selected_material_context = state
                    .selected_id
                    .as_deref()
                    .and_then(|id| registry.get(id))
                    .map(|material| SelectedMaterialContext {
                        name: material.name.clone(),
                        spec_ref: material
                            .spec_ref
                            .as_ref()
                            .map(|asset_id| asset_id.as_str().to_string()),
                    });
                draw_sources_tab(
                    ui,
                    state,
                    source_registry,
                    nomination_queue,
                    selected_material_context,
                );
            } else if let Some(selected_id) = state.selected_id.clone() {
                if registry.contains(&selected_id) {
                    draw_material_editor(
                        ui,
                        contexts,
                        state,
                        registry,
                        texture_registry,
                        asset_server,
                        images,
                        &selected_id,
                    );
                } else {
                    ui.label("(Material removed)");
                    state.selected_id = None;
                }
            } else {
                ui.add_space(40.0);
                ui.label(
                    egui::RichText::new("Select a material to edit it")
                        .weak()
                        .italics(),
                );
            }
        });
    });
}

fn draw_editor_tabs(ui: &mut egui::Ui, state: &mut MaterialsWindowState) {
    ui.horizontal(|ui| {
        if ui
            .selectable_label(state.editor_tab == EditorTab::Properties, "Properties")
            .clicked()
        {
            state.editor_tab = EditorTab::Properties;
        }
        if ui
            .selectable_label(state.editor_tab == EditorTab::Textures, "Textures")
            .clicked()
        {
            state.editor_tab = EditorTab::Textures;
        }
        if ui
            .selectable_label(state.editor_tab == EditorTab::Selection, "Selection")
            .clicked()
        {
            state.editor_tab = EditorTab::Selection;
        }
        if ui
            .selectable_label(state.editor_tab == EditorTab::Sources, "Sources")
            .clicked()
        {
            state.editor_tab = EditorTab::Sources;
        }
    });
}

// ─── Editor panel ─────────────────────────────────────────────────────────────

fn draw_material_editor(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    state: &mut MaterialsWindowState,
    registry: &mut MaterialRegistry,
    texture_registry: &mut TextureRegistry,
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    id: &str,
) {
    match state.editor_tab {
        EditorTab::Properties => draw_properties_tab(ui, state),
        EditorTab::Textures => {
            draw_textures_tab(ui, contexts, state, texture_registry, asset_server, images)
        }
        EditorTab::Selection | EditorTab::Sources => {}
    }

    ui.add_space(8.0);
    ui.separator();

    // Apply / revert
    ui.horizontal(|ui| {
        let apply = ui
            .add_enabled(state.dirty, egui::Button::new("Apply"))
            .on_hover_text("Save changes to registry");
        if apply.clicked() {
            if let Some(def) = registry.get_mut(id) {
                state.flush_to_def(def);
                normalize_material_textures(def, texture_registry);
                state.dirty = false;
            }
        }
        if ui
            .button("Revert")
            .on_hover_text("Discard unsaved changes")
            .clicked()
        {
            if let Some(def) = registry.get(id) {
                state.load_def(&def.clone());
            }
        }
    });
}

fn draw_properties_tab(ui: &mut egui::Ui, state: &mut MaterialsWindowState) {
    egui::ScrollArea::vertical()
        .id_salt("mat_props")
        .show(ui, |ui| {
            egui::Grid::new("mat_prop_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    // Name
                    ui.label("Name");
                    if ui.text_edit_singleline(&mut state.name_buf).changed() {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Material Spec");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut state.spec_ref_buf)
                                .hint_text("material_spec.v1/slug"),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    // Base colour
                    ui.label("Base Color");
                    let [r, g, b, a] = state.base_color;
                    let mut col = egui::Color32::from_rgba_premultiplied(
                        (r * 255.0) as u8,
                        (g * 255.0) as u8,
                        (b * 255.0) as u8,
                        (a * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        let [cr, cg, cb, ca] = col.to_srgba_unmultiplied();
                        state.base_color = [
                            cr as f32 / 255.0,
                            cg as f32 / 255.0,
                            cb as f32 / 255.0,
                            ca as f32 / 255.0,
                        ];
                        state.dirty = true;
                    }
                    ui.end_row();

                    // Roughness
                    ui.label("Roughness");
                    if ui
                        .add(egui::Slider::new(&mut state.roughness, 0.0..=1.0).fixed_decimals(2))
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    // Metallic
                    ui.label("Metallic");
                    if ui
                        .add(egui::Slider::new(&mut state.metallic, 0.0..=1.0).fixed_decimals(2))
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    // Reflectance
                    ui.label("Reflectance");
                    if ui
                        .add(egui::Slider::new(&mut state.reflectance, 0.0..=1.0).fixed_decimals(2))
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Specular Tint");
                    let [sr, sg, sb] = state.specular_tint;
                    let mut scol = egui::Color32::from_rgb(
                        (sr.clamp(0.0, 1.0) * 255.0) as u8,
                        (sg.clamp(0.0, 1.0) * 255.0) as u8,
                        (sb.clamp(0.0, 1.0) * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut scol).changed() {
                        let [cr, cg, cb, _] = scol.to_srgba_unmultiplied();
                        state.specular_tint =
                            [cr as f32 / 255.0, cg as f32 / 255.0, cb as f32 / 255.0];
                        state.dirty = true;
                    }
                    ui.end_row();

                    // Emissive
                    ui.label("Emissive");
                    let [er, eg, eb] = state.emissive;
                    let mut ecol = egui::Color32::from_rgb(
                        (er.clamp(0.0, 1.0) * 255.0) as u8,
                        (eg.clamp(0.0, 1.0) * 255.0) as u8,
                        (eb.clamp(0.0, 1.0) * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut ecol).changed() {
                        let [cr, cg, cb, _] = ecol.to_srgba_unmultiplied();
                        state.emissive = [cr as f32 / 255.0, cg as f32 / 255.0, cb as f32 / 255.0];
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Emissive Strength");
                    if ui
                        .add(
                            egui::Slider::new(&mut state.emissive_exposure_weight, 0.0..=8.0)
                                .fixed_decimals(2),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Diffuse Transmission");
                    if ui
                        .add(
                            egui::Slider::new(&mut state.diffuse_transmission, 0.0..=1.0)
                                .fixed_decimals(2),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Specular Transmission");
                    if ui
                        .add(
                            egui::Slider::new(&mut state.specular_transmission, 0.0..=1.0)
                                .fixed_decimals(2),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Thickness (m)");
                    if ui
                        .add(
                            egui::DragValue::new(&mut state.thickness)
                                .speed(0.005)
                                .range(0.0..=10.0),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("IOR");
                    if ui
                        .add(
                            egui::DragValue::new(&mut state.ior)
                                .speed(0.01)
                                .range(1.0..=3.0),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Attenuation Dist.");
                    if ui
                        .add(
                            egui::DragValue::new(&mut state.attenuation_distance)
                                .speed(0.05)
                                .range(0.0..=1000.0),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Attenuation Color");
                    let [atr, atg, atb] = state.attenuation_color;
                    let mut atcol = egui::Color32::from_rgb(
                        (atr.clamp(0.0, 1.0) * 255.0) as u8,
                        (atg.clamp(0.0, 1.0) * 255.0) as u8,
                        (atb.clamp(0.0, 1.0) * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut atcol).changed() {
                        let [cr, cg, cb, _] = atcol.to_srgba_unmultiplied();
                        state.attenuation_color =
                            [cr as f32 / 255.0, cg as f32 / 255.0, cb as f32 / 255.0];
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Clearcoat");
                    if ui
                        .add(egui::Slider::new(&mut state.clearcoat, 0.0..=1.0).fixed_decimals(2))
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Clearcoat Rough.");
                    if ui
                        .add(
                            egui::Slider::new(&mut state.clearcoat_perceptual_roughness, 0.0..=1.0)
                                .fixed_decimals(2),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Anisotropy");
                    if ui
                        .add(
                            egui::Slider::new(&mut state.anisotropy_strength, 0.0..=1.0)
                                .fixed_decimals(2),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Aniso Rotation (°)");
                    if ui
                        .add(
                            egui::DragValue::new(&mut state.anisotropy_rotation_deg)
                                .speed(1.0)
                                .suffix("°"),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    // Alpha mode
                    ui.label("Alpha Mode");
                    egui::ComboBox::from_id_salt("alpha_mode_combo")
                        .selected_text(alpha_mode_name(state.alpha_mode_idx))
                        .show_ui(ui, |ui| {
                            for (i, name) in ALPHA_MODE_NAMES.iter().enumerate() {
                                if ui
                                    .selectable_label(state.alpha_mode_idx == i, *name)
                                    .clicked()
                                {
                                    state.alpha_mode_idx = i;
                                    state.dirty = true;
                                }
                            }
                        });
                    ui.end_row();

                    if state.alpha_mode_idx == 1 {
                        // Mask
                        ui.label("Alpha Cutoff");
                        if ui
                            .add(
                                egui::Slider::new(&mut state.alpha_cutoff, 0.0..=1.0)
                                    .fixed_decimals(2),
                            )
                            .changed()
                        {
                            state.dirty = true;
                        }
                        ui.end_row();
                    }

                    // Double-sided
                    ui.label("Double-sided");
                    if ui.checkbox(&mut state.double_sided, "").changed() {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Unlit");
                    if ui.checkbox(&mut state.unlit, "").changed() {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Fog Enabled");
                    if ui.checkbox(&mut state.fog_enabled, "").changed() {
                        state.dirty = true;
                    }
                    ui.end_row();

                    // UV scale
                    ui.label("UV Tiling (X, Y)");
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::DragValue::new(&mut state.uv_scale[0])
                                    .speed(0.05)
                                    .range(0.01..=100.0)
                                    .prefix("X:"),
                            )
                            .changed()
                        {
                            state.dirty = true;
                        }
                        if ui
                            .add(
                                egui::DragValue::new(&mut state.uv_scale[1])
                                    .speed(0.05)
                                    .range(0.01..=100.0)
                                    .prefix("Y:"),
                            )
                            .changed()
                        {
                            state.dirty = true;
                        }
                    });
                    ui.end_row();

                    // UV rotation
                    ui.label("UV Rotation (°)");
                    if ui
                        .add(
                            egui::DragValue::new(&mut state.uv_rotation_deg)
                                .speed(1.0)
                                .suffix("°"),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Depth Bias");
                    if ui
                        .add(
                            egui::DragValue::new(&mut state.depth_bias)
                                .speed(0.01)
                                .range(-10.0..=10.0),
                        )
                        .changed()
                    {
                        state.dirty = true;
                    }
                    ui.end_row();
                });
        });
}

fn draw_selection_tab(
    ui: &mut egui::Ui,
    state: &mut MaterialsWindowState,
    pending: &mut PendingCommandInvocations,
    selected_assignments: &[(u64, Option<MaterialAssignment>)],
    selected_material_id: Option<&str>,
) {
    let selected_count = selected_assignments.len();
    ui.label(
        egui::RichText::new(format!(
            "Selection assignment editor for {} selected entit{}.",
            selected_count,
            if selected_count == 1 { "y" } else { "ies" }
        ))
        .small(),
    );
    if state.selection_assignment_mixed {
        ui.label(
            egui::RichText::new(
                "The selection currently has mixed assignments. Applying here will overwrite them.",
            )
            .italics()
            .weak(),
        );
    } else if selected_count == 0 {
        ui.label(
            egui::RichText::new("Select one or more entities to inspect or edit assignments.")
                .italics()
                .weak(),
        );
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                selected_material_id.is_some(),
                egui::Button::new("Seed Single From Material"),
            )
            .clicked()
        {
            let assignment = MaterialAssignment::new(
                selected_material_id.expect("button is disabled when no material is selected"),
            );
            state.selection_assignment_json =
                serde_json::to_string_pretty(&material_assignment_to_value(&assignment))
                    .unwrap_or_else(|_| "{}".to_string());
            state.selection_status = None;
        }
        if ui.button("New Layer Set").clicked() {
            let assignment =
                MaterialAssignment::LayerSet(crate::plugins::materials::MaterialLayerSet {
                    layers: vec![crate::plugins::materials::MaterialLayer::default()],
                });
            state.selection_assignment_json =
                serde_json::to_string_pretty(&material_assignment_to_value(&assignment))
                    .unwrap_or_else(|_| "{}".to_string());
            state.selection_status = None;
        }
        if ui.button("Reload Selection").clicked() {
            state.selection_assignment_signature = 0;
            sync_selection_assignment_buffer(state, selected_assignments);
        }
        if ui
            .add_enabled(selected_count > 0, egui::Button::new("Clear Selection"))
            .clicked()
        {
            queue_command_invocation_resource(
                pending,
                "materials.clear_assignment_on_selection".to_string(),
                serde_json::json!({}),
            );
            state.selection_status = None;
        }
    });

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Advanced JSON editor. Use `{ \"type\": \"single\", ... }` or `{ \"type\": \"layer_set\", \"layers\": [...] }`.",
        )
        .small()
        .weak(),
    );
    ui.add(
        egui::TextEdit::multiline(&mut state.selection_assignment_json)
            .desired_rows(14)
            .code_editor()
            .hint_text(
                "{\n  \"type\": \"single\",\n  \"render\": \"material_def.v1/oak_finish\"\n}",
            ),
    );

    if let Some(status) = &state.selection_status {
        ui.label(
            egui::RichText::new(status)
                .small()
                .color(egui::Color32::LIGHT_RED),
        );
    }

    if ui
        .add_enabled(selected_count > 0, egui::Button::new("Apply To Selection"))
        .clicked()
    {
        match parse_assignment_json(&state.selection_assignment_json) {
            Ok(value) => {
                queue_command_invocation_resource(
                    pending,
                    "materials.set_assignment_on_selection".to_string(),
                    serde_json::json!({ "assignment": value }),
                );
                state.selection_status = None;
            }
            Err(error) => state.selection_status = Some(error),
        }
    }
}

fn draw_sources_tab(
    ui: &mut egui::Ui,
    state: &mut MaterialsWindowState,
    source_registry: &mut SourceRegistry,
    nomination_queue: &mut NominationQueue,
    selected_material_context: Option<SelectedMaterialContext>,
) {
    ui.label(
        egui::RichText::new(
            "Manage external material sources, project references, and pending import nominations here.",
        )
        .small(),
    );
    if let Some(context) = selected_material_context {
        ui.add_space(4.0);
        ui.group(|ui| {
            ui.label(egui::RichText::new("Material Context").strong());
            ui.label(format!("Selected material: {}", context.name));
            ui.label(format!(
                "Material spec: {}",
                context.spec_ref.unwrap_or_else(|| "(none)".to_string())
            ));
            ui.label(
                egui::RichText::new(
                    "Use source entries to track import provenance and curated upstream references. Rendering parameters still live on MaterialDef.",
                )
                .small()
                .weak(),
            );
        });
    }

    ui.add_space(8.0);
    ui.columns(2, |columns| {
        draw_source_registry_column(&mut columns[0], state, source_registry, nomination_queue);
        draw_source_draft_column(&mut columns[1], state, source_registry, nomination_queue);
    });
}

fn draw_source_registry_column(
    ui: &mut egui::Ui,
    state: &mut MaterialsWindowState,
    source_registry: &mut SourceRegistry,
    nomination_queue: &mut NominationQueue,
) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Source Registry").strong());
            ui.label(
                egui::RichText::new(format!("{} revisions", source_registry.revision_count()))
                    .small()
                    .weak(),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut state.source_search);
            if ui.small_button("New Draft").clicked() {
                state.selected_source_key = None;
                state.source_draft = SourceDraftState::default();
                state.source_status_message = None;
                state.source_status_is_error = false;
            }
        });

        ui.add_space(4.0);
        let search_lower = state.source_search.to_lowercase();
        let mut entries: Vec<&SourceRegistryEntry> = source_registry
            .iter()
            .filter(|entry| source_matches_search(entry, &search_lower))
            .collect();
        entries.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then_with(|| left.source_id.0.cmp(&right.source_id.0))
                .then_with(|| left.revision.0.cmp(&right.revision.0))
        });

        egui::ScrollArea::vertical()
            .id_salt("material_sources_registry")
            .max_height(230.0)
            .show(ui, |ui| {
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new("No sources match the current filter.")
                            .italics()
                            .weak(),
                    );
                }
                for entry in entries {
                    let key = source_key_entry(entry);
                    let selected = state.selected_source_key.as_deref() == Some(key.as_str());
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            let label = format!(
                                "{}\n{} @ {}",
                                entry.title, entry.source_id.0, entry.revision.0
                            );
                            if ui
                                .add_sized(
                                    [ui.available_width() - 44.0, 36.0],
                                    egui::Button::new(label).selected(selected),
                                )
                                .clicked()
                            {
                                state.selected_source_key = Some(key.clone());
                            }
                            if ui.small_button("Load").clicked() {
                                state.selected_source_key = Some(key.clone());
                                state.source_draft.load_from_entry(entry);
                                state.source_status_message =
                                    Some(format!("Loaded {} into the draft editor.", entry.title));
                                state.source_status_is_error = false;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} / {} / {}",
                                    source_tier_label(entry.tier),
                                    source_license_label(entry.license),
                                    source_status_label(&entry.status)
                                ))
                                .small()
                                .weak(),
                            );
                            if let Some(url) = &entry.canonical_url {
                                ui.hyperlink_to("open", url);
                            }
                        });
                    });
                }
            });
    });

    ui.add_space(8.0);
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Pending Imports").strong());
            ui.label(
                egui::RichText::new(format!("{} pending", nomination_queue.len()))
                    .small()
                    .weak(),
            );
        });
        ui.label(
            egui::RichText::new(
                "Queue shared or curated sources here, then approve or reject them from the same panel.",
            )
            .small()
            .weak(),
        );
        ui.add_space(4.0);

        let mut queue_action: Option<(bool, NominationId)> = None;
        egui::ScrollArea::vertical()
            .id_salt("material_source_nominations")
            .max_height(210.0)
            .show(ui, |ui| {
                if nomination_queue.is_empty() {
                    ui.label(
                        egui::RichText::new("No pending source nominations.")
                            .italics()
                            .weak(),
                    );
                }
                for nomination in nomination_queue.list() {
                    ui.group(|ui| {
                        ui.label(
                            egui::RichText::new(source_nomination_title(nomination)).strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} proposed by {}",
                                nomination.id.0, nomination.proposed_by
                            ))
                            .small()
                            .weak(),
                        );
                        if let Some(justification) = &nomination.justification {
                            ui.label(
                                egui::RichText::new(justification)
                                    .small()
                                    .italics()
                                    .weak(),
                            );
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("Approve").clicked() {
                                queue_action = Some((true, nomination.id.clone()));
                            }
                            if ui.small_button("Reject").clicked() {
                                queue_action = Some((false, nomination.id.clone()));
                            }
                        });
                    });
                }
            });

        if let Some((approve, nomination_id)) = queue_action {
            let result = if approve {
                nomination_queue
                    .approve(&nomination_id, source_registry)
                    .map(|_| format!("Approved {}.", nomination_id.0))
                    .map_err(|error| error.to_string())
            } else {
                nomination_queue
                    .reject(&nomination_id, Some("Rejected from material browser".to_string()))
                    .map(|_| format!("Rejected {}.", nomination_id.0))
                    .map_err(|error| error.to_string())
            };
            let is_error = result.is_err();
            state.source_status_message = Some(match result {
                Ok(message) => message,
                Err(message) => format!("Source review failed: {message}"),
            });
            state.source_status_is_error = is_error;
        }
    });
}

fn draw_source_draft_column(
    ui: &mut egui::Ui,
    state: &mut MaterialsWindowState,
    source_registry: &mut SourceRegistry,
    nomination_queue: &mut NominationQueue,
) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Source / Import Draft").strong());
            if ui.small_button("Reset").clicked() {
                state.source_draft = SourceDraftState::default();
                state.source_status_message = None;
                state.source_status_is_error = false;
            }
        });
        ui.label(
            egui::RichText::new(
                "Use this draft for two paths: register a project/ad-hoc source immediately, or queue a higher-trust source for review.",
            )
            .small()
            .weak(),
        );

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Template");
            egui::ComboBox::from_id_salt("source_preset_combo")
                .selected_text(
                    SOURCE_PRESET_NAMES
                        .get(state.source_draft.preset_idx)
                        .copied()
                        .unwrap_or(SOURCE_PRESET_NAMES[0]),
                )
                .show_ui(ui, |ui| {
                    for (idx, label) in SOURCE_PRESET_NAMES.iter().enumerate() {
                        ui.selectable_value(&mut state.source_draft.preset_idx, idx, *label);
                    }
                });
            if ui.small_button("Load Preset").clicked() {
                state.source_draft.apply_preset();
                state.source_status_message = Some(format!(
                    "Loaded the {} source template.",
                    SOURCE_PRESET_NAMES
                        .get(state.source_draft.preset_idx)
                        .copied()
                        .unwrap_or("custom")
                ));
                state.source_status_is_error = false;
            }
        });

        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("material_source_draft")
            .show(ui, |ui| {
                egui::Grid::new("material_source_draft_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Source ID");
                        ui.text_edit_singleline(&mut state.source_draft.source_id);
                        ui.end_row();

                        ui.label("Revision");
                        ui.text_edit_singleline(&mut state.source_draft.revision);
                        ui.end_row();

                        ui.label("Title");
                        ui.text_edit_singleline(&mut state.source_draft.title);
                        ui.end_row();

                        ui.label("Publisher");
                        ui.text_edit_singleline(&mut state.source_draft.publisher);
                        ui.end_row();

                        ui.label("Tier");
                        egui::ComboBox::from_id_salt("material_source_tier")
                            .selected_text(source_tier_name(state.source_draft.tier_idx))
                            .show_ui(ui, |ui| {
                                for (idx, (label, _)) in SOURCE_TIER_OPTIONS.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut state.source_draft.tier_idx,
                                        idx,
                                        *label,
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("License");
                        egui::ComboBox::from_id_salt("material_source_license")
                            .selected_text(source_license_name(state.source_draft.license_idx))
                            .show_ui(ui, |ui| {
                                for (idx, (label, _)) in SOURCE_LICENSE_OPTIONS.iter().enumerate()
                                {
                                    ui.selectable_value(
                                        &mut state.source_draft.license_idx,
                                        idx,
                                        *label,
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Jurisdiction");
                        ui.text_edit_singleline(&mut state.source_draft.jurisdiction);
                        ui.end_row();

                        ui.label("Canonical URL");
                        ui.text_edit_singleline(&mut state.source_draft.canonical_url);
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.label("Metadata JSON");
                ui.add(
                    egui::TextEdit::multiline(&mut state.source_draft.metadata_json)
                        .desired_rows(7)
                        .code_editor()
                        .hint_text("{\n  \"provider\": \"ambientcg\",\n  \"asset_id\": \"\"\n}"),
                );

                ui.add_space(6.0);
                ui.label("Justification / import notes");
                ui.add(
                    egui::TextEdit::multiline(&mut state.source_draft.justification)
                        .desired_rows(4)
                        .hint_text(
                            "Why this source should be imported, promoted, or preserved in the project.",
                        ),
                );

                let tier = source_tier_by_idx(state.source_draft.tier_idx);
                let can_register_directly = matches!(tier, SourceTier::Project | SourceTier::AdHoc);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let register_response = ui.add_enabled(
                        can_register_directly,
                        egui::Button::new("Register Project Source"),
                    );
                    if register_response.clicked() {
                        match state.source_draft.build_entry() {
                            Ok(entry) => {
                                let key = source_key_entry(&entry);
                                source_registry.insert(entry);
                                state.selected_source_key = Some(key);
                                state.source_status_message = Some(
                                    "Registered the source directly in the project registry."
                                        .to_string(),
                                );
                                state.source_status_is_error = false;
                            }
                            Err(error) => {
                                state.source_status_message = Some(error);
                                state.source_status_is_error = true;
                            }
                        }
                    }
                    register_response.on_hover_text(
                        "Direct registration is limited to project/ad-hoc sources. Shared or promoted sources should go through review.",
                    );

                    if ui.button("Queue Review").clicked() {
                        match state.source_draft.build_entry() {
                            Ok(entry) => {
                                let justification = trimmed_or_none(&state.source_draft.justification);
                                let nomination_id = nomination_queue.push(
                                    NominationKind::AddSource { entry },
                                    "user:material_browser",
                                    current_unix_seconds(),
                                    justification,
                                );
                                state.source_status_message = Some(format!(
                                    "Queued {} for review.",
                                    nomination_id.0
                                ));
                                state.source_status_is_error = false;
                            }
                            Err(error) => {
                                state.source_status_message = Some(error);
                                state.source_status_is_error = true;
                            }
                        }
                    }
                });

                ui.add_space(4.0);
                if let Some(key) = state.source_draft.key() {
                    if let Some(selected) = state.selected_source_key.as_deref() {
                        if selected == key {
                            ui.label(
                                egui::RichText::new(
                                    "The draft matches the currently selected registry entry.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                    }
                }
                if let Some(message) = &state.source_status_message {
                    ui.label(
                        egui::RichText::new(message)
                            .small()
                            .color(if state.source_status_is_error {
                                egui::Color32::LIGHT_RED
                            } else {
                                egui::Color32::LIGHT_BLUE
                            }),
                    );
                }
            });
    });
}

fn sync_selection_assignment_buffer(
    state: &mut MaterialsWindowState,
    selected_assignments: &[(u64, Option<MaterialAssignment>)],
) {
    let signature = selection_assignment_signature(selected_assignments);
    if signature == state.selection_assignment_signature {
        return;
    }

    state.selection_assignment_signature = signature;
    state.selection_assignment_mixed = false;
    state.selection_status = None;

    let Some((_, first_assignment)) = selected_assignments.first() else {
        state.selection_assignment_json.clear();
        return;
    };

    if selected_assignments
        .iter()
        .all(|(_, assignment)| assignment == first_assignment)
    {
        state.selection_assignment_json = first_assignment
            .as_ref()
            .map(|assignment| {
                serde_json::to_string_pretty(&material_assignment_to_value(assignment))
                    .unwrap_or_else(|_| "{}".to_string())
            })
            .unwrap_or_default();
    } else {
        state.selection_assignment_mixed = true;
        state.selection_assignment_json.clear();
    }
}

fn selection_assignment_signature(
    selected_assignments: &[(u64, Option<MaterialAssignment>)],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    selected_assignments.len().hash(&mut hasher);
    for (element_id, assignment) in selected_assignments {
        element_id.hash(&mut hasher);
        assignment
            .as_ref()
            .map(|assignment| material_assignment_to_value(assignment).to_string())
            .unwrap_or_else(|| "null".to_string())
            .hash(&mut hasher);
    }
    hasher.finish()
}

fn parse_assignment_json(json: &str) -> Result<serde_json::Value, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err(
            "Assignment JSON is empty. Use Clear Selection to remove assignments.".to_string(),
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|error| format!("Invalid JSON: {error}"))?;
    material_assignment_from_value(&value)
        .ok_or_else(|| "JSON did not decode into a MaterialAssignment".to_string())?;
    Ok(value)
}

fn source_matches_search(entry: &SourceRegistryEntry, search_lower: &str) -> bool {
    if search_lower.is_empty() {
        return true;
    }
    entry.title.to_lowercase().contains(search_lower)
        || entry.publisher.to_lowercase().contains(search_lower)
        || entry.source_id.0.to_lowercase().contains(search_lower)
        || entry.revision.0.to_lowercase().contains(search_lower)
        || entry
            .canonical_url
            .as_deref()
            .map(|url| url.to_lowercase().contains(search_lower))
            .unwrap_or(false)
}

fn source_key_entry(entry: &SourceRegistryEntry) -> String {
    source_key_parts(&entry.source_id.0, &entry.revision.0)
}

fn source_key_parts(source_id: &str, revision: &str) -> String {
    format!("{source_id}@{revision}")
}

fn source_tier_by_idx(idx: usize) -> SourceTier {
    SOURCE_TIER_OPTIONS
        .get(idx)
        .map(|(_, tier)| *tier)
        .unwrap_or(SourceTier::Project)
}

fn source_tier_name(idx: usize) -> &'static str {
    SOURCE_TIER_OPTIONS
        .get(idx)
        .map(|(label, _)| *label)
        .unwrap_or(SOURCE_TIER_OPTIONS[0].0)
}

fn source_tier_to_idx(tier: SourceTier) -> usize {
    SOURCE_TIER_OPTIONS
        .iter()
        .position(|(_, candidate)| *candidate == tier)
        .unwrap_or(0)
}

fn source_license_by_idx(idx: usize) -> SourceLicense {
    SOURCE_LICENSE_OPTIONS
        .get(idx)
        .map(|(_, license)| *license)
        .unwrap_or(SourceLicense::UserAttachedPrivate)
}

fn source_license_name(idx: usize) -> &'static str {
    SOURCE_LICENSE_OPTIONS
        .get(idx)
        .map(|(label, _)| *label)
        .unwrap_or(SOURCE_LICENSE_OPTIONS[0].0)
}

fn source_license_to_idx(license: SourceLicense) -> usize {
    SOURCE_LICENSE_OPTIONS
        .iter()
        .position(|(_, candidate)| *candidate == license)
        .unwrap_or(0)
}

fn source_tier_label(tier: SourceTier) -> &'static str {
    SOURCE_TIER_OPTIONS
        .iter()
        .find_map(|(label, candidate)| (*candidate == tier).then_some(*label))
        .unwrap_or("unknown")
}

fn source_license_label(license: SourceLicense) -> &'static str {
    SOURCE_LICENSE_OPTIONS
        .iter()
        .find_map(|(label, candidate)| (*candidate == license).then_some(*label))
        .unwrap_or("unknown")
}

fn source_status_label(status: &crate::curation::SourceStatus) -> &'static str {
    match status {
        crate::curation::SourceStatus::Active => "active",
        crate::curation::SourceStatus::Superseded { .. } => "superseded",
        crate::curation::SourceStatus::Sunset { .. } => "sunset",
    }
}

fn source_nomination_title(nomination: &crate::curation::Nomination) -> String {
    match &nomination.kind {
        NominationKind::AddSource { entry } => {
            format!("Add {} @ {}", entry.source_id.0, entry.revision.0)
        }
        NominationKind::SunsetSource {
            source_id,
            revision,
            replacement,
            ..
        } => format!(
            "Sunset {} @ {}{}",
            source_id.0,
            revision.0,
            replacement
                .as_ref()
                .map(|replacement| format!(" -> {}", replacement.0))
                .unwrap_or_default()
        ),
    }
}

fn trimmed_or_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn draw_textures_tab(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    state: &mut MaterialsWindowState,
    texture_registry: &TextureRegistry,
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
) {
    egui::ScrollArea::vertical()
        .id_salt("mat_textures")
        .show(ui, |ui| {
            egui::Grid::new("mat_tex_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    texture_row(
                        ui,
                        contexts,
                        &mut state.preview_texture_handles,
                        asset_server,
                        texture_registry,
                        images,
                        "Base Color",
                        &mut state.base_color_tex,
                        &mut state.dirty,
                    );
                    texture_row(
                        ui,
                        contexts,
                        &mut state.preview_texture_handles,
                        asset_server,
                        texture_registry,
                        images,
                        "Normal Map",
                        &mut state.normal_map_tex,
                        &mut state.dirty,
                    );
                    texture_row(
                        ui,
                        contexts,
                        &mut state.preview_texture_handles,
                        asset_server,
                        texture_registry,
                        images,
                        "Metallic/Roughness",
                        &mut state.metallic_roughness_tex,
                        &mut state.dirty,
                    );
                    texture_row(
                        ui,
                        contexts,
                        &mut state.preview_texture_handles,
                        asset_server,
                        texture_registry,
                        images,
                        "Emissive",
                        &mut state.emissive_tex,
                        &mut state.dirty,
                    );
                    texture_row(
                        ui,
                        contexts,
                        &mut state.preview_texture_handles,
                        asset_server,
                        texture_registry,
                        images,
                        "Occlusion",
                        &mut state.occlusion_tex,
                        &mut state.dirty,
                    );
                });
        });
}

/// Render a single texture slot row: label, current texture name, Upload and
/// Clear buttons.  Modifies `slot` and `dirty` in place.
fn texture_row(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    preview_texture_handles: &mut HashMap<String, Handle<Image>>,
    asset_server: &AssetServer,
    texture_registry: &TextureRegistry,
    images: &mut Assets<Image>,
    label: &str,
    slot: &mut Option<TextureRef>,
    dirty: &mut bool,
) {
    ui.label(label);
    ui.horizontal(|ui| {
        if let Some(texture) = slot.as_ref() {
            draw_texture_thumbnail(
                ui,
                contexts,
                preview_texture_handles,
                asset_server,
                texture_registry,
                images,
                Some(texture),
                TEXTURE_SLOT_THUMBNAIL_SIZE,
                egui::Color32::from_gray(52),
            );
        } else {
            draw_placeholder_thumbnail(
                ui,
                egui::vec2(TEXTURE_SLOT_THUMBNAIL_SIZE, TEXTURE_SLOT_THUMBNAIL_SIZE),
                egui::Color32::from_gray(52),
                egui::Color32::from_gray(110),
                "No\nTexture",
            );
        }

        // Show current texture label
        let current = slot
            .as_ref()
            .map(|t| t.label(Some(texture_registry)).to_string())
            .unwrap_or_else(|| "(none)".to_string());
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(&current).weak());

            if ui.small_button("Upload").clicked() {
                if let Some(tex) = pick_texture_file() {
                    *slot = Some(tex);
                    *dirty = true;
                }
            }
            if slot.is_some() && ui.small_button("✕").clicked() {
                *slot = None;
                *dirty = true;
            }
        });
    });
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::{SourceLicense, SourceTier};

    #[test]
    fn source_draft_builds_entry_with_optional_fields() {
        let draft = SourceDraftState {
            source_id: "poly_haven.material.brick_wall".to_string(),
            revision: "2026-04-20".to_string(),
            title: "Poly Haven Brick Wall".to_string(),
            publisher: "Poly Haven".to_string(),
            tier_idx: source_tier_to_idx(SourceTier::AdHoc),
            license_idx: source_license_to_idx(SourceLicense::PublicDomain),
            jurisdiction: "SE".to_string(),
            canonical_url: "https://polyhaven.com/a/brick_wall".to_string(),
            metadata_json: "{ \"provider\": \"poly_haven\", \"asset_slug\": \"brick_wall\" }"
                .to_string(),
            ..Default::default()
        };

        let entry = draft.build_entry().expect("draft should be valid");
        assert_eq!(entry.source_id.0, "poly_haven.material.brick_wall");
        assert_eq!(entry.revision.0, "2026-04-20");
        assert_eq!(entry.tier, SourceTier::AdHoc);
        assert_eq!(entry.license, SourceLicense::PublicDomain);
        assert_eq!(
            entry.jurisdiction.as_ref().map(|j| j.0.as_str()),
            Some("SE")
        );
        assert_eq!(
            entry.canonical_url.as_deref(),
            Some("https://polyhaven.com/a/brick_wall")
        );
        assert_eq!(entry.metadata["provider"], "poly_haven");
    }

    #[test]
    fn ambientcg_preset_sets_bootstrap_defaults() {
        let mut draft = SourceDraftState {
            preset_idx: 1,
            ..Default::default()
        };
        draft.apply_preset();

        assert_eq!(draft.publisher, "ambientCG");
        assert_eq!(source_tier_by_idx(draft.tier_idx), SourceTier::AdHoc);
        assert_eq!(
            source_license_by_idx(draft.license_idx),
            SourceLicense::PublicDomain
        );
        assert!(draft.canonical_url.contains("ambientcg.com"));
        assert!(draft.metadata_json.contains("\"provider\": \"ambientcg\""));
    }
}

fn draw_material_list_thumbnail(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    preview_texture_handles: &mut HashMap<String, Handle<Image>>,
    asset_server: &AssetServer,
    texture_registry: &TextureRegistry,
    images: &mut Assets<Image>,
    def: &MaterialDef,
) {
    let preview_texture = def
        .base_color_texture
        .as_ref()
        .or(def.emissive_texture.as_ref())
        .or(def.metallic_roughness_texture.as_ref())
        .or(def.normal_map_texture.as_ref())
        .or(def.occlusion_texture.as_ref());
    draw_texture_thumbnail(
        ui,
        contexts,
        preview_texture_handles,
        asset_server,
        texture_registry,
        images,
        preview_texture,
        MATERIAL_LIST_THUMBNAIL_SIZE,
        material_swatch_color(def),
    );
}

fn draw_texture_thumbnail(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    preview_texture_handles: &mut HashMap<String, Handle<Image>>,
    asset_server: &AssetServer,
    texture_registry: &TextureRegistry,
    images: &mut Assets<Image>,
    texture: Option<&TextureRef>,
    size: f32,
    fallback_fill: egui::Color32,
) {
    if let Some(texture) = texture {
        if let Some(handle) = cached_texture_handle(
            preview_texture_handles,
            asset_server,
            texture_registry,
            images,
            texture,
        ) {
            let image = egui_texture_image(contexts, &handle, egui::vec2(size, size));
            ui.add(image);
            return;
        }
    }

    draw_placeholder_thumbnail(
        ui,
        egui::vec2(size, size),
        fallback_fill,
        egui::Color32::from_gray(86),
        "",
    );
}

fn draw_placeholder_thumbnail(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    fill: egui::Color32,
    stroke_color: egui::Color32,
    label: &str,
) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 6.0, fill);
    ui.painter().rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, stroke_color),
        egui::StrokeKind::Inside,
    );
    if !label.is_empty() {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::TextStyle::Small.resolve(ui.style()),
            egui::Color32::from_gray(220),
        );
    }
}

fn egui_texture_image<'a>(
    contexts: &'a mut EguiContexts,
    handle: &Handle<Image>,
    size: egui::Vec2,
) -> egui::Image<'a> {
    let texture_id = contexts
        .image_id(handle)
        .unwrap_or_else(|| contexts.add_image(EguiTextureHandle::Weak(handle.id())));
    egui::Image::new((texture_id, size)).corner_radius(6.0)
}

fn cached_texture_handle(
    preview_texture_handles: &mut HashMap<String, Handle<Image>>,
    asset_server: &AssetServer,
    texture_registry: &TextureRegistry,
    images: &mut Assets<Image>,
    texture: &TextureRef,
) -> Option<Handle<Image>> {
    let key = texture_preview_key(texture, texture_registry);
    if let Some(handle) = preview_texture_handles.get(&key) {
        return Some(handle.clone());
    }
    let handle = texture.load(asset_server, images, Some(texture_registry))?;
    preview_texture_handles.insert(key, handle.clone());
    Some(handle)
}

fn texture_preview_key(texture: &TextureRef, _texture_registry: &TextureRegistry) -> String {
    match texture {
        TextureRef::TextureAsset { id } => format!("texture_asset:{}", id.as_str()),
        TextureRef::AssetPath(path) => format!("asset:{path}"),
        TextureRef::Embedded { data, mime } => {
            let mut hasher = DefaultHasher::new();
            data.hash(&mut hasher);
            mime.hash(&mut hasher);
            format!("embedded:{mime}:{:x}", hasher.finish())
        }
    }
}

fn material_swatch_color(def: &MaterialDef) -> egui::Color32 {
    let [r, g, b, a] = def.base_color;
    egui::Color32::from_rgba_premultiplied(
        (r.clamp(0.0, 1.0) * 255.0) as u8,
        (g.clamp(0.0, 1.0) * 255.0) as u8,
        (b.clamp(0.0, 1.0) * 255.0) as u8,
        (a.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn material_meta_label(def: &MaterialDef) -> String {
    let texture_count = [
        def.base_color_texture.as_ref(),
        def.normal_map_texture.as_ref(),
        def.metallic_roughness_texture.as_ref(),
        def.emissive_texture.as_ref(),
        def.occlusion_texture.as_ref(),
    ]
    .into_iter()
    .flatten()
    .count();

    if texture_count > 0 {
        format!("{} · {texture_count} tex", def.summary())
    } else {
        def.summary()
    }
}

/// Open a native file picker and return a `TextureRef::Embedded` for the
/// chosen image file, or `None` if the user cancelled or an error occurred.
fn pick_texture_file() -> Option<TextureRef> {
    use base64::prelude::*;

    let path = rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
        .pick_file()?;

    let bytes = std::fs::read(&path).ok()?;
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };

    Some(TextureRef::Embedded {
        data: BASE64_STANDARD.encode(&bytes),
        mime: mime.to_string(),
    })
}

// ─── Alpha mode helpers ───────────────────────────────────────────────────────

const ALPHA_MODE_NAMES: &[&str] = &["Opaque", "Mask", "Blend", "Premultiplied", "Add"];

fn alpha_mode_name(idx: usize) -> &'static str {
    ALPHA_MODE_NAMES.get(idx).copied().unwrap_or("Opaque")
}

fn alpha_mode_to_idx(mode: &MaterialAlphaMode) -> usize {
    match mode {
        MaterialAlphaMode::Opaque => 0,
        MaterialAlphaMode::Mask => 1,
        MaterialAlphaMode::Blend => 2,
        MaterialAlphaMode::Premultiplied => 3,
        MaterialAlphaMode::Add => 4,
    }
}

fn idx_to_alpha_mode(idx: usize) -> MaterialAlphaMode {
    match idx {
        1 => MaterialAlphaMode::Mask,
        2 => MaterialAlphaMode::Blend,
        3 => MaterialAlphaMode::Premultiplied,
        4 => MaterialAlphaMode::Add,
        _ => MaterialAlphaMode::Opaque,
    }
}
