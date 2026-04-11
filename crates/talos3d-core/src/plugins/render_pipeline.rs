/// Real-time render pipeline configuration for Talos3D.
///
/// Adds SSAO, Bloom, optional SSR, and AgX tonemapping to the scene camera.
/// Exposes [`RenderSettings`] as a hot-reloadable resource: toggling any flag
/// takes effect on the very next frame.
use bevy::{
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
use std::collections::HashMap;

use crate::{
    capability_registry::CapabilityRegistry,
    plugins::{identity::ElementId, modeling::mesh_generation::MeshGenerationSet},
};

const WIREFRAME_OVERLAY_COLOR: Color = Color::srgba(0.12, 0.13, 0.15, 1.0);
const CONTOUR_OVERLAY_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 1.0);
const EDGE_QUANTIZATION_SCALE: f32 = 10_000.0;

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
            .add_systems(Update, sync_render_settings)
            .add_systems(
                Update,
                draw_model_edge_overlays.after(MeshGenerationSet::Generate),
            );
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

fn draw_model_edge_overlays(
    world: &World,
    settings: Res<RenderSettings>,
    registry: Res<CapabilityRegistry>,
    mesh_assets: Res<Assets<Mesh>>,
    camera_query: Query<(&GlobalTransform, &Projection), With<Camera3d>>,
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
    )>() else {
        return;
    };

    for (entity, _element_id, mesh_handle, mesh_transform, visibility) in query.iter(world) {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        if drawing_overlay_excluded(snapshot.type_name()) {
            continue;
        }

        if settings.wireframe_overlay_enabled {
            if let Some(factory) = registry.factory_for(snapshot.type_name()) {
                factory.draw_selection(world, entity, &mut gizmos, WIREFRAME_OVERLAY_COLOR);
            }
        }

        if settings.contour_overlay_enabled {
            let Some(mesh) = mesh_assets.get(mesh_handle.id()) else {
                continue;
            };
            draw_mesh_contours(
                mesh,
                mesh_transform,
                camera_position,
                camera_forward,
                orthographic,
                &mut gizmos,
                CONTOUR_OVERLAY_COLOR,
            );
        }
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
        let normal = (world_b - world_a).cross(world_c - world_a).normalize_or_zero();
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
