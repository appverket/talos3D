/// Real-time render pipeline configuration for Talos3D.
///
/// Adds SSAO, Bloom, optional SSR, and AgX tonemapping to the scene camera.
/// Exposes [`RenderSettings`] as a hot-reloadable resource: toggling any flag
/// takes effect on the very next frame.
use bevy::{
    anti_alias::fxaa::Fxaa,
    asset::RenderAssetUsages,
    camera::Exposure,
    core_pipeline::tonemapping::Tonemapping,
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    pbr::{
        ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel,
        ScreenSpaceReflections,
    },
    post_process::bloom::Bloom,
    prelude::*,
};
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
        modeling::mesh_generation::MeshGenerationSet,
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
    },
};

const WIREFRAME_OVERLAY_COLOR: Color = Color::srgba(0.12, 0.13, 0.15, 1.0);
const CONTOUR_OVERLAY_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 1.0);
const VISIBLE_EDGE_OVERLAY_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 1.0);
const EDGE_QUANTIZATION_SCALE: f32 = 10_000.0;
const DEFAULT_BACKGROUND_RGB: [f32; 3] = [0.17, 0.18, 0.20];
const FEATURE_EDGE_COS_THRESHOLD: f32 = 0.85;
pub const VIEW_RENDER_TOOLBAR_ID: &str = "view.render";
const PAPER_BACKGROUND_RGB: [f32; 3] = [1.0, 1.0, 1.0];
const DEFAULT_XRAY_SURFACE_ALPHA: f32 = 0.28;

// ─── Settings resource ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderTonemapping {
    None,
    Reinhard,
    ReinhardLuminance,
    AcesFitted,
    #[default]
    AgX,
    SomewhatBoringDisplayTransform,
    TonyMcMapface,
    BlenderFilmic,
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

    #[allow(clippy::should_implement_trait)]
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
    /// Make surface materials semi-transparent to inspect hidden/interior parts.
    pub xray_enabled: bool,
    /// Alpha applied to surface materials while X-ray mode is enabled.
    pub xray_surface_alpha: f32,
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
            xray_enabled: false,
            xray_surface_alpha: DEFAULT_XRAY_SURFACE_ALPHA,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDisplayMode {
    Shaded,
    Outline,
    Wireframe,
}

impl EdgeDisplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shaded => "shaded",
            Self::Outline => "outline",
            Self::Wireframe => "wireframe",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "shaded" | "solid" | "none" => Some(Self::Shaded),
            "outline" | "visible_edges" | "visible-edge" => Some(Self::Outline),
            "wireframe" | "wire" => Some(Self::Wireframe),
            _ => None,
        }
    }
}

impl RenderSettings {
    pub fn edge_display_mode(&self) -> EdgeDisplayMode {
        if self.wireframe_overlay_enabled {
            EdgeDisplayMode::Wireframe
        } else if self.visible_edge_overlay_enabled {
            EdgeDisplayMode::Outline
        } else {
            EdgeDisplayMode::Shaded
        }
    }

    pub fn set_edge_display_mode(&mut self, mode: EdgeDisplayMode) {
        self.wireframe_overlay_enabled = mode == EdgeDisplayMode::Wireframe;
        self.visible_edge_overlay_enabled = mode == EdgeDisplayMode::Outline;
        if mode != EdgeDisplayMode::Shaded {
            self.contour_overlay_enabled = false;
        }
    }
}

#[derive(Component, Debug, Clone)]
struct SurfaceMaterialOverride {
    original: Handle<StandardMaterial>,
    override_handle: Handle<StandardMaterial>,
    mode: SurfaceMaterialMode,
    xray_alpha: f32,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireframeSurfaceVisibilityOverride {
    pub(crate) original: Visibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SurfaceMaterialMode {
    PaperFill,
    WireframeOnly,
    Xray,
}

#[derive(Resource, Debug, Clone, Default)]
struct SurfaceMaterialOverrideCache {
    handles: HashMap<SurfaceMaterialOverrideCacheKey, Handle<StandardMaterial>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SurfaceMaterialOverrideCacheKey {
    original: String,
    mode: SurfaceMaterialMode,
    xray_alpha_milli: u16,
}

#[derive(Component, Debug, Clone)]
struct OutlineMeshOverlay {
    entity: Entity,
    source_mesh: Handle<Mesh>,
}

#[derive(Component, Debug, Clone, Copy)]
struct OutlineMeshOverlayEntity;

#[derive(Resource, Debug, Clone, Default)]
struct OutlineMeshOverlayMaterial {
    handle: Option<Handle<StandardMaterial>>,
}

#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct PaperDrawingState {
    baseline: Option<RenderSettings>,
}

#[derive(Resource, Debug, Clone, Default)]
struct RenderPipelineSetupState {
    configured: bool,
    warned_missing_camera: bool,
}

// ─── Plugin ──────────────────────────────────────────────────────────────────

/// Registers [`RenderSettings`] and wires up the camera post-processing stack.
pub struct RenderPipelinePlugin;

impl Plugin for RenderPipelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderSettings>()
            .init_resource::<RenderPipelineSetupState>()
            .init_resource::<PaperDrawingState>()
            .init_resource::<SurfaceMaterialOverrideCache>()
            .init_resource::<OutlineMeshOverlayMaterial>()
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
                        "view.toggle_xray".to_string(),
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
                    id: "view.toggle_xray".to_string(),
                    label: "X-Ray".to_string(),
                    description: "Toggle X-Ray view, rendering scene faces translucent."
                        .to_string(),
                    category: CommandCategory::View,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "description": "Set X-Ray on or off. Omit to toggle the current state."
                            },
                            "xray_surface_alpha": {
                                "type": "number",
                                "minimum": 0.02,
                                "maximum": 0.95,
                                "description": "Optional face alpha override. Defaults to 0.28."
                            }
                        }
                    })),
                    default_shortcut: None,
                    icon: Some("icon.view_xray".to_string()),
                    hint: Some("Make faces translucent to inspect hidden geometry".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_xray,
            )
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_outline".to_string(),
                    label: "Outline".to_string(),
                    description: "Use visible-edge outline display mode.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_outline".to_string()),
                    hint: Some("Use hidden-line-friendly outline rendering".to_string()),
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
                    label: "Wireframe".to_string(),
                    description: "Use full wireframe display mode.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_wireframe".to_string()),
                    hint: Some("Use full wireframe rendering".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_wireframe,
            )
            .add_systems(
                Update,
                (
                    setup_render_pipeline_once,
                    sync_render_settings,
                    sync_clear_color,
                    sync_wireframe_surface_visibility.after(MeshGenerationSet::Generate),
                    sync_surface_display_materials,
                    sync_outline_mesh_overlays.after(MeshGenerationSet::Generate),
                ),
            )
            .add_systems(
                Update,
                (
                    draw_model_edge_overlays.after(sync_wireframe_surface_visibility),
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

fn execute_toggle_xray(world: &mut World, parameters: &Value) -> Result<CommandResult, String> {
    update_render_settings(world, "", |settings| {
        if let Some(alpha) = parameters
            .get("xray_surface_alpha")
            .and_then(Value::as_f64)
            .map(|alpha| alpha as f32)
        {
            settings.xray_surface_alpha = alpha.clamp(0.02, 0.95);
        }

        settings.xray_enabled = parameters
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(!settings.xray_enabled);
        if settings.xray_enabled {
            settings.paper_fill_enabled = false;
        }
    })
}

fn execute_toggle_outline(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    update_render_settings(world, "", |settings| {
        if settings.edge_display_mode() == EdgeDisplayMode::Outline {
            settings.set_edge_display_mode(EdgeDisplayMode::Shaded);
        } else {
            settings.set_edge_display_mode(EdgeDisplayMode::Outline);
        }
    })
}

fn execute_toggle_wireframe(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    update_render_settings(world, "", |settings| {
        if settings.edge_display_mode() == EdgeDisplayMode::Wireframe {
            settings.set_edge_display_mode(EdgeDisplayMode::Shaded);
        } else {
            settings.set_edge_display_mode(EdgeDisplayMode::Wireframe);
        }
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
                "Grid {} · X-Ray {} · Display {}",
                on_off(settings.grid_enabled),
                on_off(settings.xray_enabled),
                settings.edge_display_mode().as_str()
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

fn live_depth_tested_outline_active(settings: &RenderSettings) -> bool {
    settings.visible_edge_overlay_enabled && !settings.paper_fill_enabled
}

pub(crate) fn apply_paper_drawing_preset(settings: &mut RenderSettings) {
    settings.tonemapping = RenderTonemapping::None;
    settings.ssao_enabled = false;
    settings.bloom_enabled = false;
    settings.ssr_enabled = false;
    settings.background_rgb = PAPER_BACKGROUND_RGB;
    settings.grid_enabled = false;
    settings.paper_fill_enabled = true;
    settings.xray_enabled = false;
    settings.visible_edge_overlay_enabled = false;
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

// ─── Camera render setup ─────────────────────────────────────────────────────

fn setup_render_pipeline_once(
    mut commands: Commands,
    settings: Res<RenderSettings>,
    camera_query: Query<Entity, With<Camera3d>>,
    mut clear_color: ResMut<ClearColor>,
    mut setup_state: ResMut<RenderPipelineSetupState>,
) {
    if setup_state.configured {
        return;
    }

    let Ok(camera) = camera_query.single() else {
        if !setup_state.warned_missing_camera {
            warn!("RenderPipelinePlugin: no Camera3d found; will retry render setup");
            setup_state.warned_missing_camera = true;
        }
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
    setup_state.configured = true;
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

fn sync_surface_display_materials(
    settings: Res<RenderSettings>,
    mut commands: Commands,
    mut mesh_queries: ParamSet<(
        Query<(
            Entity,
            &mut MeshMaterial3d<StandardMaterial>,
            Option<&SurfaceMaterialOverride>,
        )>,
        Query<
            (
                Entity,
                &mut MeshMaterial3d<StandardMaterial>,
                Option<&SurfaceMaterialOverride>,
            ),
            Or<(
                Added<MeshMaterial3d<StandardMaterial>>,
                Changed<MeshMaterial3d<StandardMaterial>>,
            )>,
        >,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut override_cache: ResMut<SurfaceMaterialOverrideCache>,
) {
    let target_mode = active_surface_material_mode(&settings);
    if !settings.is_changed() && target_mode.is_none() {
        return;
    }

    if settings.is_changed() {
        for (entity, mut material_handle, override_state) in &mut mesh_queries.p0() {
            sync_surface_display_material(
                &mut commands,
                &mut materials,
                &mut override_cache,
                &settings,
                target_mode,
                entity,
                &mut material_handle,
                override_state,
            );
        }
    } else {
        // While a display override is active, only newly spawned or explicitly
        // re-materialised meshes need work. Unchanged frames avoid a full scene
        // scan, keeping pointer hover/selection responsive.
        for (entity, mut material_handle, override_state) in &mut mesh_queries.p1() {
            sync_surface_display_material(
                &mut commands,
                &mut materials,
                &mut override_cache,
                &settings,
                target_mode,
                entity,
                &mut material_handle,
                override_state,
            );
        }
    }
}

fn sync_surface_display_material(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    override_cache: &mut SurfaceMaterialOverrideCache,
    settings: &RenderSettings,
    target_mode: Option<SurfaceMaterialMode>,
    entity: Entity,
    material_handle: &mut MeshMaterial3d<StandardMaterial>,
    override_state: Option<&SurfaceMaterialOverride>,
) {
    match (target_mode, override_state) {
        (Some(mode), Some(state))
            if surface_material_override_is_current(state, mode, settings)
                && material_handle.0 == state.override_handle => {}
        (Some(mode), Some(state)) => {
            let original = if material_handle.0 == state.override_handle {
                state.original.clone()
            } else {
                material_handle.0.clone()
            };
            apply_surface_material_override(
                commands,
                materials,
                override_cache,
                settings,
                entity,
                material_handle,
                original,
                mode,
            );
        }
        (Some(mode), None) => {
            apply_surface_material_override(
                commands,
                materials,
                override_cache,
                settings,
                entity,
                material_handle,
                material_handle.0.clone(),
                mode,
            );
        }
        (None, Some(state)) => {
            if material_handle.0 == state.override_handle {
                material_handle.0 = state.original.clone();
            }
            commands.entity(entity).remove::<SurfaceMaterialOverride>();
        }
        _ => {}
    }
}

fn apply_surface_material_override(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    override_cache: &mut SurfaceMaterialOverrideCache,
    settings: &RenderSettings,
    entity: Entity,
    material_handle: &mut MeshMaterial3d<StandardMaterial>,
    original: Handle<StandardMaterial>,
    mode: SurfaceMaterialMode,
) {
    let Some(source) = materials.get(&original).cloned() else {
        commands.entity(entity).remove::<SurfaceMaterialOverride>();
        return;
    };
    let xray_alpha = xray_alpha_for_mode(mode, settings);
    let override_handle = cached_display_override_material(
        override_cache,
        materials,
        &original,
        &source,
        mode,
        xray_alpha,
    );
    material_handle.0 = override_handle.clone();
    commands.entity(entity).insert(SurfaceMaterialOverride {
        original,
        override_handle,
        mode,
        xray_alpha,
    });
}

fn cached_display_override_material(
    override_cache: &mut SurfaceMaterialOverrideCache,
    materials: &mut Assets<StandardMaterial>,
    original: &Handle<StandardMaterial>,
    source: &StandardMaterial,
    mode: SurfaceMaterialMode,
    xray_alpha: f32,
) -> Handle<StandardMaterial> {
    let key = SurfaceMaterialOverrideCacheKey::new(original, mode, xray_alpha);
    if let Some(handle) = override_cache.handles.get(&key) {
        if materials.contains(handle) {
            return handle.clone();
        }
    }

    let override_material = display_override_material(source, mode, xray_alpha);
    let handle = materials.add(override_material);
    override_cache.handles.insert(key, handle.clone());
    handle
}

impl SurfaceMaterialOverrideCacheKey {
    fn new(
        original: &Handle<StandardMaterial>,
        mode: SurfaceMaterialMode,
        xray_alpha: f32,
    ) -> Self {
        Self {
            original: format!("{:?}", original.id()),
            mode,
            xray_alpha_milli: (xray_alpha.clamp(0.0, 1.0) * 1000.0).round() as u16,
        }
    }
}

fn sync_wireframe_surface_visibility(
    settings: Res<RenderSettings>,
    mut commands: Commands,
    mut mesh_queries: ParamSet<(
        Query<
            (
                Entity,
                Option<&Visibility>,
                Option<&WireframeSurfaceVisibilityOverride>,
            ),
            With<Mesh3d>,
        >,
        Query<
            (
                Entity,
                Option<&Visibility>,
                Option<&WireframeSurfaceVisibilityOverride>,
            ),
            (With<Mesh3d>, Or<(Added<Mesh3d>, Changed<Visibility>)>),
        >,
    )>,
) {
    let wireframe_only =
        active_surface_material_mode(&settings) == Some(SurfaceMaterialMode::WireframeOnly);
    if !settings.is_changed() && !wireframe_only {
        return;
    }

    if settings.is_changed() {
        for (entity, visibility, override_state) in &mesh_queries.p0() {
            apply_wireframe_surface_visibility(
                &mut commands,
                entity,
                wireframe_only,
                visibility.copied(),
                override_state.copied(),
            );
        }
    } else {
        for (entity, visibility, override_state) in &mesh_queries.p1() {
            apply_wireframe_surface_visibility(
                &mut commands,
                entity,
                wireframe_only,
                visibility.copied(),
                override_state.copied(),
            );
        }
    }
}

fn apply_wireframe_surface_visibility(
    commands: &mut Commands,
    entity: Entity,
    wireframe_only: bool,
    visibility: Option<Visibility>,
    override_state: Option<WireframeSurfaceVisibilityOverride>,
) {
    match (wireframe_only, visibility, override_state) {
        (true, Some(Visibility::Hidden), Some(_)) => {}
        (true, current, None) => {
            commands.entity(entity).insert((
                WireframeSurfaceVisibilityOverride {
                    original: current.unwrap_or(Visibility::Inherited),
                },
                Visibility::Hidden,
            ));
        }
        (true, _, Some(_)) => {
            commands.entity(entity).insert(Visibility::Hidden);
        }
        (false, _, Some(state)) => {
            commands
                .entity(entity)
                .insert(state.original)
                .remove::<WireframeSurfaceVisibilityOverride>();
        }
        (false, _, None) => {}
    }
}

fn sync_outline_mesh_overlays(
    settings: Res<RenderSettings>,
    mut commands: Commands,
    mut source_queries: ParamSet<(
        Query<
            (
                Entity,
                &Mesh3d,
                Option<&Visibility>,
                Option<&OutlineMeshOverlay>,
            ),
            With<ElementId>,
        >,
        Query<
            (
                Entity,
                &Mesh3d,
                Option<&Visibility>,
                Option<&OutlineMeshOverlay>,
            ),
            (
                With<ElementId>,
                Or<(Added<Mesh3d>, Changed<Mesh3d>, Changed<Visibility>)>,
            ),
        >,
    )>,
    mesh_assets: Res<Assets<Mesh>>,
    mut outline_mesh_assets: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut outline_material: ResMut<OutlineMeshOverlayMaterial>,
) {
    let outline_active =
        live_depth_tested_outline_active(&settings) && !settings.wireframe_overlay_enabled;
    if !settings.is_changed() && !outline_active {
        return;
    }

    let material_handle = outline_overlay_material(
        &mut outline_material,
        &mut materials,
        VISIBLE_EDGE_OVERLAY_COLOR,
    );

    if settings.is_changed() {
        for (entity, mesh_handle, visibility, overlay) in &source_queries.p0() {
            sync_outline_mesh_overlay_for_source(
                &mut commands,
                &mesh_assets,
                &mut outline_mesh_assets,
                &material_handle,
                outline_active,
                entity,
                mesh_handle,
                visibility.copied(),
                overlay,
            );
        }
    } else {
        for (entity, mesh_handle, visibility, overlay) in &source_queries.p1() {
            sync_outline_mesh_overlay_for_source(
                &mut commands,
                &mesh_assets,
                &mut outline_mesh_assets,
                &material_handle,
                outline_active,
                entity,
                mesh_handle,
                visibility.copied(),
                overlay,
            );
        }
    }
}

fn sync_outline_mesh_overlay_for_source(
    commands: &mut Commands,
    mesh_assets: &Assets<Mesh>,
    outline_mesh_assets: &mut Assets<Mesh>,
    material_handle: &Handle<StandardMaterial>,
    outline_active: bool,
    source_entity: Entity,
    source_mesh_handle: &Mesh3d,
    visibility: Option<Visibility>,
    overlay: Option<&OutlineMeshOverlay>,
) {
    if !outline_active || visibility == Some(Visibility::Hidden) {
        remove_outline_mesh_overlay(commands, source_entity, overlay);
        return;
    }

    if overlay.is_some_and(|overlay| overlay.source_mesh == source_mesh_handle.0) {
        return;
    }
    remove_outline_mesh_overlay(commands, source_entity, overlay);

    let Some(source_mesh) = mesh_assets.get(&source_mesh_handle.0) else {
        return;
    };
    let Some(outline_mesh) = outline_mesh_from_surface_mesh(source_mesh) else {
        return;
    };
    let outline_mesh_handle = outline_mesh_assets.add(outline_mesh);
    let outline_entity = commands
        .spawn((
            Mesh3d(outline_mesh_handle),
            MeshMaterial3d(material_handle.clone()),
            Transform::IDENTITY,
            Visibility::Inherited,
            OutlineMeshOverlayEntity,
        ))
        .id();
    commands.entity(source_entity).add_child(outline_entity);
    commands.entity(source_entity).insert(OutlineMeshOverlay {
        entity: outline_entity,
        source_mesh: source_mesh_handle.0.clone(),
    });
}

fn remove_outline_mesh_overlay(
    commands: &mut Commands,
    source_entity: Entity,
    overlay: Option<&OutlineMeshOverlay>,
) {
    if let Some(overlay) = overlay {
        commands.entity(overlay.entity).despawn();
        commands
            .entity(source_entity)
            .remove::<OutlineMeshOverlay>();
    }
}

fn outline_overlay_material(
    cache: &mut OutlineMeshOverlayMaterial,
    materials: &mut Assets<StandardMaterial>,
    color: Color,
) -> Handle<StandardMaterial> {
    if let Some(handle) = cache
        .handle
        .as_ref()
        .filter(|handle| materials.contains(*handle))
    {
        return handle.clone();
    }

    let handle = materials.add(StandardMaterial {
        base_color: color,
        unlit: true,
        alpha_mode: AlphaMode::Opaque,
        depth_bias: 1.0,
        ..Default::default()
    });
    cache.handle = Some(handle.clone());
    handle
}

fn surface_material_override_is_current(
    state: &SurfaceMaterialOverride,
    mode: SurfaceMaterialMode,
    settings: &RenderSettings,
) -> bool {
    state.mode == mode
        && match mode {
            SurfaceMaterialMode::PaperFill => true,
            SurfaceMaterialMode::WireframeOnly => true,
            SurfaceMaterialMode::Xray => {
                (state.xray_alpha - xray_alpha_for_mode(mode, settings)).abs() < f32::EPSILON
            }
        }
}

fn xray_alpha_for_mode(mode: SurfaceMaterialMode, settings: &RenderSettings) -> f32 {
    match mode {
        SurfaceMaterialMode::PaperFill => 1.0,
        SurfaceMaterialMode::WireframeOnly => 0.0,
        SurfaceMaterialMode::Xray => settings.xray_surface_alpha.clamp(0.02, 0.95),
    }
}

fn active_surface_material_mode(settings: &RenderSettings) -> Option<SurfaceMaterialMode> {
    if settings.paper_fill_enabled {
        Some(SurfaceMaterialMode::PaperFill)
    } else if settings.wireframe_overlay_enabled {
        Some(SurfaceMaterialMode::WireframeOnly)
    } else if settings.xray_enabled {
        Some(SurfaceMaterialMode::Xray)
    } else {
        None
    }
}

fn display_override_material(
    source: &StandardMaterial,
    mode: SurfaceMaterialMode,
    xray_alpha: f32,
) -> StandardMaterial {
    match mode {
        SurfaceMaterialMode::PaperFill => paper_fill_material_from(source),
        SurfaceMaterialMode::WireframeOnly => wireframe_only_material_from(source),
        SurfaceMaterialMode::Xray => xray_material_from(source, xray_alpha),
    }
}

fn paper_fill_material_from(source: &StandardMaterial) -> StandardMaterial {
    let mut override_material = source.clone();
    override_material.base_color = Color::WHITE;
    override_material.emissive = LinearRgba::BLACK;
    override_material.perceptual_roughness = 1.0;
    override_material.metallic = 0.0;
    override_material.reflectance = 0.0;
    override_material.unlit = true;
    override_material.alpha_mode = AlphaMode::Opaque;
    override_material
}

fn xray_material_from(source: &StandardMaterial, xray_alpha: f32) -> StandardMaterial {
    let mut override_material = source.clone();
    override_material
        .base_color
        .set_alpha(xray_alpha.clamp(0.02, 0.95));
    override_material.base_color_texture = None;
    override_material.alpha_mode = AlphaMode::Blend;
    override_material.cull_mode = None;
    override_material
}

fn wireframe_only_material_from(source: &StandardMaterial) -> StandardMaterial {
    let mut override_material = source.clone();
    override_material.base_color = Color::srgba(0.0, 0.0, 0.0, 0.0);
    override_material.emissive = LinearRgba::BLACK;
    override_material.unlit = true;
    override_material.alpha_mode = AlphaMode::Mask(0.5);
    override_material.cull_mode = None;
    override_material
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
        let max_roughness = settings.ssr_perceptual_roughness_threshold.clamp(0.0, 1.0);
        commands.entity(camera).insert(ScreenSpaceReflections {
            min_perceptual_roughness: 0.0..0.0,
            max_perceptual_roughness: max_roughness..max_roughness,
            edge_fadeout: ScreenSpaceReflections::default().edge_fadeout,
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
    if !settings.wireframe_overlay_enabled && !settings.contour_overlay_enabled {
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
        Option<&WireframeSurfaceVisibilityOverride>,
    )>() else {
        return;
    };

    let mut subjects = Vec::new();
    for (entity, _element_id, mesh_handle, mesh_transform, visibility, wireframe_surface_hidden) in
        query.iter(world)
    {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden)
            && wireframe_surface_hidden.is_none()
        {
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

        // Live outline uses the fast authored linework path above. The older
        // mesh feature-edge classifier remains for tests/offline use, but not
        // as a per-frame viewport path.
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

fn wireframe_overlay_color(settings: &RenderSettings) -> Color {
    if settings.paper_fill_enabled {
        Color::BLACK
    } else {
        WIREFRAME_OVERLAY_COLOR
    }
}

fn drawing_overlay_excluded(type_name: &str) -> bool {
    matches!(
        type_name,
        "dimension_line" | "guide_line" | "scene_light" | "clipping_plane" | "group"
    )
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

fn outline_mesh_from_surface_mesh(mesh: &Mesh) -> Option<Mesh> {
    let positions = mesh_positions(mesh)?;
    let indices = mesh_triangle_indices(mesh, positions.len())?;
    let segments = collect_feature_edge_segments(&positions, &indices);
    if segments.is_empty() {
        return None;
    }

    let mut line_positions = Vec::with_capacity(segments.len() * 2);
    for (start, end) in segments {
        line_positions.push(start);
        line_positions.push(end);
    }

    let mut outline_mesh = Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default());
    outline_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, line_positions);
    Some(outline_mesh)
}

fn collect_feature_edge_segments(
    positions: &[[f32; 3]],
    indices: &[u32],
) -> Vec<([f32; 3], [f32; 3])> {
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

        let normal = (Vec3::from(local_b) - Vec3::from(local_a))
            .cross(Vec3::from(local_c) - Vec3::from(local_a))
            .normalize_or_zero();
        if normal.length_squared() <= f32::EPSILON {
            continue;
        }

        register_feature_edge(&mut edges, local_a, local_b, normal);
        register_feature_edge(&mut edges, local_b, local_c, normal);
        register_feature_edge(&mut edges, local_c, local_a, normal);
    }

    edges
        .into_values()
        .filter(FeatureEdgeState::is_visible_candidate)
        .map(|edge| (edge.start, edge.end))
        .collect()
}

#[cfg(test)]
fn collect_feature_edges(mesh: &Mesh, mesh_transform: &GlobalTransform) -> Vec<FeatureEdgeState> {
    let Some(positions) = mesh_positions(mesh) else {
        return Vec::new();
    };
    let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
        return Vec::new();
    };

    collect_feature_edge_segments(&positions, &indices)
        .into_iter()
        .map(|(start, end)| FeatureEdgeState {
            start,
            end,
            start_world: mesh_transform.transform_point(Vec3::from(start)),
            end_world: mesh_transform.transform_point(Vec3::from(end)),
            normals: [Vec3::ZERO; 2],
            total_faces: 1,
        })
        .collect()
}

fn register_feature_edge(
    edges: &mut HashMap<EdgeKey, FeatureEdgeState>,
    local_start: [f32; 3],
    local_end: [f32; 3],
    normal: Vec3,
) {
    let key = EdgeKey::from_points(local_start, local_end);
    let state = edges.entry(key).or_insert_with(|| FeatureEdgeState {
        start: local_start,
        end: local_end,
        #[cfg(test)]
        start_world: Vec3::from(local_start),
        #[cfg(test)]
        end_world: Vec3::from(local_end),
        normals: [Vec3::ZERO; 2],
        total_faces: 0,
    });
    let face_index = usize::from(state.total_faces.min(1));
    state.normals[face_index] = normal;
    state.total_faces = state.total_faces.saturating_add(1);
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
        None if vertex_count.is_multiple_of(3) => Some((0..vertex_count as u32).collect()),
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
    start: [f32; 3],
    end: [f32; 3],
    #[cfg(test)]
    start_world: Vec3,
    #[cfg(test)]
    end_world: Vec3,
    normals: [Vec3; 2],
    total_faces: u8,
}

impl FeatureEdgeState {
    fn is_visible_candidate(&self) -> bool {
        match self.total_faces {
            0 => false,
            1 => true,
            _ => self.normals[0].dot(self.normals[1]) <= FEATURE_EDGE_COS_THRESHOLD,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::ui::StatusBarData;
    use bevy::render::render_resource::Face;

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
        assert!(!settings.xray_enabled);
        assert_eq!(settings.xray_surface_alpha, DEFAULT_XRAY_SURFACE_ALPHA);
        assert_eq!(settings.background_rgb, DEFAULT_BACKGROUND_RGB);
    }

    #[test]
    fn paper_preset_enables_white_background_without_live_hidden_line() {
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
        assert!(!settings.xray_enabled);
        assert!(!settings.visible_edge_overlay_enabled);
        assert!(!settings.wireframe_overlay_enabled);
        assert!(!settings.contour_overlay_enabled);
    }

    #[test]
    fn xray_command_disables_paper_fill_and_toggles_transparent_surface_mode() {
        let mut app = App::new();
        app.insert_resource(RenderSettings {
            paper_fill_enabled: true,
            ..RenderSettings::default()
        })
        .insert_resource(StatusBarData::default());

        execute_toggle_xray(app.world_mut(), &Value::Null).expect("xray should toggle on");

        let settings = app.world().resource::<RenderSettings>();
        assert!(settings.xray_enabled);
        assert!(!settings.paper_fill_enabled);
        assert_eq!(
            active_surface_material_mode(settings),
            Some(SurfaceMaterialMode::Xray)
        );

        execute_toggle_xray(app.world_mut(), &Value::Null).expect("xray should toggle off");

        let settings = app.world().resource::<RenderSettings>();
        assert!(!settings.xray_enabled);
        assert_eq!(active_surface_material_mode(settings), None);
    }

    #[test]
    fn xray_command_accepts_explicit_state_for_mcp_invocation() {
        let mut app = App::new();
        app.insert_resource(RenderSettings::default())
            .insert_resource(StatusBarData::default());

        execute_toggle_xray(
            app.world_mut(),
            &serde_json::json!({
                "enabled": true,
                "xray_surface_alpha": 0.62
            }),
        )
        .expect("xray should turn on explicitly");

        let settings = app.world().resource::<RenderSettings>();
        assert!(settings.xray_enabled);
        assert_eq!(settings.xray_surface_alpha, 0.62);

        execute_toggle_xray(
            app.world_mut(),
            &serde_json::json!({
                "enabled": false
            }),
        )
        .expect("xray should turn off explicitly");

        let settings = app.world().resource::<RenderSettings>();
        assert!(!settings.xray_enabled);
        assert_eq!(settings.xray_surface_alpha, 0.62);
    }

    #[test]
    fn paper_fill_wins_when_material_modes_are_both_enabled() {
        let settings = RenderSettings {
            paper_fill_enabled: true,
            xray_enabled: true,
            ..RenderSettings::default()
        };

        assert_eq!(
            active_surface_material_mode(&settings),
            Some(SurfaceMaterialMode::PaperFill)
        );
    }

    #[test]
    fn xray_material_uses_bevy_transparent_blend_material() {
        let source = StandardMaterial {
            base_color: Color::srgba(0.2, 0.4, 0.6, 1.0),
            alpha_mode: AlphaMode::Opaque,
            cull_mode: Some(Face::Back),
            ..Default::default()
        };

        let xray = xray_material_from(&source, DEFAULT_XRAY_SURFACE_ALPHA);

        assert_eq!(xray.alpha_mode, AlphaMode::Blend);
        assert_eq!(xray.base_color.alpha(), DEFAULT_XRAY_SURFACE_ALPHA);
        assert_eq!(xray.cull_mode, None);
        assert_eq!(
            xray.base_color.to_srgba().red,
            source.base_color.to_srgba().red
        );
        assert_eq!(
            xray.base_color.to_srgba().green,
            source.base_color.to_srgba().green
        );
        assert_eq!(
            xray.base_color.to_srgba().blue,
            source.base_color.to_srgba().blue
        );
    }

    #[test]
    fn xray_material_drops_base_color_texture_so_alpha_controls_opacity() {
        let source = StandardMaterial {
            base_color: Color::srgba(0.2, 0.4, 0.6, 1.0),
            base_color_texture: Some(Handle::<Image>::default()),
            alpha_mode: AlphaMode::Opaque,
            ..Default::default()
        };

        let xray = xray_material_from(&source, 0.33);

        assert!(xray.base_color_texture.is_none());
        assert_eq!(xray.alpha_mode, AlphaMode::Blend);
        assert_eq!(xray.base_color.alpha(), 0.33);
    }

    #[test]
    fn xray_material_clamps_alpha_to_visible_transparency_bounds() {
        let source = StandardMaterial::default();

        assert_eq!(xray_material_from(&source, -1.0).base_color.alpha(), 0.02);
        assert_eq!(xray_material_from(&source, 2.0).base_color.alpha(), 0.95);
    }

    #[test]
    fn display_override_cache_reuses_xray_material_for_same_source_and_alpha() {
        let mut materials = Assets::<StandardMaterial>::default();
        let source_material = StandardMaterial {
            base_color: Color::srgba(0.2, 0.4, 0.6, 1.0),
            ..Default::default()
        };
        let source_handle = materials.add(source_material.clone());
        let mut cache = SurfaceMaterialOverrideCache::default();

        let first = cached_display_override_material(
            &mut cache,
            &mut materials,
            &source_handle,
            &source_material,
            SurfaceMaterialMode::Xray,
            0.28,
        );
        let second = cached_display_override_material(
            &mut cache,
            &mut materials,
            &source_handle,
            &source_material,
            SurfaceMaterialMode::Xray,
            0.28,
        );

        assert_eq!(first, second);
        assert_eq!(cache.handles.len(), 1);
    }

    #[test]
    fn display_override_cache_separates_xray_alpha_levels() {
        let mut materials = Assets::<StandardMaterial>::default();
        let source_material = StandardMaterial::default();
        let source_handle = materials.add(source_material.clone());
        let mut cache = SurfaceMaterialOverrideCache::default();

        let low_alpha = cached_display_override_material(
            &mut cache,
            &mut materials,
            &source_handle,
            &source_material,
            SurfaceMaterialMode::Xray,
            0.28,
        );
        let high_alpha = cached_display_override_material(
            &mut cache,
            &mut materials,
            &source_handle,
            &source_material,
            SurfaceMaterialMode::Xray,
            0.6,
        );

        assert_ne!(low_alpha, high_alpha);
        assert_eq!(cache.handles.len(), 2);
    }

    #[test]
    fn xray_override_state_expires_when_alpha_changes() {
        let mut settings = RenderSettings {
            xray_enabled: true,
            xray_surface_alpha: 0.25,
            ..RenderSettings::default()
        };
        let state = SurfaceMaterialOverride {
            original: Handle::default(),
            override_handle: Handle::default(),
            mode: SurfaceMaterialMode::Xray,
            xray_alpha: 0.25,
        };

        assert!(surface_material_override_is_current(
            &state,
            SurfaceMaterialMode::Xray,
            &settings
        ));

        settings.xray_surface_alpha = 0.4;

        assert!(!surface_material_override_is_current(
            &state,
            SurfaceMaterialMode::Xray,
            &settings
        ));
    }

    #[test]
    fn outline_command_toggles_gpu_depth_tested_feature_linework() {
        let mut app = App::new();
        app.insert_resource(RenderSettings {
            contour_overlay_enabled: true,
            ..RenderSettings::default()
        })
        .insert_resource(StatusBarData::default());

        execute_toggle_outline(app.world_mut(), &Value::Null).expect("outline should toggle on");
        let settings = app.world().resource::<RenderSettings>();
        assert!(settings.visible_edge_overlay_enabled);
        assert!(!settings.contour_overlay_enabled);
        assert!(live_depth_tested_outline_active(settings));

        execute_toggle_outline(app.world_mut(), &Value::Null).expect("outline should toggle off");
        let settings = app.world().resource::<RenderSettings>();
        assert!(!settings.visible_edge_overlay_enabled);
        assert!(!live_depth_tested_outline_active(settings));
    }

    #[test]
    fn wireframe_overlay_uses_line_only_surface_mode() {
        let settings = RenderSettings {
            wireframe_overlay_enabled: true,
            xray_enabled: false,
            paper_fill_enabled: false,
            ..RenderSettings::default()
        };

        assert_eq!(
            active_surface_material_mode(&settings),
            Some(SurfaceMaterialMode::WireframeOnly)
        );

        let source = StandardMaterial {
            base_color: Color::srgb(0.8, 0.2, 0.1),
            alpha_mode: AlphaMode::Opaque,
            ..Default::default()
        };
        let material = display_override_material(&source, SurfaceMaterialMode::WireframeOnly, 0.0);

        assert_eq!(material.alpha_mode, AlphaMode::Mask(0.5));
        assert_eq!(material.base_color.to_srgba().alpha, 0.0);
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
        assert!(!settings.visible_edge_overlay_enabled);
    }

    #[test]
    fn paper_fill_does_not_activate_live_outline_path() {
        let settings = RenderSettings {
            visible_edge_overlay_enabled: true,
            paper_fill_enabled: true,
            ..RenderSettings::default()
        };

        assert!(!live_depth_tested_outline_active(&settings));
    }

    #[test]
    fn outline_mesh_omits_coplanar_internal_diagonal() {
        let positions = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, Default::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_indices(Indices::U32(indices));

        let outline = outline_mesh_from_surface_mesh(&mesh).expect("outline mesh");
        let line_positions = mesh_positions(&outline).expect("line positions");

        assert_eq!(
            line_positions.len(),
            8,
            "four visible perimeter edges should emit two line vertices each"
        );
    }

    #[test]
    fn outline_material_is_depth_tested_visible_edge_linework() {
        let mut cache = OutlineMeshOverlayMaterial::default();
        let mut materials = Assets::<StandardMaterial>::default();

        let handle =
            outline_overlay_material(&mut cache, &mut materials, VISIBLE_EDGE_OVERLAY_COLOR);
        let material = materials.get(&handle).expect("outline material");

        assert_eq!(material.alpha_mode, AlphaMode::Opaque);
        assert!(material.unlit);
        assert_eq!(material.depth_bias, 1.0);
    }

    #[test]
    fn feature_edges_collect_cube_outer_edges_without_visibility_sampling() {
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
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_indices(Indices::U32(indices));

        let edges = collect_feature_edges(&mesh, &GlobalTransform::IDENTITY);

        assert_eq!(edges.len(), 12);
        assert!(edges.iter().any(|edge| {
            edge.start_world == Vec3::new(-1.0, -1.0, -1.0)
                && edge.end_world == Vec3::new(1.0, -1.0, -1.0)
        }));
    }
}
