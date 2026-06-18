#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

use bevy::window::{PrimaryWindow, Window};
use bevy::{ecs::system::SystemParam, picking::prelude::*, prelude::*};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{closure::Closure, JsCast};

use super::document_properties::DocumentProperties;
use crate::plugins::egui_chrome::{EguiChromeSystems, EguiWantsInput};
#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::plugins::scene_ray;
use crate::plugins::{
    camera::{CameraProjectionMode, OrbitCamera},
    identity::ElementId,
    layers::{LayerAssignment, LayerRegistry},
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
            .configure_sets(
                Update,
                (
                    CursorSystems::UpdateWorldPosition,
                    CursorSystems::DrawCrosshair,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                update_cursor_world_pos
                    .in_set(CursorSystems::UpdateWorldPosition)
                    .after(EguiChromeSystems),
            )
            .add_systems(
                Update,
                draw_cursor_crosshair.in_set(CursorSystems::DrawCrosshair),
            );

        #[cfg(target_arch = "wasm32")]
        app.add_systems(Startup, install_browser_canvas_cursor_mapping);
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum CursorSystems {
    UpdateWorldPosition,
    DrawCrosshair,
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
    layer_assignment_query: Query<'w, 's, Option<&'static LayerAssignment>>,
    layer_registry: Res<'w, LayerRegistry>,
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

#[cfg(target_arch = "wasm32")]
thread_local! {
    static BROWSER_CANVAS_CURSOR_POSITION: RefCell<Option<Vec2>> = RefCell::new(None);
}

/// Return the cursor position in camera viewport logical pixels.
///
/// Native builds use Bevy's window cursor directly. Web builds prefer a browser
/// canvas event mapping, because shells can differ in how DOM/client pixels are
/// routed into winit after UI interaction. That environment-specific mapping is
/// normalized here so tools and ray-casting code can remain generic.
pub fn cursor_viewport_position(window: &Window, camera: &Camera) -> Option<Vec2> {
    let cursor_position = platform_window_cursor_position(window)?;
    Some(match camera.logical_viewport_rect() {
        Some(rect) => cursor_position - rect.min,
        None => cursor_position,
    })
}

/// Return the cursor position in Bevy window logical pixels.
pub fn cursor_window_position(window: &Window) -> Option<Vec2> {
    platform_window_cursor_position(window)
}

#[cfg(not(target_arch = "wasm32"))]
fn platform_window_cursor_position(window: &Window) -> Option<Vec2> {
    window.cursor_position()
}

#[cfg(target_arch = "wasm32")]
fn platform_window_cursor_position(window: &Window) -> Option<Vec2> {
    browser_canvas_cursor_position(window).or_else(|| window.cursor_position())
}

#[cfg(target_arch = "wasm32")]
fn install_browser_canvas_cursor_mapping() {
    let Some(canvas) = browser_canvas() else {
        return;
    };

    for event_name in ["pointermove", "pointerdown", "pointerup"] {
        let canvas_for_event = canvas.clone();
        let closure = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |event| {
            update_browser_canvas_cursor_position(&canvas_for_event, &event);
        });
        let _ =
            canvas.add_event_listener_with_callback(event_name, closure.as_ref().unchecked_ref());
        closure.forget();
    }

    let closure = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |_| {
        BROWSER_CANVAS_CURSOR_POSITION.with(|position| {
            *position.borrow_mut() = None;
        });
    });
    let _ =
        canvas.add_event_listener_with_callback("pointerleave", closure.as_ref().unchecked_ref());
    closure.forget();
}

#[cfg(target_arch = "wasm32")]
fn update_browser_canvas_cursor_position(
    canvas: &web_sys::HtmlCanvasElement,
    event: &web_sys::PointerEvent,
) {
    let rect = canvas.get_bounding_client_rect();
    let width = rect.width() as f32;
    let height = rect.height() as f32;
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let x = (event.client_x() as f64 - rect.left()) as f32;
    let y = (event.client_y() as f64 - rect.top()) as f32;
    let inside = x >= 0.0 && y >= 0.0 && x <= width && y <= height;
    BROWSER_CANVAS_CURSOR_POSITION.with(|position| {
        *position.borrow_mut() = inside.then_some(Vec2::new(x, y));
    });
}

#[cfg(target_arch = "wasm32")]
fn browser_canvas_cursor_position(window: &Window) -> Option<Vec2> {
    let cursor_position = BROWSER_CANVAS_CURSOR_POSITION.with(|position| *position.borrow())?;
    let canvas_size =
        browser_canvas_css_size().unwrap_or_else(|| Vec2::new(window.width(), window.height()));
    if canvas_size.x <= 0.0 || canvas_size.y <= 0.0 {
        return Some(cursor_position);
    }
    Some(Vec2::new(
        cursor_position.x * window.width() / canvas_size.x,
        cursor_position.y * window.height() / canvas_size.y,
    ))
}

#[cfg(target_arch = "wasm32")]
fn browser_canvas_css_size() -> Option<Vec2> {
    let rect = browser_canvas()?.get_bounding_client_rect();
    Some(Vec2::new(rect.width() as f32, rect.height() as f32))
}

#[cfg(target_arch = "wasm32")]
fn browser_canvas() -> Option<web_sys::HtmlCanvasElement> {
    web_sys::window()?
        .document()?
        .query_selector("#bevy")
        .ok()
        .flatten()?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .ok()
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
            orthogonal_dimension_plane(orbit.focus, camera_transform)
        }
        _ => drawing_plane.clone(),
    }
}

fn orthogonal_dimension_plane(origin: Vec3, camera_transform: &GlobalTransform) -> DrawingPlane {
    let normal = snapped_orthogonal_normal(Vec3::from(camera_transform.forward()));
    let mut tangent = reject_from_plane(Vec3::from(camera_transform.right()), normal)
        .try_normalize()
        .unwrap_or_else(|| orthogonal_plane_basis(normal).0);
    let bitangent = reject_from_plane(Vec3::from(camera_transform.up()), normal)
        .try_normalize()
        .or_else(|| normal.cross(tangent).try_normalize())
        .unwrap_or_else(|| orthogonal_plane_basis(normal).1);
    tangent = (tangent - bitangent * tangent.dot(bitangent))
        .try_normalize()
        .unwrap_or(tangent);
    DrawingPlane {
        origin,
        normal,
        tangent,
        bitangent,
    }
}

fn snapped_orthogonal_normal(direction: Vec3) -> Vec3 {
    let abs = direction.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        Vec3::X * non_zero_sign(direction.x)
    } else if abs.y >= abs.z {
        Vec3::Y * non_zero_sign(direction.y)
    } else {
        Vec3::Z * non_zero_sign(direction.z)
    }
}

fn non_zero_sign(value: f32) -> f32 {
    if value < 0.0 {
        -1.0
    } else {
        1.0
    }
}

fn orthogonal_plane_basis(normal: Vec3) -> (Vec3, Vec3) {
    if normal.x.abs() > 0.9 {
        (Vec3::Z, Vec3::Y)
    } else if normal.y.abs() > 0.9 {
        (Vec3::X, Vec3::Z)
    } else {
        (Vec3::X, Vec3::Y)
    }
}

fn reject_from_plane(vector: Vec3, normal: Vec3) -> Vec3 {
    vector - normal * vector.dot(normal)
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

    let Some((camera, camera_transform, orbit)) =
        active_orbit_camera(camera_query.iter()).or_else(|| camera_query.iter().next())
    else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let Ok(window) = window_query.single() else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let Some(viewport_cursor) = cursor_viewport_position(window, camera) else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let Ok(ray) = camera.viewport_to_world(camera_transform, viewport_cursor) else {
        clear_cursor_world_pos(&mut cursor_world_pos);
        return;
    };

    let use_scene_surface_cursor = matches!(
        active_tool.get(),
        ActiveTool::PlaceDimensionLine
            | ActiveTool::PlaceGuideLine
            | ActiveTool::PlaceTerrainElevationCurve
            | ActiveTool::PlaceTerrainSpotElevation
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

fn active_orbit_camera<'a>(
    cameras: impl Iterator<Item = (&'a Camera, &'a GlobalTransform, Option<&'a OrbitCamera>)>,
) -> Option<(&'a Camera, &'a GlobalTransform, Option<&'a OrbitCamera>)> {
    cameras
        .into_iter()
        .find(|(camera, _, orbit)| camera.is_active && orbit.is_some())
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
                    && ray_cast
                        .layer_assignment_query
                        .get(entity)
                        .ok()
                        .flatten()
                        .map(|assignment| ray_cast.layer_registry.is_visible(&assignment.layer))
                        .unwrap_or(true)
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
