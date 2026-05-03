/// Real-time render pipeline configuration for Talos3D.
///
/// Adds SSAO, Bloom, optional SSR, and AgX tonemapping to the scene camera.
/// Exposes [`RenderSettings`] as a hot-reloadable resource: toggling any flag
/// takes effect on the very next frame.
use bevy::{
    anti_alias::fxaa::Fxaa,
    camera::Exposure,
    core_pipeline::tonemapping::Tonemapping,
    mesh::{Indices, VertexAttributeValues},
    pbr::{
        ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel,
        ScreenSpaceReflections,
    },
    post_process::bloom::Bloom,
    prelude::*,
};
use bevy_egui::{egui, EguiContexts};
use serde_json::Value;
use std::collections::HashMap;

use crate::{
    capability_registry::CapabilityRegistry,
    plugins::{
        camera::OrbitCamera,
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        identity::ElementId,
        modeling::{mesh_generation::MeshGenerationSet, snapshots::ray_triangle_intersection},
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
    },
};

const WIREFRAME_OVERLAY_COLOR: Color = Color::srgba(0.12, 0.13, 0.15, 1.0);
const CONTOUR_OVERLAY_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 1.0);
const VISIBLE_EDGE_OVERLAY_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 1.0);
const EDGE_QUANTIZATION_SCALE: f32 = 10_000.0;
const DEFAULT_BACKGROUND_RGB: [f32; 3] = [0.17, 0.18, 0.20];
const FEATURE_EDGE_COS_THRESHOLD: f32 = 0.85;
const VISIBLE_EDGE_SEGMENT_STEPS: usize = 24;
const EDGE_VISIBILITY_EPSILON: f32 = 0.01;
const ORTHOGRAPHIC_VISIBILITY_RAY_LENGTH: f32 = 10_000.0;
pub const VIEW_RENDER_TOOLBAR_ID: &str = "view.render";
const PAPER_BACKGROUND_RGB: [f32; 3] = [1.0, 1.0, 1.0];
const PAPER_MM_PER_WORLD_M: f32 = 20.0;

// ─── Settings resource ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTonemapping {
    None,
    Reinhard,
    ReinhardLuminance,
    AcesFitted,
    AgX,
    SomewhatBoringDisplayTransform,
    TonyMcMapface,
    BlenderFilmic,
}

impl Default for RenderTonemapping {
    fn default() -> Self {
        Self::AgX
    }
}

impl RenderTonemapping {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Reinhard => "reinhard",
            Self::ReinhardLuminance => "reinhard_luminance",
            Self::AcesFitted => "aces_fitted",
            Self::AgX => "agx",
            Self::SomewhatBoringDisplayTransform => "somewhat_boring_display_transform",
            Self::TonyMcMapface => "tony_mc_mapface",
            Self::BlenderFilmic => "blender_filmic",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "reinhard" => Some(Self::Reinhard),
            "reinhard_luminance" | "reinhardluminance" => Some(Self::ReinhardLuminance),
            "aces_fitted" | "acesfitted" => Some(Self::AcesFitted),
            "agx" => Some(Self::AgX),
            "somewhat_boring_display_transform"
            | "somewhatboringdisplaytransform"
            | "somewhat_boring" => Some(Self::SomewhatBoringDisplayTransform),
            "tony_mc_mapface" | "tonymcmapface" => Some(Self::TonyMcMapface),
            "blender_filmic" | "blenderfilmic" => Some(Self::BlenderFilmic),
            _ => None,
        }
    }

    fn to_bevy(self) -> Tonemapping {
        match self {
            Self::None => Tonemapping::None,
            Self::Reinhard => Tonemapping::Reinhard,
            Self::ReinhardLuminance => Tonemapping::ReinhardLuminance,
            Self::AcesFitted => Tonemapping::AcesFitted,
            Self::AgX => Tonemapping::AgX,
            Self::SomewhatBoringDisplayTransform => Tonemapping::SomewhatBoringDisplayTransform,
            Self::TonyMcMapface => Tonemapping::TonyMcMapface,
            Self::BlenderFilmic => Tonemapping::BlenderFilmic,
        }
    }
}

/// Hot-reloadable render quality settings.
///
/// Insert or mutate this resource to toggle effects at runtime.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct RenderSettings {
    /// Tonemapper applied to the main 3D view.
    pub tonemapping: RenderTonemapping,
    /// Manual camera exposure in EV100.
    pub exposure_ev100: f32,
    /// Enable screen-space ambient occlusion.
    pub ssao_enabled: bool,
    /// SSAO thickness heuristic in metres.
    pub ssao_constant_object_thickness: f32,
    /// Enable bloom post-processing.
    pub bloom_enabled: bool,
    /// Bloom intensity (linear scale applied on top of `Bloom::default()`).
    pub bloom_intensity: f32,
    /// Extra boost for low-frequency bloom.
    pub bloom_low_frequency_boost: f32,
    /// Curve shaping for low-frequency bloom boost.
    pub bloom_low_frequency_boost_curvature: f32,
    /// Controls how tightly bloom scatters.
    pub bloom_high_pass_frequency: f32,
    /// Bloom prefilter threshold.
    pub bloom_threshold: f32,
    /// Bloom prefilter softness.
    pub bloom_threshold_softness: f32,
    /// Bloom stretch per axis for anamorphic looks.
    pub bloom_scale: [f32; 2],
    /// Enable screen-space reflections (requires deferred prepass; GPU-heavy).
    pub ssr_enabled: bool,
    /// SSAO quality: 0 = Low, 1 = Medium, 2 = High.
    pub ambient_occlusion_quality: u8,
    /// Maximum roughness that still receives SSR.
    pub ssr_perceptual_roughness_threshold: f32,
    /// SSR thickness heuristic.
    pub ssr_thickness: f32,
    /// SSR linear march steps.
    pub ssr_linear_steps: u32,
    /// SSR step distribution exponent.
    pub ssr_linear_march_exponent: f32,
    /// SSR refinement steps.
    pub ssr_bisection_steps: u32,
    /// Whether SSR uses secant refinement.
    pub ssr_use_secant: bool,
    /// Draw authored edge linework on top of the shaded model.
    pub wireframe_overlay_enabled: bool,
    /// Draw silhouette / contour edges against the active camera.
    pub contour_overlay_enabled: bool,
    /// Draw visible sharp and silhouette edges with hidden-line removal.
    pub visible_edge_overlay_enabled: bool,
    /// Whether the construction grid is visible.
    pub grid_enabled: bool,
    /// Background color used for the 3D viewport and exported drawing views.
    pub background_rgb: [f32; 3],
    /// Swap scene materials for white unlit fill so drawing edges read cleanly.
    pub paper_fill_enabled: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            tonemapping: RenderTonemapping::AgX,
            exposure_ev100: Exposure::EV100_BLENDER,
            ssao_enabled: true,
            ssao_constant_object_thickness: 0.1,
            bloom_enabled: true,
            bloom_intensity: 0.15,
            bloom_low_frequency_boost: 0.7,
            bloom_low_frequency_boost_curvature: 0.95,
            bloom_high_pass_frequency: 1.0,
            bloom_threshold: 0.0,
            bloom_threshold_softness: 0.0,
            bloom_scale: [1.0, 1.0],
            ssr_enabled: false,
            ambient_occlusion_quality: 2,
            ssr_perceptual_roughness_threshold: 0.4,
            ssr_thickness: 0.25,
            ssr_linear_steps: 12,
            ssr_linear_march_exponent: 1.0,
            ssr_bisection_steps: 4,
            ssr_use_secant: true,
            wireframe_overlay_enabled: false,
            contour_overlay_enabled: false,
            visible_edge_overlay_enabled: false,
            grid_enabled: true,
            background_rgb: DEFAULT_BACKGROUND_RGB,
            paper_fill_enabled: false,
        }
    }
}

#[derive(Component, Debug, Clone)]
struct PaperFillMaterialOverride {
    original: Handle<StandardMaterial>,
    override_handle: Handle<StandardMaterial>,
}

#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct PaperDrawingState {
    baseline: Option<RenderSettings>,
}

// ─── Plugin ──────────────────────────────────────────────────────────────────

/// Registers [`RenderSettings`] and wires up the camera post-processing stack.
pub struct RenderPipelinePlugin;

impl Plugin for RenderPipelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderSettings>()
            .init_resource::<PaperDrawingState>()
            .register_toolbar(ToolbarDescriptor {
                id: VIEW_RENDER_TOOLBAR_ID.to_string(),
                label: "Render".to_string(),
                default_dock: ToolbarDock::Top,
                default_visible: true,
                sections: vec![ToolbarSection {
                    label: "Drawing".to_string(),
                    command_ids: vec![
                        "view.apply_paper_preset".to_string(),
                        "view.toggle_grid".to_string(),
                        "view.toggle_outline".to_string(),
                        "view.toggle_wireframe".to_string(),
                    ],
                }],
            })
            .register_command(
                CommandDescriptor {
                    id: "view.apply_paper_preset".to_string(),
                    label: "Paper Drawing".to_string(),
                    description: "Toggle the paper drawing presentation mode.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_paper".to_string()),
                    hint: Some(
                        "Toggle white paper drawing mode with reversible renderer state"
                            .to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_apply_paper_preset,
            )
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_grid".to_string(),
                    label: "Toggle Grid".to_string(),
                    description: "Show or hide the modeling grid.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_grid".to_string()),
                    hint: Some("Show or hide the modeling grid".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_grid,
            )
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_outline".to_string(),
                    label: "Toggle Outline".to_string(),
                    description: "Toggle visible-edge outline rendering.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_outline".to_string()),
                    hint: Some("Toggle hidden-line-friendly outline rendering".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_outline,
            )
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_wireframe".to_string(),
                    label: "Toggle Wireframe".to_string(),
                    description: "Toggle full wireframe overlay rendering.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_wireframe".to_string()),
                    hint: Some("Toggle full wireframe overlay rendering".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_wireframe,
            )
            // PostStartup ensures the camera plugin has already run Startup.
            .add_systems(PostStartup, setup_render_pipeline)
            .add_systems(
                Update,
                (
                    sync_render_settings,
                    sync_clear_color,
                    sync_paper_fill_materials,
                ),
            )
            .add_systems(
                Update,
                (
                    draw_model_edge_overlays.after(MeshGenerationSet::Generate),
                    draw_silhouette_edge_overlay.after(MeshGenerationSet::Generate),
                    draw_section_fill_overlays.after(MeshGenerationSet::Generate),
                ),
            );
    }
}

fn execute_apply_paper_preset(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    if !world.contains_resource::<RenderSettings>() {
        return Err("Render settings are unavailable".to_string());
    }
    if !world.contains_resource::<PaperDrawingState>() {
        return Err("Paper drawing state is unavailable".to_string());
    }
    let message = world.resource_scope(|world, mut settings: Mut<RenderSettings>| {
        let mut paper_state = world.resource_mut::<PaperDrawingState>();
        if toggle_paper_drawing_mode(&mut settings, &mut paper_state) {
            "Paper drawing enabled".to_string()
        } else {
            "Paper drawing disabled".to_string()
        }
    });

    set_render_feedback(world, &message);
    Ok(CommandResult::empty())
}

fn execute_toggle_grid(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    update_render_settings(world, "", |settings| {
        settings.grid_enabled = !settings.grid_enabled;
    })
}

fn execute_toggle_outline(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    update_render_settings(world, "", |settings| {
        settings.visible_edge_overlay_enabled = !settings.visible_edge_overlay_enabled;
    })
}

fn execute_toggle_wireframe(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    update_render_settings(world, "", |settings| {
        settings.wireframe_overlay_enabled = !settings.wireframe_overlay_enabled;
    })
}

fn update_render_settings(
    world: &mut World,
    feedback: &str,
    apply: impl FnOnce(&mut RenderSettings),
) -> Result<CommandResult, String> {
    let message = {
        let mut settings = world
            .get_resource_mut::<RenderSettings>()
            .ok_or_else(|| "Render settings are unavailable".to_string())?;
        apply(&mut settings);

        if feedback.is_empty() {
            format!(
                "Grid {} · Outline {} · Wireframe {}",
                on_off(settings.grid_enabled),
                on_off(settings.visible_edge_overlay_enabled),
                on_off(settings.wireframe_overlay_enabled)
            )
        } else {
            feedback.to_string()
        }
    };

    set_render_feedback(world, &message);

    Ok(CommandResult::empty())
}

fn set_render_feedback(world: &mut World, feedback: &str) {
    if let Some(mut status_bar_data) = world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
    {
        status_bar_data.set_feedback(feedback.to_string(), 2.0);
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

pub(crate) fn apply_paper_drawing_preset(settings: &mut RenderSettings) {
    settings.tonemapping = RenderTonemapping::None;
    settings.ssao_enabled = false;
    settings.bloom_enabled = false;
    settings.ssr_enabled = false;
    settings.background_rgb = PAPER_BACKGROUND_RGB;
    settings.grid_enabled = false;
    settings.paper_fill_enabled = true;
    settings.visible_edge_overlay_enabled = true;
    settings.contour_overlay_enabled = false;
    settings.wireframe_overlay_enabled = false;
}

pub(crate) fn paper_drawing_active(settings: &RenderSettings) -> bool {
    settings.paper_fill_enabled
}

pub(crate) fn paper_drawing_toggle_active(paper_state: &PaperDrawingState) -> bool {
    paper_state.baseline.is_some()
}

pub(crate) fn toggle_paper_drawing_mode(
    settings: &mut RenderSettings,
    paper_state: &mut PaperDrawingState,
) -> bool {
    if paper_drawing_toggle_active(paper_state) {
        *settings = paper_state.baseline.take().unwrap_or_default();
        return false;
    }

    paper_state.baseline = Some(settings.clone());
    apply_paper_drawing_preset(settings);
    true
}

// ─── Startup system ──────────────────────────────────────────────────────────

fn setup_render_pipeline(
    mut commands: Commands,
    settings: Res<RenderSettings>,
    camera_query: Query<Entity, With<Camera3d>>,
    mut clear_color: ResMut<ClearColor>,
) {
    let Ok(camera) = camera_query.single() else {
        warn!("RenderPipelinePlugin: no Camera3d found at startup");
        return;
    };

    // SSAO requires MSAA disabled — set per-camera.
    commands.entity(camera).insert(Msaa::Off);

    commands
        .entity(camera)
        .insert(Fxaa::default())
        .insert(settings.tonemapping.to_bevy())
        .insert(Exposure {
            ev100: settings.exposure_ev100,
        });
    *clear_color = ClearColor(Color::srgb(
        settings.background_rgb[0],
        settings.background_rgb[1],
        settings.background_rgb[2],
    ));

    sync_ssao_component(&mut commands, camera, &settings);
    sync_bloom_component(&mut commands, camera, &settings);
    sync_ssr_component(&mut commands, camera, &settings);
}

// ─── Hot-reload system ───────────────────────────────────────────────────────

/// Watches [`RenderSettings`] for changes and synchronises camera components.
fn sync_render_settings(
    mut commands: Commands,
    settings: Res<RenderSettings>,
    camera_query: Query<Entity, With<Camera3d>>,
) {
    if !settings.is_changed() {
        return;
    }

    let Ok(camera) = camera_query.single() else {
        return;
    };

    commands
        .entity(camera)
        .insert(Fxaa::default())
        .insert(settings.tonemapping.to_bevy())
        .insert(Exposure {
            ev100: settings.exposure_ev100,
        });

    sync_ssao_component(&mut commands, camera, &settings);
    sync_bloom_component(&mut commands, camera, &settings);
    sync_ssr_component(&mut commands, camera, &settings);
}

fn sync_clear_color(settings: Res<RenderSettings>, mut clear_color: ResMut<ClearColor>) {
    let target = Color::srgb(
        settings.background_rgb[0],
        settings.background_rgb[1],
        settings.background_rgb[2],
    );
    if clear_color.0 != target {
        *clear_color = ClearColor(target);
    }
}

fn sync_paper_fill_materials(
    settings: Res<RenderSettings>,
    mut commands: Commands,
    mut mesh_query: Query<(
        Entity,
        &mut MeshMaterial3d<StandardMaterial>,
        Option<&PaperFillMaterialOverride>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, mut material_handle, override_state) in &mut mesh_query {
        match (settings.paper_fill_enabled, override_state) {
            (true, None) => {
                let original = material_handle.0.clone();
                let Some(source) = materials.get(&original).cloned() else {
                    continue;
                };
                let mut override_material = source;
                override_material.base_color = Color::WHITE;
                override_material.emissive = LinearRgba::BLACK;
                override_material.perceptual_roughness = 1.0;
                override_material.metallic = 0.0;
                override_material.reflectance = 0.0;
                override_material.unlit = true;
                override_material.alpha_mode = AlphaMode::Opaque;
                let override_handle = materials.add(override_material);
                material_handle.0 = override_handle.clone();
                commands.entity(entity).insert(PaperFillMaterialOverride {
                    original,
                    override_handle,
                });
            }
            (false, Some(state)) => {
                material_handle.0 = state.original.clone();
                materials.remove(state.override_handle.id());
                commands
                    .entity(entity)
                    .remove::<PaperFillMaterialOverride>();
            }
            _ => {}
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn ssao_quality(level: u8) -> ScreenSpaceAmbientOcclusionQualityLevel {
    match level {
        0 => ScreenSpaceAmbientOcclusionQualityLevel::Low,
        1 => ScreenSpaceAmbientOcclusionQualityLevel::Medium,
        3 => ScreenSpaceAmbientOcclusionQualityLevel::Ultra,
        _ => ScreenSpaceAmbientOcclusionQualityLevel::High,
    }
}

fn sync_ssao_component(commands: &mut Commands, camera: Entity, settings: &RenderSettings) {
    if settings.ssao_enabled {
        commands.entity(camera).insert(ScreenSpaceAmbientOcclusion {
            quality_level: ssao_quality(settings.ambient_occlusion_quality),
            constant_object_thickness: settings.ssao_constant_object_thickness,
        });
    } else {
        commands
            .entity(camera)
            .remove::<ScreenSpaceAmbientOcclusion>();
    }
}

fn sync_bloom_component(commands: &mut Commands, camera: Entity, settings: &RenderSettings) {
    if settings.bloom_enabled {
        commands.entity(camera).insert(Bloom {
            intensity: settings.bloom_intensity,
            low_frequency_boost: settings.bloom_low_frequency_boost,
            low_frequency_boost_curvature: settings.bloom_low_frequency_boost_curvature,
            high_pass_frequency: settings.bloom_high_pass_frequency,
            prefilter: bevy::post_process::bloom::BloomPrefilter {
                threshold: settings.bloom_threshold,
                threshold_softness: settings.bloom_threshold_softness,
            },
            scale: Vec2::new(settings.bloom_scale[0], settings.bloom_scale[1]),
            ..Bloom::default()
        });
    } else {
        commands.entity(camera).remove::<Bloom>();
    }
}

fn sync_ssr_component(commands: &mut Commands, camera: Entity, settings: &RenderSettings) {
    if settings.ssr_enabled {
        commands.entity(camera).insert(ScreenSpaceReflections {
            perceptual_roughness_threshold: settings.ssr_perceptual_roughness_threshold,
            thickness: settings.ssr_thickness,
            linear_steps: settings.ssr_linear_steps.max(1),
            linear_march_exponent: settings.ssr_linear_march_exponent,
            bisection_steps: settings.ssr_bisection_steps,
            use_secant: settings.ssr_use_secant,
        });
    } else {
        commands.entity(camera).remove::<ScreenSpaceReflections>();
    }
}

#[derive(Clone)]
struct MeshOverlaySubject {
    entity: Entity,
    mesh_handle: Handle<Mesh>,
    mesh_transform: GlobalTransform,
    type_name: &'static str,
}

fn draw_model_edge_overlays(
    world: &World,
    settings: Res<RenderSettings>,
    registry: Res<CapabilityRegistry>,
    mesh_assets: Res<Assets<Mesh>>,
    camera_query: Query<(&GlobalTransform, &Projection), With<OrbitCamera>>,
    mut gizmos: Gizmos,
) {
    if !settings.wireframe_overlay_enabled
        && !settings.contour_overlay_enabled
        && !settings.visible_edge_overlay_enabled
    {
        return;
    }

    let Ok((camera_transform, projection)) = camera_query.single() else {
        return;
    };
    let camera_position = camera_transform.translation();
    let camera_forward = camera_transform.forward().as_vec3();
    let orthographic = matches!(projection, Projection::Orthographic(_));
    let Some(mut query) = world.try_query::<(
        Entity,
        &ElementId,
        &Mesh3d,
        &GlobalTransform,
        Option<&Visibility>,
    )>() else {
        return;
    };

    let mut subjects = Vec::new();
    for (entity, _element_id, mesh_handle, mesh_transform, visibility) in query.iter(world) {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        let type_name = registry
            .capture_snapshot(&entity_ref, world)
            .map(|snapshot| snapshot.type_name())
            .unwrap_or("triangle_mesh");
        if drawing_overlay_excluded(type_name) {
            continue;
        }
        subjects.push(MeshOverlaySubject {
            entity,
            mesh_handle: mesh_handle.0.clone(),
            mesh_transform: *mesh_transform,
            type_name,
        });
    }

    let scene_triangles = if settings.visible_edge_overlay_enabled {
        collect_scene_triangles(&subjects, &mesh_assets)
    } else {
        Vec::new()
    };

    for subject in subjects {
        if settings.wireframe_overlay_enabled {
            if let Some(factory) = registry.factory_for(subject.type_name) {
                factory.draw_selection(
                    world,
                    subject.entity,
                    &mut gizmos,
                    wireframe_overlay_color(&settings),
                );
            }
        }

        let Some(mesh) = mesh_assets.get(&subject.mesh_handle) else {
            continue;
        };

        if settings.contour_overlay_enabled {
            draw_mesh_contours(
                mesh,
                &subject.mesh_transform,
                camera_position,
                camera_forward,
                orthographic,
                &mut gizmos,
                CONTOUR_OVERLAY_COLOR,
            );
        }

        if settings.visible_edge_overlay_enabled {
            if settings.paper_fill_enabled {
                continue;
            }
            draw_visible_feature_edges(
                mesh,
                &subject.mesh_transform,
                subject.entity,
                camera_position,
                camera_forward,
                orthographic,
                &scene_triangles,
                &mut gizmos,
                VISIBLE_EDGE_OVERLAY_COLOR,
            );
        }
    }
}

/// Draw section fill cut edges and hatch lines in the viewport when paper mode
/// is active and clip planes are cutting geometry.
fn draw_section_fill_overlays(
    world: &World,
    settings: Res<RenderSettings>,
    mesh_assets: Res<Assets<Mesh>>,
    mut gizmos: Gizmos,
) {
    // Only draw section fills when visible-edge overlay is enabled (paper mode)
    if !settings.visible_edge_overlay_enabled {
        return;
    }

    let fills = crate::plugins::section_fill::extract_section_fills(world, &mesh_assets);
    if fills.is_empty() {
        return;
    }

    for fill in &fills {
        if fill.polygon_3d.len() < 3 {
            continue;
        }

        // Draw section cut outline (heavy)
        let color = if settings.paper_fill_enabled {
            Color::BLACK
        } else {
            Color::srgba(0.2, 0.6, 1.0, 0.9)
        };
        for i in 0..fill.polygon_3d.len() {
            let j = (i + 1) % fill.polygon_3d.len();
            gizmos.line(fill.polygon_3d[i], fill.polygon_3d[j], color);
        }

        // Draw hatch lines in 3D (project hatch from 2D back onto the clip plane)
        // For live preview we use a simplified approach: draw the cut polygon edges
        // in the heavy section-cut weight. Full hatch rendering is in vector export.
    }
}

fn draw_silhouette_edge_overlay(
    mut contexts: EguiContexts,
    settings: Res<RenderSettings>,
    viewport_export_state: Res<crate::plugins::drawing_export::ViewportExportState>,
    mesh_assets: Res<Assets<Mesh>>,
    camera_query: Query<
        (&Camera, &GlobalTransform, &Projection, Option<&OrbitCamera>),
        With<OrbitCamera>,
    >,
    mesh_query: Query<(
        Entity,
        &ElementId,
        &Mesh3d,
        &GlobalTransform,
        Option<&Visibility>,
    )>,
    clip_plane_query: Query<&crate::plugins::clipping_planes::ClipPlaneNode>,
) {
    if viewport_export_state.annotation_overlays_suppressed()
        || !settings.visible_edge_overlay_enabled
    {
        return;
    }
    let Ok(ctx_ref) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx_ref.clone();
    let Ok((camera, camera_transform, projection, orbit)) = camera_query.single() else {
        return;
    };
    let Some(px_per_world_m) = viewport_pixels_per_world_m(camera, camera_transform, orbit) else {
        return;
    };
    let px_per_paper_mm = (px_per_world_m / PAPER_MM_PER_WORLD_M).max(0.5);
    let camera_position = camera_transform.translation();
    let camera_forward = camera_transform.forward().as_vec3();
    let orthographic = matches!(projection, Projection::Orthographic(_));
    let mut subjects = Vec::new();
    for (entity, _element_id, mesh_handle, mesh_transform, visibility) in &mesh_query {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        subjects.push(crate::plugins::vector_drawing::MeshSubject {
            entity,
            mesh_handle: mesh_handle.0.clone(),
            mesh_transform: *mesh_transform,
        });
    }
    let scene_triangles =
        crate::plugins::vector_drawing::collect_scene_triangles(&subjects, &mesh_assets);
    let clip_planes: Vec<_> = clip_plane_query
        .iter()
        .filter(|plane| plane.active)
        .map(|plane| (plane.origin, plane.normal))
        .collect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("paper_visible_edge_overlay"),
    ));
    for subject in &subjects {
        let Some(mesh) = mesh_assets.get(&subject.mesh_handle) else {
            continue;
        };
        for (start, end, edge_type) in
            crate::plugins::vector_drawing::collect_classified_visible_edges(
                mesh,
                &subject.mesh_transform,
                subject.entity,
                camera_position,
                camera_forward,
                orthographic,
                &scene_triangles,
                &clip_planes,
            )
        {
            let Some((a, b)) =
                project_world_segment_to_viewport(camera, camera_transform, start, end)
            else {
                continue;
            };
            painter.line_segment(
                [a, b],
                egui::Stroke::new(
                    edge_stroke_width_px(edge_type, px_per_paper_mm),
                    edge_overlay_color(edge_type),
                ),
            );
        }
    }
}

fn wireframe_overlay_color(settings: &RenderSettings) -> Color {
    if settings.paper_fill_enabled {
        Color::BLACK
    } else {
        WIREFRAME_OVERLAY_COLOR
    }
}

fn viewport_pixels_per_world_m(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    orbit: Option<&OrbitCamera>,
) -> Option<f32> {
    let anchor = orbit.map(|orbit| orbit.focus).unwrap_or_else(|| {
        camera_transform.translation() + camera_transform.forward().as_vec3() * 10.0
    });
    let anchor_px = camera.world_to_viewport(camera_transform, anchor).ok()?;
    let right_px = camera
        .world_to_viewport(
            camera_transform,
            anchor + camera_transform.right().as_vec3(),
        )
        .ok()
        .map(|px| (px - anchor_px).length());
    let up_px = camera
        .world_to_viewport(camera_transform, anchor + camera_transform.up().as_vec3())
        .ok()
        .map(|px| (px - anchor_px).length());
    right_px
        .into_iter()
        .chain(up_px)
        .reduce(f32::max)
        .filter(|length| *length > 0.0)
}

fn edge_stroke_width_px(
    edge_type: crate::plugins::vector_drawing::EdgeType,
    px_per_paper_mm: f32,
) -> f32 {
    (edge_type.default_weight_mm() * px_per_paper_mm).clamp(1.0, 3.5)
}

fn edge_overlay_color(edge_type: crate::plugins::vector_drawing::EdgeType) -> egui::Color32 {
    match edge_type {
        crate::plugins::vector_drawing::EdgeType::Silhouette
        | crate::plugins::vector_drawing::EdgeType::SectionCut => egui::Color32::BLACK,
        crate::plugins::vector_drawing::EdgeType::Crease
        | crate::plugins::vector_drawing::EdgeType::Boundary => egui::Color32::from_rgb(48, 48, 52),
        crate::plugins::vector_drawing::EdgeType::Dimension => egui::Color32::from_rgb(74, 74, 78),
    }
}

fn project_world_segment_to_viewport(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    start: Vec3,
    end: Vec3,
) -> Option<(egui::Pos2, egui::Pos2)> {
    let a = camera.world_to_viewport(camera_transform, start).ok()?;
    let b = camera.world_to_viewport(camera_transform, end).ok()?;
    Some((egui::pos2(a.x, a.y), egui::pos2(b.x, b.y)))
}

fn drawing_overlay_excluded(type_name: &str) -> bool {
    matches!(
        type_name,
        "dimension_line" | "guide_line" | "scene_light" | "clipping_plane" | "group"
    )
}

#[derive(Clone, Copy)]
struct SceneTriangle {
    entity: Entity,
    a: Vec3,
    b: Vec3,
    c: Vec3,
}

fn collect_scene_triangles(
    subjects: &[MeshOverlaySubject],
    mesh_assets: &Assets<Mesh>,
) -> Vec<SceneTriangle> {
    let mut triangles = Vec::new();
    for subject in subjects {
        let Some(mesh) = mesh_assets.get(&subject.mesh_handle) else {
            continue;
        };
        let Some(positions) = mesh_positions(mesh) else {
            continue;
        };
        let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
            continue;
        };
        for triangle in indices.chunks(3) {
            if triangle.len() < 3 {
                continue;
            }
            let (Some(local_a), Some(local_b), Some(local_c)) = (
                positions.get(triangle[0] as usize).copied(),
                positions.get(triangle[1] as usize).copied(),
                positions.get(triangle[2] as usize).copied(),
            ) else {
                continue;
            };
            triangles.push(SceneTriangle {
                entity: subject.entity,
                a: subject.mesh_transform.transform_point(Vec3::from(local_a)),
                b: subject.mesh_transform.transform_point(Vec3::from(local_b)),
                c: subject.mesh_transform.transform_point(Vec3::from(local_c)),
            });
        }
    }
    triangles
}

fn draw_mesh_contours(
    mesh: &Mesh,
    mesh_transform: &GlobalTransform,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    gizmos: &mut Gizmos,
    color: Color,
) {
    let Some(positions) = mesh_positions(mesh) else {
        return;
    };
    let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
        return;
    };
    let contour_segments = collect_contour_segments(
        &positions,
        &indices,
        mesh_transform,
        camera_position,
        camera_forward,
        orthographic,
    );
    for (start, end) in contour_segments {
        gizmos.line(start, end, color);
    }
}

fn draw_visible_feature_edges(
    mesh: &Mesh,
    mesh_transform: &GlobalTransform,
    entity: Entity,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    scene_triangles: &[SceneTriangle],
    gizmos: &mut Gizmos,
    color: Color,
) {
    for (start, end) in collect_visible_feature_segments(
        mesh,
        mesh_transform,
        entity,
        camera_position,
        camera_forward,
        orthographic,
        scene_triangles,
    ) {
        gizmos.line(start, end, color);
    }
}

fn collect_visible_feature_segments(
    mesh: &Mesh,
    mesh_transform: &GlobalTransform,
    entity: Entity,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    scene_triangles: &[SceneTriangle],
) -> Vec<(Vec3, Vec3)> {
    let Some(positions) = mesh_positions(mesh) else {
        return Vec::new();
    };
    let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
        return Vec::new();
    };

    let mut edges = HashMap::<EdgeKey, FeatureEdgeState>::new();
    for triangle in indices.chunks(3) {
        if triangle.len() < 3 {
            continue;
        }
        let (Some(local_a), Some(local_b), Some(local_c)) = (
            positions.get(triangle[0] as usize).copied(),
            positions.get(triangle[1] as usize).copied(),
            positions.get(triangle[2] as usize).copied(),
        ) else {
            continue;
        };

        let world_a = mesh_transform.transform_point(Vec3::from(local_a));
        let world_b = mesh_transform.transform_point(Vec3::from(local_b));
        let world_c = mesh_transform.transform_point(Vec3::from(local_c));
        let normal = (world_b - world_a)
            .cross(world_c - world_a)
            .normalize_or_zero();
        if normal.length_squared() <= f32::EPSILON {
            continue;
        }
        let face_center = (world_a + world_b + world_c) / 3.0;
        let view_to_camera = if orthographic {
            -camera_forward
        } else {
            (camera_position - face_center).normalize_or_zero()
        };
        let front_facing = normal.dot(view_to_camera) >= 0.0;

        register_feature_edge(
            &mut edges,
            local_a,
            local_b,
            world_a,
            world_b,
            normal,
            front_facing,
        );
        register_feature_edge(
            &mut edges,
            local_b,
            local_c,
            world_b,
            world_c,
            normal,
            front_facing,
        );
        register_feature_edge(
            &mut edges,
            local_c,
            local_a,
            world_c,
            world_a,
            normal,
            front_facing,
        );
    }

    edges
        .into_values()
        .filter(|edge| edge.is_visible_candidate())
        .flat_map(|edge| {
            edge_visible_subsegments(
                edge.start_world,
                edge.end_world,
                entity,
                camera_position,
                camera_forward,
                orthographic,
                scene_triangles,
            )
        })
        .collect()
}

fn register_feature_edge(
    edges: &mut HashMap<EdgeKey, FeatureEdgeState>,
    local_start: [f32; 3],
    local_end: [f32; 3],
    world_start: Vec3,
    world_end: Vec3,
    normal: Vec3,
    front_facing: bool,
) {
    let key = EdgeKey::from_points(local_start, local_end);
    let state = edges.entry(key).or_insert_with(|| FeatureEdgeState {
        start_world: world_start,
        end_world: world_end,
        normals: [Vec3::ZERO; 2],
        front_facing: [false; 2],
        total_faces: 0,
    });
    let face_index = usize::from(state.total_faces.min(1));
    state.normals[face_index] = normal;
    state.front_facing[face_index] = front_facing;
    state.total_faces = state.total_faces.saturating_add(1);
}

fn edge_visible_subsegments(
    start: Vec3,
    end: Vec3,
    owner_entity: Entity,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    scene_triangles: &[SceneTriangle],
) -> Vec<(Vec3, Vec3)> {
    let mut visible_segments = Vec::new();
    for step in 0..VISIBLE_EDGE_SEGMENT_STEPS {
        let t0 = step as f32 / VISIBLE_EDGE_SEGMENT_STEPS as f32;
        let t1 = (step + 1) as f32 / VISIBLE_EDGE_SEGMENT_STEPS as f32;
        let midpoint = start.lerp(end, (t0 + t1) * 0.5);
        if point_is_visible(
            midpoint,
            owner_entity,
            camera_position,
            camera_forward,
            orthographic,
            scene_triangles,
        ) {
            visible_segments.push((start.lerp(end, t0), start.lerp(end, t1)));
        }
    }
    visible_segments
}

fn point_is_visible(
    sample: Vec3,
    owner_entity: Entity,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    scene_triangles: &[SceneTriangle],
) -> bool {
    let (ray_origin, ray_direction, max_distance) = if orthographic {
        let Some(direction) = Dir3::new(camera_forward).ok() else {
            return true;
        };
        (
            sample - direction.as_vec3() * ORTHOGRAPHIC_VISIBILITY_RAY_LENGTH,
            direction,
            ORTHOGRAPHIC_VISIBILITY_RAY_LENGTH,
        )
    } else {
        let ray_vector = sample - camera_position;
        let max_distance = ray_vector.length();
        let Some(direction) = Dir3::new(ray_vector).ok() else {
            return true;
        };
        (camera_position, direction, max_distance)
    };
    let ray = Ray3d::new(ray_origin, ray_direction);
    !scene_triangles.iter().any(|triangle| {
        if triangle.entity == owner_entity {
            return ray_triangle_intersection(ray, triangle.a, triangle.b, triangle.c).is_some_and(
                |distance| {
                    distance > EDGE_VISIBILITY_EPSILON
                        && distance < max_distance - EDGE_VISIBILITY_EPSILON
                },
            );
        }
        ray_triangle_intersection(ray, triangle.a, triangle.b, triangle.c).is_some_and(|distance| {
            distance > EDGE_VISIBILITY_EPSILON && distance < max_distance - EDGE_VISIBILITY_EPSILON
        })
    })
}

fn mesh_positions(mesh: &Mesh) -> Option<Vec<[f32; 3]>> {
    match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
        VertexAttributeValues::Float32x3(values) => Some(values.clone()),
        _ => None,
    }
}

fn mesh_triangle_indices(mesh: &Mesh, vertex_count: usize) -> Option<Vec<u32>> {
    match mesh.indices() {
        Some(Indices::U32(values)) => Some(values.clone()),
        Some(Indices::U16(values)) => Some(values.iter().map(|value| *value as u32).collect()),
        None if vertex_count % 3 == 0 => Some((0..vertex_count as u32).collect()),
        None => None,
    }
}

fn collect_contour_segments(
    positions: &[[f32; 3]],
    indices: &[u32],
    mesh_transform: &GlobalTransform,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
) -> Vec<(Vec3, Vec3)> {
    let mut edges = HashMap::<EdgeKey, EdgeContourState>::new();
    for triangle in indices.chunks(3) {
        if triangle.len() < 3 {
            continue;
        }
        let (Some(local_a), Some(local_b), Some(local_c)) = (
            positions.get(triangle[0] as usize).copied(),
            positions.get(triangle[1] as usize).copied(),
            positions.get(triangle[2] as usize).copied(),
        ) else {
            continue;
        };

        let world_a = mesh_transform.transform_point(Vec3::from(local_a));
        let world_b = mesh_transform.transform_point(Vec3::from(local_b));
        let world_c = mesh_transform.transform_point(Vec3::from(local_c));
        let normal = (world_b - world_a)
            .cross(world_c - world_a)
            .normalize_or_zero();
        if normal.length_squared() <= f32::EPSILON {
            continue;
        }
        let face_center = (world_a + world_b + world_c) / 3.0;
        let view_to_camera = if orthographic {
            -camera_forward
        } else {
            (camera_position - face_center).normalize_or_zero()
        };
        let front_facing = normal.dot(view_to_camera) >= 0.0;
        register_contour_edge(&mut edges, local_a, local_b, world_a, world_b, front_facing);
        register_contour_edge(&mut edges, local_b, local_c, world_b, world_c, front_facing);
        register_contour_edge(&mut edges, local_c, local_a, world_c, world_a, front_facing);
    }

    edges
        .into_values()
        .filter(|edge| edge.is_contour())
        .map(|edge| (edge.start_world, edge.end_world))
        .collect()
}

fn register_contour_edge(
    edges: &mut HashMap<EdgeKey, EdgeContourState>,
    local_start: [f32; 3],
    local_end: [f32; 3],
    world_start: Vec3,
    world_end: Vec3,
    front_facing: bool,
) {
    let key = EdgeKey::from_points(local_start, local_end);
    let state = edges.entry(key).or_insert_with(|| EdgeContourState {
        start_world: world_start,
        end_world: world_end,
        front_faces: 0,
        back_faces: 0,
        total_faces: 0,
    });
    state.total_faces = state.total_faces.saturating_add(1);
    if front_facing {
        state.front_faces = state.front_faces.saturating_add(1);
    } else {
        state.back_faces = state.back_faces.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct QuantizedPoint(i64, i64, i64);

impl QuantizedPoint {
    fn from_point(point: [f32; 3]) -> Self {
        Self(
            (point[0] * EDGE_QUANTIZATION_SCALE).round() as i64,
            (point[1] * EDGE_QUANTIZATION_SCALE).round() as i64,
            (point[2] * EDGE_QUANTIZATION_SCALE).round() as i64,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EdgeKey(QuantizedPoint, QuantizedPoint);

impl EdgeKey {
    fn from_points(a: [f32; 3], b: [f32; 3]) -> Self {
        let start = QuantizedPoint::from_point(a);
        let end = QuantizedPoint::from_point(b);
        if start <= end {
            Self(start, end)
        } else {
            Self(end, start)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct EdgeContourState {
    start_world: Vec3,
    end_world: Vec3,
    front_faces: u8,
    back_faces: u8,
    total_faces: u8,
}

impl EdgeContourState {
    fn is_contour(&self) -> bool {
        match self.total_faces {
            0 => false,
            1 => self.front_faces > 0,
            _ => self.front_faces > 0 && self.back_faces > 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FeatureEdgeState {
    start_world: Vec3,
    end_world: Vec3,
    normals: [Vec3; 2],
    front_facing: [bool; 2],
    total_faces: u8,
}

impl FeatureEdgeState {
    fn is_visible_candidate(&self) -> bool {
        match self.total_faces {
            0 => false,
            1 => true,
            _ => {
                let silhouette = self.front_facing[0] != self.front_facing[1];
                let crease = self.normals[0].dot(self.normals[1]) <= FEATURE_EDGE_COS_THRESHOLD;
                silhouette || crease
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::ui::StatusBarData;

    #[test]
    fn internal_triangle_diagonal_is_not_treated_as_contour() {
        let positions = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let contours = collect_contour_segments(
            &positions,
            &indices,
            &GlobalTransform::IDENTITY,
            Vec3::new(0.5, 0.5, 3.0),
            Vec3::NEG_Z,
            false,
        );

        assert_eq!(contours.len(), 4);
    }

    #[test]
    fn default_render_settings_start_with_drawing_overlays_disabled() {
        let settings = RenderSettings::default();

        assert!(!settings.wireframe_overlay_enabled);
        assert!(!settings.contour_overlay_enabled);
        assert!(!settings.visible_edge_overlay_enabled);
        assert!(settings.grid_enabled);
        assert!(!settings.paper_fill_enabled);
        assert_eq!(settings.background_rgb, DEFAULT_BACKGROUND_RGB);
    }

    #[test]
    fn paper_preset_enables_white_background_and_outline_mode() {
        let mut app = App::new();
        app.insert_resource(RenderSettings::default())
            .insert_resource(PaperDrawingState::default())
            .insert_resource(StatusBarData::default());

        execute_apply_paper_preset(app.world_mut(), &Value::Null)
            .expect("paper preset should apply");

        let settings = app.world().resource::<RenderSettings>();
        assert_eq!(settings.background_rgb, PAPER_BACKGROUND_RGB);
        assert_eq!(settings.tonemapping, RenderTonemapping::None);
        assert!(!settings.ssao_enabled);
        assert!(!settings.bloom_enabled);
        assert!(!settings.ssr_enabled);
        assert!(!settings.grid_enabled);
        assert!(settings.paper_fill_enabled);
        assert!(settings.visible_edge_overlay_enabled);
        assert!(!settings.wireframe_overlay_enabled);
        assert!(!settings.contour_overlay_enabled);
    }

    #[test]
    fn paper_preset_command_restores_previous_render_state() {
        let previous = RenderSettings {
            grid_enabled: false,
            wireframe_overlay_enabled: true,
            background_rgb: [0.2, 0.3, 0.4],
            ..RenderSettings::default()
        };
        let mut app = App::new();
        app.insert_resource(previous.clone())
            .insert_resource(PaperDrawingState::default())
            .insert_resource(StatusBarData::default());

        execute_apply_paper_preset(app.world_mut(), &Value::Null)
            .expect("paper preset should enable");
        execute_apply_paper_preset(app.world_mut(), &Value::Null)
            .expect("paper preset should disable");

        let restored = app.world().resource::<RenderSettings>();
        assert_eq!(*restored, previous);
    }

    #[test]
    fn paper_toggle_uses_baseline_state_instead_of_paper_fill_flag() {
        let mut settings = RenderSettings {
            paper_fill_enabled: true,
            visible_edge_overlay_enabled: false,
            ..RenderSettings::default()
        };
        let mut paper_state = PaperDrawingState::default();

        let enabled = toggle_paper_drawing_mode(&mut settings, &mut paper_state);

        assert!(enabled);
        assert!(paper_drawing_toggle_active(&paper_state));
        assert!(settings.visible_edge_overlay_enabled);
    }

    #[test]
    fn visible_feature_edges_hide_back_edges_in_front_projection() {
        let positions = vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let indices = vec![
            0, 1, 2, 0, 2, 3, // back
            4, 6, 5, 4, 7, 6, // front
            0, 4, 5, 0, 5, 1, // bottom
            3, 2, 6, 3, 6, 7, // top
            1, 5, 6, 1, 6, 2, // right
            0, 3, 7, 0, 7, 4, // left
        ];
        let mut mesh = Mesh::new(
            bevy::render::render_resource::PrimitiveTopology::TriangleList,
            Default::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
        mesh.insert_indices(Indices::U32(indices));

        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(mesh.clone());
        let subjects = vec![MeshOverlaySubject {
            entity: Entity::from_bits(1),
            mesh_handle: handle,
            mesh_transform: GlobalTransform::IDENTITY,
            type_name: "box",
        }];
        let scene_triangles = collect_scene_triangles(&subjects, &meshes);
        let segments = collect_visible_feature_segments(
            &mesh,
            &GlobalTransform::IDENTITY,
            Entity::from_bits(1),
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::NEG_Z,
            true,
            &scene_triangles,
        );

        assert_eq!(segments.len(), 4 * VISIBLE_EDGE_SEGMENT_STEPS);
    }
}
