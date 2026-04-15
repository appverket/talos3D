use bevy::window::PrimaryWindow;
use bevy::{ecs::system::SystemParam, picking::prelude::*, prelude::*};

use super::document_properties::DocumentProperties;
use crate::plugins::egui_chrome::EguiWantsInput;
#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::plugins::scene_ray;
use crate::plugins::{
    camera::{CameraProjectionMode, OrbitCamera},
    identity::ElementId,
    modeling::profile_feature::FaceProfileFeature,
    tools::ActiveTool,
};

const CROSSHAIR_HALF_SIZE: f32 = 0.15;
const CROSSHAIR_COLOR: Color = Color::srgb(1.0, 0.95, 0.4);

pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CursorWorldPos>()
            .init_resource::<DrawingPlane>()
            .init_resource::<ViewportUiInset>()
            .add_systems(Update, (update_cursor_world_pos, draw_cursor_crosshair));
    }
}

#[derive(Resource, Default)]
pub struct CursorWorldPos {
    pub raw: Option<Vec3>,
    pub snapped: Option<Vec3>,
    pub hovered_element_id: Option<ElementId>,
}

#[derive(Resource, Default)]
pub struct ViewportUiInset {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(SystemParam)]
struct ToolCursorRayCast<'w, 's> {
    ray_cast: MeshRayCast<'w, 's>,
    mesh_selectable_query: Query<'w, 's, (), With<ElementId>>,
    element_id_query: Query<'w, 's, &'static ElementId>,
    visibility_query: Query<'w, 's, &'static Visibility>,
    face_profile_feature_query: Query<'w, 's, (), With<FaceProfileFeature>>,
}

// ---------------------------------------------------------------------------
// DrawingPlane — the plane that tools project the cursor onto
// ---------------------------------------------------------------------------

/// The plane that drawing tools project the cursor onto.
///
/// Default: Y=0 ground plane. Set to a face's plane when face-editing.
/// All tools that read `CursorWorldPos` automatically get face-aware
/// projection without any per-tool changes.
#[derive(Resource, Debug, Clone)]
pub struct DrawingPlane {
    /// A point on the plane.
    pub origin: Vec3,
    /// The plane normal (outward-facing).
    pub normal: Vec3,
    /// Local X axis on the plane.
    pub tangent: Vec3,
    /// Local Y axis on the plane.
    pub bitangent: Vec3,
}

impl Default for DrawingPlane {
    fn default() -> Self {
        Self::ground()
    }
}

impl DrawingPlane {
    /// The ground plane at Y=0.
    pub fn ground() -> Self {
        Self {
            origin: Vec3::ZERO,
            normal: Vec3::Y,
            tangent: Vec3::X,
            bitangent: Vec3::Z,
        }
    }

    /// A plane from a face centroid and normal.
    pub fn from_face(centroid: Vec3, normal: Vec3) -> Self {
        let normal = normal.normalize_or_zero();
        let (tangent, bitangent) = normal_basis(normal);
        Self {
            origin: centroid,
            normal,
            tangent,
            bitangent,
        }
    }

    /// Project a world-space point onto the plane's 2D coordinate system.
    pub fn project_to_2d(&self, point: Vec3) -> Vec2 {
        let d = point - self.origin;
        Vec2::new(d.dot(self.tangent), d.dot(self.bitangent))
    }

    /// Convert 2D plane coordinates back to world space.
    pub fn to_world(&self, uv: Vec2) -> Vec3 {
        self.origin + self.tangent * uv.x + self.bitangent * uv.y
    }

    /// Intersect a ray with this plane.
    pub fn intersect_ray(&self, ray: Ray3d) -> Option<Vec3> {
        scene_ray::project_ray_to_plane(ray, self.origin, self.normal)
    }

    /// Whether this is the default ground plane.
    pub fn is_ground(&self) -> bool {
        (self.normal - Vec3::Y).length_squared() < 1e-6 && self.origin.y.abs() < 1e-6
    }
}

pub fn dimension_annotation_plane(
    drawing_plane: &DrawingPlane,
    orbit: Option<&OrbitCamera>,
    camera_transform: Option<&GlobalTransform>,
) -> DrawingPlane {
    match (orbit, camera_transform) {
        (Some(orbit), Some(camera_transform))
            if orbit.projection_mode == CameraProjectionMode::Isometric =>
        {
            DrawingPlane::from_face(orbit.focus, Vec3::from(camera_transform.forward()))
        }
        _ => drawing_plane.clone(),
    }
}

/// Build an orthonormal tangent frame from a normal vector.
fn normal_basis(normal: Vec3) -> (Vec3, Vec3) {
    let seed = if normal.y.abs() > 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let tangent = normal.cross(seed).normalize_or_zero();
    let bitangent = tangent.cross(normal).normalize_or_zero();
    (tangent, bitangent)
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

fn update_cursor_world_pos(
    mut cursor_world_pos: ResMut<CursorWorldPos>,
    drawing_plane: Res<DrawingPlane>,
    window_query: Query<&Window, With<PrimaryWindow>>,
    camera_query: Query<(&Camera, &GlobalTransform, Option<&OrbitCamera>)>,
    egui_wants_input: Res<EguiWantsInput>,
    doc_props: Res<DocumentProperties>,
    active_tool: Res<State<ActiveTool>>,
    mut tool_cursor_ray_cast: ToolCursorRayCast,
) {
    if egui_wants_input.pointer {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    }

    let Some((camera, camera_transform, orbit)) = camera_query.iter().next() else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let Ok(window) = window_query.single() else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let Some(cursor_position) = window.cursor_position() else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let viewport_cursor = match camera.logical_viewport_rect() {
        Some(rect) => cursor_position - rect.min,
        None => cursor_position,
    };

    let Ok(ray) = camera.viewport_to_world(camera_transform, viewport_cursor) else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let use_scene_surface_cursor = matches!(
        active_tool.get(),
        ActiveTool::PlaceDimensionLine | ActiveTool::PlaceGuideLine
    );
    let cursor_plane = if *active_tool.get() == ActiveTool::PlaceDimensionLine {
        dimension_annotation_plane(&drawing_plane, orbit, Some(camera_transform))
    } else {
        drawing_plane.clone()
    };
    let surface_hit = if use_scene_surface_cursor {
        ray_cast_scene_surface(ray, &mut tool_cursor_ray_cast)
    } else {
        None
    };
    let raw_position = surface_hit
        .map(|(position, _)| position)
        .or_else(|| cursor_plane.intersect_ray(ray));
    let Some(raw_position) = raw_position else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let snapped_position = if use_scene_surface_cursor {
        raw_position
    } else {
        let uv = cursor_plane.project_to_2d(raw_position);
        let snap = doc_props.snap_increment;
        let snapped_uv = Vec2::new(snap_to_increment(uv.x, snap), snap_to_increment(uv.y, snap));
        cursor_plane.to_world(snapped_uv)
    };

    cursor_world_pos.raw = Some(raw_position);
    cursor_world_pos.snapped = Some(snapped_position);
    cursor_world_pos.hovered_element_id = surface_hit.and_then(|(_, entity)| {
        tool_cursor_ray_cast
            .element_id_query
            .get(entity)
            .ok()
            .copied()
    });
}

fn ray_cast_scene_surface(ray: Ray3d, ray_cast: &mut ToolCursorRayCast) -> Option<(Vec3, Entity)> {
    ray_cast
        .ray_cast
        .cast_ray(
            ray,
            &MeshRayCastSettings::default().with_filter(&|entity| {
                ray_cast.mesh_selectable_query.contains(entity)
                    && !ray_cast.face_profile_feature_query.contains(entity)
                    && ray_cast
                        .visibility_query
                        .get(entity)
                        .map_or(true, |visibility| *visibility != Visibility::Hidden)
            }),
        )
        .first()
        .map(|(entity, hit)| (ray.origin + *ray.direction * hit.distance, *entity))
}

fn draw_cursor_crosshair(
    cursor_world_pos: Res<CursorWorldPos>,
    drawing_plane: Res<DrawingPlane>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    let Some(position) = cursor_world_pos.snapped else {
        return;
    };

    // Draw crosshair along the drawing plane's axes
    gizmos.line(
        position - drawing_plane.tangent * CROSSHAIR_HALF_SIZE,
        position + drawing_plane.tangent * CROSSHAIR_HALF_SIZE,
        CROSSHAIR_COLOR,
    );
    gizmos.line(
        position - drawing_plane.bitangent * CROSSHAIR_HALF_SIZE,
        position + drawing_plane.bitangent * CROSSHAIR_HALF_SIZE,
        CROSSHAIR_COLOR,
    );
    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, 2);
}

fn clear_cursor_world_pos(cursor_world_pos: &mut CursorWorldPos) {
    cursor_world_pos.raw = None;
    cursor_world_pos.snapped = None;
    cursor_world_pos.hovered_element_id = None;
}

fn snap_to_increment(value: f32, increment: f32) -> f32 {
    (value / increment).round() * increment
}
