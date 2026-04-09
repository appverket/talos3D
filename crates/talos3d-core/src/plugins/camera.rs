use bevy::{
    camera::ScalingMode,
    ecs::system::SystemParam,
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
    input::touch::TouchPhase,
    prelude::*,
};

use crate::authored_entity::EntityBounds;
use crate::plugins::{
    cursor::ViewportUiInset,
    egui_chrome::EguiWantsInput,
    input_ownership::InputPhase,
    toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt},
};

pub struct CameraPlugin;

pub const CAMERA_TOOLBAR_ID: &str = "camera.controls";
const DEFAULT_FOCAL_LENGTH_MM: f32 = 50.0;
const MIN_FOCAL_LENGTH_MM: f32 = 12.0;
const MAX_FOCAL_LENGTH_MM: f32 = 200.0;
const FULL_FRAME_SENSOR_HEIGHT_MM: f32 = 24.0;
const ORTHOGRAPHIC_VIEWPORT_HEIGHT: f32 = 2.0;
const TRUE_ISOMETRIC_PITCH: f32 = -0.615_479_7; // -atan(1 / sqrt(2))

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TrackpadState>()
            .init_resource::<CameraControlsState>()
            .register_toolbar(ToolbarDescriptor {
                id: CAMERA_TOOLBAR_ID.to_string(),
                label: "Camera".to_string(),
                default_dock: ToolbarDock::Top,
                default_visible: false,
                sections: Vec::new(),
            })
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    apply_camera_controls,
                    orbit_camera.in_set(InputPhase::CameraInput),
                    update_camera_viewport,
                ),
            );
    }
}

/// Tracks the centroid of a three-finger trackpad gesture between frames.
#[derive(Resource, Default)]
struct TrackpadState {
    prev_centroid: Option<Vec2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CameraProjectionMode {
    Perspective,
    Isometric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraViewPreset {
    Isometric,
    Top,
    Left,
    Right,
    Bottom,
}

#[derive(Resource, Debug, Clone)]
pub struct CameraControlsState {
    pub projection_mode: CameraProjectionMode,
    pub focal_length_mm: f32,
    pub pending_view_preset: Option<CameraViewPreset>,
}

impl Default for CameraControlsState {
    fn default() -> Self {
        Self {
            projection_mode: CameraProjectionMode::Perspective,
            focal_length_mm: DEFAULT_FOCAL_LENGTH_MM,
            pending_view_preset: None,
        }
    }
}

/// Orbit camera state stored as a component.
#[derive(Component)]
pub struct OrbitCamera {
    pub focus: Vec3,
    pub radius: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub projection_mode: CameraProjectionMode,
    pub focal_length_mm: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            radius: 15.0,
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: -std::f32::consts::FRAC_PI_6, // negative = above the grid
            projection_mode: CameraProjectionMode::Perspective,
            focal_length_mm: DEFAULT_FOCAL_LENGTH_MM,
        }
    }
}

fn spawn_camera(mut commands: Commands) {
    let orbit = OrbitCamera::default();
    let mut transform = orbit_transform(&orbit);
    let mut projection = Projection::Perspective(PerspectiveProjection::default());
    apply_orbit_state(&orbit, &mut transform, &mut projection);
    commands.spawn((Camera3d::default(), projection, transform, orbit));
}

#[derive(SystemParam)]
struct OrbitCameraInput<'w, 's> {
    mouse_buttons: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    egui_wants_input: Res<'w, EguiWantsInput>,
    motion: MessageReader<'w, 's, MouseMotion>,
    scroll: MessageReader<'w, 's, MouseWheel>,
    touch_events: MessageReader<'w, 's, TouchInput>,
    touches: Res<'w, Touches>,
    trackpad: ResMut<'w, TrackpadState>,
    query: Query<
        'w,
        's,
        (
            &'static mut OrbitCamera,
            &'static mut Transform,
            &'static mut Projection,
        ),
    >,
}

fn orbit_camera(mut input: OrbitCameraInput) {
    if input.egui_wants_input.pointer {
        input.motion.clear();
        input.scroll.clear();
        input.touch_events.clear();
        input.trackpad.prev_centroid = None;
        return;
    }

    let Ok((mut orbit, mut transform, mut projection)) = input.query.single_mut() else {
        return;
    };

    // --- Three-finger trackpad orbit ---
    // Consume touch events to detect phase changes (ended/cancelled resets state).
    for ev in input.touch_events.read() {
        if matches!(ev.phase, TouchPhase::Ended | TouchPhase::Canceled) {
            input.trackpad.prev_centroid = None;
        }
    }

    let active_touches: Vec<Vec2> = input.touches.iter().map(|t| t.position()).collect();
    if active_touches.len() == 3 {
        let centroid = active_touches.iter().copied().sum::<Vec2>() / 3.0;
        if let Some(prev) = input.trackpad.prev_centroid {
            let delta = centroid - prev;
            orbit.yaw -= delta.x * 0.005;
            orbit.pitch = (orbit.pitch - delta.y * 0.005).clamp(
                -std::f32::consts::FRAC_PI_2 + 0.05,
                std::f32::consts::FRAC_PI_2 - 0.05,
            );
        }
        input.trackpad.prev_centroid = Some(centroid);
    } else if active_touches.len() != 3 {
        input.trackpad.prev_centroid = None;
    }

    // --- Alt/Option+drag orbit / Shift+right-drag pan ---
    // Right-click without drag is reserved for the viewport context menu.
    let shift = input.keys.pressed(KeyCode::ShiftLeft) || input.keys.pressed(KeyCode::ShiftRight);
    let orbit_modifier = orbit_modifier_pressed(&input.keys);
    let right_pressed = input.mouse_buttons.pressed(MouseButton::Right);
    let any_mouse_pressed = input.mouse_buttons.pressed(MouseButton::Left) || right_pressed;
    let orbiting = orbit_modifier && any_mouse_pressed;
    let panning = input.mouse_buttons.pressed(MouseButton::Middle) || (right_pressed && shift);

    for ev in input.motion.read() {
        if orbiting {
            orbit.yaw -= ev.delta.x * 0.005;
            orbit.pitch = (orbit.pitch - ev.delta.y * 0.005).clamp(
                -std::f32::consts::FRAC_PI_2 + 0.05,
                std::f32::consts::FRAC_PI_2 - 0.05,
            );
        }
        if panning {
            let right = transform.rotation * Vec3::X;
            let up = transform.rotation * Vec3::Y;
            let radius = orbit.radius;
            orbit.focus -= right * ev.delta.x * radius * 0.001;
            orbit.focus += up * ev.delta.y * radius * 0.001;
        }
    }

    // --- Scroll to zoom ---
    for ev in input.scroll.read() {
        let delta = match ev.unit {
            MouseScrollUnit::Line => ev.y * 0.5,
            MouseScrollUnit::Pixel => ev.y * 0.002,
        };
        orbit.radius = (orbit.radius - delta * orbit.radius * 0.1).clamp(0.5, 500.0);
    }

    apply_orbit_state(&orbit, &mut transform, &mut projection);
}

pub fn orbit_modifier_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

fn orbit_transform(orbit: &OrbitCamera) -> Transform {
    let rotation = Quat::from_euler(EulerRot::YXZ, orbit.yaw, orbit.pitch, 0.0);
    let offset = rotation * Vec3::new(0.0, 0.0, orbit.radius);
    let translation = orbit.focus + offset;
    let forward = (orbit.focus - translation).normalize_or_zero();
    let up = if forward.dot(Vec3::Y).abs() > 0.999 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    Transform::from_translation(translation).looking_at(orbit.focus, up)
}

pub(crate) fn frame_orbit_camera(
    orbit: &mut OrbitCamera,
    transform: &mut Transform,
    projection: &mut Projection,
    focus: Vec3,
    radius: f32,
) {
    orbit.focus = focus;
    orbit.radius = radius.max(0.5);
    apply_orbit_state(orbit, transform, projection);
}

pub fn focus_orbit_camera_on_bounds(world: &mut World, bounds: EntityBounds) -> bool {
    let mut query = world.query::<(&mut OrbitCamera, &mut Transform, &mut Projection)>();
    let Some((mut orbit, mut transform, mut projection)) = query.iter_mut(world).next() else {
        return false;
    };
    let radius = orbit_frame_radius(bounds);
    frame_orbit_camera(
        &mut orbit,
        &mut transform,
        &mut projection,
        bounds.center(),
        radius,
    );
    true
}

pub fn orbit_frame_radius(bounds: EntityBounds) -> f32 {
    let extents = bounds.max - bounds.min;
    extents.length().max(10.0) * 1.25
}

fn apply_camera_controls(
    mut controls: ResMut<CameraControlsState>,
    mut query: Query<(&mut OrbitCamera, &mut Transform, &mut Projection)>,
) {
    let Ok((mut orbit, mut transform, mut projection)) = query.single_mut() else {
        return;
    };

    let mut changed = false;
    let next_focal_length = controls
        .focal_length_mm
        .clamp(MIN_FOCAL_LENGTH_MM, MAX_FOCAL_LENGTH_MM);
    if (orbit.focal_length_mm - next_focal_length).abs() > f32::EPSILON {
        orbit.focal_length_mm = next_focal_length;
        changed = true;
    }
    if orbit.projection_mode != controls.projection_mode {
        orbit.projection_mode = controls.projection_mode;
        changed = true;
    }
    if let Some(preset) = controls.pending_view_preset.take() {
        orbit.apply_view_preset(preset);
        changed = true;
    }

    if changed {
        apply_orbit_state(&orbit, &mut transform, &mut projection);
    }

    controls.focal_length_mm = orbit.focal_length_mm;
    controls.projection_mode = orbit.projection_mode;
}

pub(crate) fn apply_orbit_state(orbit: &OrbitCamera, transform: &mut Transform, projection: &mut Projection) {
    *transform = orbit_transform(orbit);
    sync_projection_from_orbit(orbit, projection);
}

fn sync_projection_from_orbit(orbit: &OrbitCamera, projection: &mut Projection) {
    let required_far = (orbit.radius * 8.0).max(1_000.0);
    match orbit.projection_mode {
        CameraProjectionMode::Perspective => {
            let aspect_ratio = match projection {
                Projection::Perspective(current) => current.aspect_ratio,
                _ => 1.0,
            };
            *projection = Projection::Perspective(PerspectiveProjection {
                fov: focal_length_to_vertical_fov(orbit.focal_length_mm),
                aspect_ratio,
                near: 0.1,
                far: required_far,
                near_clip_plane: vec4(0.0, 0.0, -1.0, -0.1),
            });
        }
        CameraProjectionMode::Isometric => {
            *projection = Projection::Orthographic(OrthographicProjection {
                near: 0.0,
                far: required_far,
                viewport_origin: Vec2::new(0.5, 0.5),
                scaling_mode: ScalingMode::FixedVertical {
                    viewport_height: ORTHOGRAPHIC_VIEWPORT_HEIGHT,
                },
                scale: orbit.radius,
                area: Rect::new(-1.0, -1.0, 1.0, 1.0),
            });
        }
    }
}

fn focal_length_to_vertical_fov(focal_length_mm: f32) -> f32 {
    let focal_length_mm = focal_length_mm.clamp(MIN_FOCAL_LENGTH_MM, MAX_FOCAL_LENGTH_MM);
    2.0 * (FULL_FRAME_SENSOR_HEIGHT_MM / (2.0 * focal_length_mm)).atan()
}

impl OrbitCamera {
    fn apply_view_preset(&mut self, preset: CameraViewPreset) {
        match preset {
            CameraViewPreset::Isometric => {
                self.projection_mode = CameraProjectionMode::Isometric;
                self.yaw = std::f32::consts::FRAC_PI_4;
                self.pitch = TRUE_ISOMETRIC_PITCH;
            }
            CameraViewPreset::Top => {
                self.yaw = 0.0;
                self.pitch = -std::f32::consts::FRAC_PI_2;
            }
            CameraViewPreset::Left => {
                self.yaw = -std::f32::consts::FRAC_PI_2;
                self.pitch = 0.0;
            }
            CameraViewPreset::Right => {
                self.yaw = std::f32::consts::FRAC_PI_2;
                self.pitch = 0.0;
            }
            CameraViewPreset::Bottom => {
                self.yaw = 0.0;
                self.pitch = std::f32::consts::FRAC_PI_2;
            }
        }
    }
}

pub fn focal_length_range_mm() -> std::ops::RangeInclusive<f32> {
    MIN_FOCAL_LENGTH_MM..=MAX_FOCAL_LENGTH_MM
}

fn update_camera_viewport(
    window_query: Query<&Window>,
    viewport_ui_inset: Res<ViewportUiInset>,
    mut cameras: Query<&mut Camera, With<OrbitCamera>>,
) {
    // In bevy_egui ≥0.39, setting camera.viewport causes the egui screen_rect
    // to shrink to the viewport, which creates a feedback loop and hides the
    // egui UI panels.  Instead we leave the camera viewport unset (full window)
    // and let the opaque egui panels cover the edges.  The ViewportUiInset is
    // still maintained so that cursor/picking calculations know the 3D area.
    let _ = (&window_query, &viewport_ui_inset, &mut cameras);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isometric_projection_uses_radius_for_orthographic_scale() {
        let orbit = OrbitCamera {
            projection_mode: CameraProjectionMode::Isometric,
            radius: 18.0,
            ..Default::default()
        };
        let mut projection = Projection::Perspective(PerspectiveProjection::default());
        sync_projection_from_orbit(&orbit, &mut projection);

        match projection {
            Projection::Orthographic(orthographic) => {
                assert_eq!(orthographic.scale, 18.0);
            }
            _ => panic!("expected orthographic projection"),
        }
    }

    #[test]
    fn focal_length_changes_perspective_fov() {
        let mut projection = Projection::Perspective(PerspectiveProjection::default());
        let orbit = OrbitCamera {
            focal_length_mm: 85.0,
            ..Default::default()
        };
        sync_projection_from_orbit(&orbit, &mut projection);

        match projection {
            Projection::Perspective(perspective) => {
                assert!(perspective.fov < PerspectiveProjection::default().fov);
            }
            _ => panic!("expected perspective projection"),
        }
    }

    #[test]
    fn top_and_bottom_views_align_to_world_vertical_axis() {
        let mut orbit = OrbitCamera::default();

        orbit.apply_view_preset(CameraViewPreset::Top);
        let top = orbit_transform(&orbit);
        assert!((top.translation.y - orbit.radius).abs() < 0.001);

        orbit.apply_view_preset(CameraViewPreset::Bottom);
        let bottom = orbit_transform(&orbit);
        assert!((bottom.translation.y + orbit.radius).abs() < 0.001);
    }
}
