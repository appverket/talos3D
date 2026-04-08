/// Real-time render pipeline configuration for Talos3D.
///
/// Adds SSAO, Bloom, optional SSR, and AgX tonemapping to the scene camera.
/// Exposes [`RenderSettings`] as a hot-reloadable resource: toggling any flag
/// takes effect on the very next frame.
use bevy::{
    core_pipeline::tonemapping::Tonemapping,
    pbr::{
        ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel,
        ScreenSpaceReflections,
    },
    post_process::bloom::Bloom,
    prelude::*,
};

// ─── Settings resource ───────────────────────────────────────────────────────

/// Hot-reloadable render quality settings.
///
/// Insert or mutate this resource to toggle effects at runtime.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct RenderSettings {
    /// Enable screen-space ambient occlusion.
    pub ssao_enabled: bool,
    /// Enable bloom post-processing.
    pub bloom_enabled: bool,
    /// Bloom intensity (linear scale applied on top of `Bloom::default()`).
    pub bloom_intensity: f32,
    /// Enable screen-space reflections (requires deferred prepass; GPU-heavy).
    pub ssr_enabled: bool,
    /// SSAO quality: 0 = Low, 1 = Medium, 2 = High.
    pub ambient_occlusion_quality: u8,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            ssao_enabled: true,
            bloom_enabled: true,
            bloom_intensity: 0.15,
            ssr_enabled: false,
            ambient_occlusion_quality: 1,
        }
    }
}

// ─── Plugin ──────────────────────────────────────────────────────────────────

/// Registers [`RenderSettings`] and wires up the camera post-processing stack.
pub struct RenderPipelinePlugin;

impl Plugin for RenderPipelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderSettings>()
            .add_systems(Startup, setup_render_pipeline)
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

    // AgX tonemapping — perceptually uniform, great for architectural previews.
    commands.entity(camera).insert(Tonemapping::AgX);

    // SSAO — the #[require(DepthPrepass, NormalPrepass)] on the component
    // automatically inserts the prepass components as well.
    if settings.ssao_enabled {
        let quality = ssao_quality(settings.ambient_occlusion_quality);
        commands.entity(camera).insert(ScreenSpaceAmbientOcclusion {
            quality_level: quality,
            constant_object_thickness: 0.1,
        });
    }

    // Bloom
    if settings.bloom_enabled {
        commands.entity(camera).insert(Bloom {
            intensity: settings.bloom_intensity,
            ..Bloom::default()
        });
    }

    // SSR (off by default — too expensive for interactive modelling).
    if settings.ssr_enabled {
        commands
            .entity(camera)
            .insert(ScreenSpaceReflections::default());
    }
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

    // SSAO
    if settings.ssao_enabled {
        let quality = ssao_quality(settings.ambient_occlusion_quality);
        commands.entity(camera).insert(ScreenSpaceAmbientOcclusion {
            quality_level: quality,
            constant_object_thickness: 0.1,
        });
    } else {
        commands
            .entity(camera)
            .remove::<ScreenSpaceAmbientOcclusion>();
    }

    // Bloom
    if settings.bloom_enabled {
        commands.entity(camera).insert(Bloom {
            intensity: settings.bloom_intensity,
            ..Bloom::default()
        });
    } else {
        commands.entity(camera).remove::<Bloom>();
    }

    // SSR
    if settings.ssr_enabled {
        commands
            .entity(camera)
            .insert(ScreenSpaceReflections::default());
    } else {
        commands.entity(camera).remove::<ScreenSpaceReflections>();
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn ssao_quality(level: u8) -> ScreenSpaceAmbientOcclusionQualityLevel {
    match level {
        0 => ScreenSpaceAmbientOcclusionQualityLevel::Low,
        2 => ScreenSpaceAmbientOcclusionQualityLevel::High,
        _ => ScreenSpaceAmbientOcclusionQualityLevel::Medium,
    }
}
