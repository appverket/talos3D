/// Material browser and editor panel.
///
/// Follows the same floating-window pattern as `definition_browser`.
/// Exposed via `draw_materials_window` which is called from `egui_chrome`.
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use bevy::{
    asset::AssetServer,
    prelude::{Assets, Handle, Image},
};
use bevy_egui::{egui, EguiContexts, EguiTextureHandle};

use crate::plugins::{
    command_registry::{queue_command_invocation_resource, PendingCommandInvocations},
    materials::{
        normalize_material_textures, MaterialAlphaMode, MaterialDef, MaterialRegistry, TextureRef,
        TextureRegistry,
    },
    ui::{tool_window_max_size, tool_window_rect},
};

const MATERIALS_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(480.0, 560.0);
const MATERIALS_WINDOW_MIN_SIZE: egui::Vec2 = egui::vec2(340.0, 280.0);
const MATERIALS_WINDOW_MAX_SIZE: egui::Vec2 = egui::vec2(640.0, 760.0);
const MATERIAL_LIST_THUMBNAIL_SIZE: f32 = 30.0;
const TEXTURE_SLOT_THUMBNAIL_SIZE: f32 = 52.0;

// ─── Window state ─────────────────────────────────────────────────────────────

#[derive(bevy::prelude::Resource, Default, Debug, Clone)]
pub struct MaterialsWindowState {
    pub visible: bool,
    pub search: String,
    pub selected_id: Option<String>,
    // Inline editor buffers (so we don't mutate the registry on every keypress)
    pub name_buf: String,
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
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum EditorTab {
    #[default]
    Properties,
    Textures,
}

impl MaterialsWindowState {
    /// Load editor buffers from a `MaterialDef`.
    pub fn load_def(&mut self, def: &MaterialDef) {
        self.name_buf = def.name.clone();
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
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    pending: &mut PendingCommandInvocations,
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
                asset_server,
                images,
                pending,
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
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    pending: &mut PendingCommandInvocations,
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

            if let Some(selected_id) = state.selected_id.clone() {
                if registry.contains(&selected_id) {
                    draw_editor(
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

// ─── Editor panel ─────────────────────────────────────────────────────────────

fn draw_editor(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    state: &mut MaterialsWindowState,
    registry: &mut MaterialRegistry,
    texture_registry: &mut TextureRegistry,
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    id: &str,
) {
    // Tab bar
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
    });
    ui.separator();

    match state.editor_tab {
        EditorTab::Properties => draw_properties_tab(ui, state),
        EditorTab::Textures => {
            draw_textures_tab(ui, contexts, state, texture_registry, asset_server, images)
        }
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
