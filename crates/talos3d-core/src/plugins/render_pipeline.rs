/// Real-time render pipeline configuration for Talos3D.
///
/// Adds SSAO, Bloom, optional SSR, and AgX tonemapping to the scene camera.
/// Exposes [`RenderSettings`] as a hot-reloadable resource: toggling any flag
/// takes effect on the very next frame.
use bevy::{
    camera::Exposure,
    core_pipeline::tonemapping::Tonemapping,
    pbr::{
        ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel,
        ScreenSpaceReflections,
    },
    post_process::bloom::Bloom,
    prelude::*,
};

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
        }
    }
}

// ─── Plugin ──────────────────────────────────────────────────────────────────

/// Registers [`RenderSettings`] and wires up the camera post-processing stack.
pub struct RenderPipelinePlugin;

impl Plugin for RenderPipelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderSettings>()
            // PostStartup ensures the camera plugin has already run Startup.
            .add_systems(PostStartup, setup_render_pipeline)
            .add_systems(Update, sync_render_settings);
    }
}

// ─── Startup system ──────────────────────────────────────────────────────────

fn setup_render_pipeline(
    mut commands: Commands,
    settings: Res<RenderSettings>,
    camera_query: Query<Entity, With<Camera3d>>,
) {
    let Ok(camera) = camera_query.single() else {
        warn!("RenderPipelinePlugin: no Camera3d found at startup");
        return;
    };

    // SSAO requires MSAA disabled — set per-camera.
    commands.entity(camera).insert(Msaa::Off);

    commands
        .entity(camera)
        .insert(settings.tonemapping.to_bevy())
        .insert(Exposure {
            ev100: settings.exposure_ev100,
        });

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
        .insert(settings.tonemapping.to_bevy())
        .insert(Exposure {
            ev100: settings.exposure_ev100,
        });

    sync_ssao_component(&mut commands, camera, &settings);
    sync_bloom_component(&mut commands, camera, &settings);
    sync_ssr_component(&mut commands, camera, &settings);
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
