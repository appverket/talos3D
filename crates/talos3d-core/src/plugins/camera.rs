use bevy::{
    camera::ScalingMode,
    ecs::system::SystemParam,
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
    input::touch::TouchPhase,
    prelude::*,
};
use bevy_egui::PrimaryEguiContext;
use serde_json::Value;

use crate::authored_entity::EntityBounds;
use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    cursor::ViewportUiInset,
    egui_chrome::EguiWantsInput,
    input_ownership::InputPhase,
    toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
};

pub struct CameraPlugin;

pub const CAMERA_TOOLBAR_ID: &str = "camera.controls";
pub const VIEW_TOOLBAR_ID: &str = "view.presets";
const DEFAULT_FOCAL_LENGTH_MM: f32 = 50.0;
const MIN_FOCAL_LENGTH_MM: f32 = 12.0;
const MAX_FOCAL_LENGTH_MM: f32 = 200.0;
const FULL_FRAME_SENSOR_HEIGHT_MM: f32 = 24.0;
const ORTHOGRAPHIC_VIEWPORT_HEIGHT: f32 = 2.0;
const TRUE_ISOMETRIC_PITCH: f32 = -0.615_479_7; // -atan(1 / sqrt(2))
const MIN_PERSPECTIVE_RADIUS: f32 = 0.5;
const MIN_ORTHOGRAPHIC_SCALE: f32 = 0.05;
const MAX_CAMERA_ZOOM: f32 = 500.0;
const FRAME_RADIUS_PADDING: f32 = 1.25;
const FRAME_ORTHOGRAPHIC_PADDING: f32 = 1.15;

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
            .register_toolbar(ToolbarDescriptor {
                id: VIEW_TOOLBAR_ID.to_string(),
                label: "Views".to_string(),
                default_dock: ToolbarDock::Top,
                default_visible: true,
                sections: vec![
                    ToolbarSection {
                        label: "Projection".to_string(),
                        command_ids: vec![
                            "view.projection_perspective".to_string(),
                            "view.projection_orthographic".to_string(),
                        ],
                    },
                    ToolbarSection {
                        label: "Views".to_string(),
                        command_ids: vec![
                            "view.isometric".to_string(),
                            "view.front".to_string(),
                            "view.back".to_string(),
                            "view.top".to_string(),
                            "view.bottom".to_string(),
                            "view.left".to_string(),
                            "view.right".to_string(),
                        ],
                    },
                ],
            })
            .register_command(
                CommandDescriptor {
                    id: "view.projection_perspective".to_string(),
                    label: "Perspective".to_string(),
                    description: "Switch the camera to perspective projection.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_perspective".to_string()),
                    hint: Some("Return to perspective projection".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_projection_perspective,
            )
            .register_command(
                CommandDescriptor {
                    id: "view.projection_orthographic".to_string(),
                    label: "Orthographic".to_string(),
                    description: "Switch the camera to orthographic projection.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.view_orthographic".to_string()),
                    hint: Some(
                        "Use orthographic projection for drawing and drafting views".to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_projection_orthographic,
            )
            .register_command(
                view_preset_command(
                    "view.isometric",
                    "Isometric",
                    "icon.view_isometric",
                    "Restore the isometric orthographic view",
                ),
                execute_view_isometric,
            )
            .register_command(
                view_preset_command(
                    "view.front",
                    "Front",
                    "icon.view_front",
                    "Look straight at the model front in orthographic projection",
                ),
                execute_view_front,
            )
            .register_command(
                view_preset_command(
                    "view.back",
                    "Back",
                    "icon.view_back",
                    "Look straight at the model back in orthographic projection",
                ),
                execute_view_back,
            )
            .register_command(
                view_preset_command(
                    "view.top",
                    "Top",
                    "icon.view_top",
                    "Look straight down in orthographic projection",
                ),
                execute_view_top,
            )
            .register_command(
                view_preset_command(
                    "view.bottom",
                    "Bottom",
                    "icon.view_bottom",
                    "Look straight up from below in orthographic projection",
                ),
                execute_view_bottom,
            )
            .register_command(
                view_preset_command(
                    "view.left",
                    "Left",
                    "icon.view_left",
                    "Look straight at the model left side in orthographic projection",
                ),
                execute_view_left,
            )
            .register_command(
                view_preset_command(
                    "view.right",
                    "Right",
                    "icon.view_right",
                    "Look straight at the model right side in orthographic projection",
                ),
                execute_view_right,
            )
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
    Front,
    Back,
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
    pub orthographic_scale: f32,
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
            orthographic_scale: default_orthographic_scale(),
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
    commands.spawn((
        PrimaryEguiContext,
        Camera3d::default(),
        projection,
        transform,
        orbit,
    ));
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
            let navigation_scale = orbit.navigation_scale();
            orbit.focus -= right * ev.delta.x * navigation_scale * 0.001;
            orbit.focus += up * ev.delta.y * navigation_scale * 0.001;
        }
    }

    // --- Scroll to zoom ---
    for ev in input.scroll.read() {
        let delta = match ev.unit {
            MouseScrollUnit::Line => ev.y * 0.5,
            MouseScrollUnit::Pixel => ev.y * 0.002,
        };
        match orbit.projection_mode {
            CameraProjectionMode::Perspective => {
                orbit.radius = zoom_with_scroll(orbit.radius, delta, MIN_PERSPECTIVE_RADIUS);
            }
            CameraProjectionMode::Isometric => {
                orbit.orthographic_scale =
                    zoom_with_scroll(orbit.orthographic_scale, delta, MIN_ORTHOGRAPHIC_SCALE);
            }
        }
    }

    apply_orbit_state(&orbit, &mut transform, &mut projection);
}

pub fn orbit_modifier_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

fn orbit_transform(orbit: &OrbitCamera) -> Transform {
    let rotation = orbit.rotation();
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
    bounds: EntityBounds,
    aspect_ratio: f32,
) {
    orbit.focus = bounds.center();
    orbit.radius = orbit_frame_radius(bounds);
    orbit.orthographic_scale = match orbit.projection_mode {
        CameraProjectionMode::Perspective => {
            perspective_distance_to_orthographic_scale(orbit.radius, orbit.focal_length_mm)
        }
        CameraProjectionMode::Isometric => {
            orthographic_scale_for_bounds(bounds, orbit, aspect_ratio)
        }
    };
    apply_orbit_state(orbit, transform, projection);
}

pub fn focus_orbit_camera_on_bounds(world: &mut World, bounds: EntityBounds) -> bool {
    let mut query = world.query::<(&mut OrbitCamera, &mut Transform, &mut Projection, &Camera)>();
    let Some((mut orbit, mut transform, mut projection, camera)) = query.iter_mut(world).next()
    else {
        return false;
    };
    let aspect_ratio = camera_aspect_ratio(camera, &projection);
    frame_orbit_camera(
        &mut orbit,
        &mut transform,
        &mut projection,
        bounds,
        aspect_ratio,
    );
    true
}

pub fn orbit_frame_radius(bounds: EntityBounds) -> f32 {
    let extents = bounds.max - bounds.min;
    extents.length().max(10.0) * FRAME_RADIUS_PADDING
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
        orbit.transition_projection_mode(controls.projection_mode);
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

pub(crate) fn apply_orbit_state(
    orbit: &OrbitCamera,
    transform: &mut Transform,
    projection: &mut Projection,
) {
    *transform = orbit_transform(orbit);
    sync_projection_from_orbit(orbit, projection);
}

fn sync_projection_from_orbit(orbit: &OrbitCamera, projection: &mut Projection) {
    let required_far = (orbit
        .radius
        .max(orthographic_visible_height(orbit.orthographic_scale))
        * 8.0)
        .max(1_000.0);
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
                scale: orbit.orthographic_scale.max(MIN_ORTHOGRAPHIC_SCALE),
                area: Rect::new(-1.0, -1.0, 1.0, 1.0),
            });
        }
    }
}

fn focal_length_to_vertical_fov(focal_length_mm: f32) -> f32 {
    let focal_length_mm = focal_length_mm.clamp(MIN_FOCAL_LENGTH_MM, MAX_FOCAL_LENGTH_MM);
    2.0 * (FULL_FRAME_SENSOR_HEIGHT_MM / (2.0 * focal_length_mm)).atan()
}

pub(crate) fn default_orthographic_scale() -> f32 {
    perspective_distance_to_orthographic_scale(15.0, DEFAULT_FOCAL_LENGTH_MM)
}

fn perspective_visible_height(distance: f32, focal_length_mm: f32) -> f32 {
    2.0 * distance.max(MIN_PERSPECTIVE_RADIUS)
        * (focal_length_to_vertical_fov(focal_length_mm) * 0.5).tan()
}

fn orthographic_visible_height(scale: f32) -> f32 {
    scale.max(MIN_ORTHOGRAPHIC_SCALE) * ORTHOGRAPHIC_VIEWPORT_HEIGHT
}

pub(crate) fn perspective_distance_to_orthographic_scale(
    distance: f32,
    focal_length_mm: f32,
) -> f32 {
    (perspective_visible_height(distance, focal_length_mm) / ORTHOGRAPHIC_VIEWPORT_HEIGHT)
        .max(MIN_ORTHOGRAPHIC_SCALE)
}

fn orthographic_scale_to_perspective_distance(scale: f32, focal_length_mm: f32) -> f32 {
    let half_height = orthographic_visible_height(scale) * 0.5;
    let half_fov_tangent = (focal_length_to_vertical_fov(focal_length_mm) * 0.5).tan();
    (half_height / half_fov_tangent.max(0.001)).max(MIN_PERSPECTIVE_RADIUS)
}

fn zoom_with_scroll(current: f32, delta: f32, min_value: f32) -> f32 {
    (current - delta * current * 0.1).clamp(min_value, MAX_CAMERA_ZOOM)
}

fn camera_aspect_ratio(camera: &Camera, projection: &Projection) -> f32 {
    camera
        .logical_viewport_size()
        .map(|size| (size.x / size.y.max(1.0)).max(0.1))
        .or_else(|| match projection {
            Projection::Perspective(current) => Some(current.aspect_ratio.max(0.1)),
            Projection::Orthographic(current) => {
                let width = (current.area.max.x - current.area.min.x).abs();
                let height = (current.area.max.y - current.area.min.y).abs();
                Some((width / height.max(0.001)).max(0.1))
            }
            _ => None,
        })
        .unwrap_or(1.0)
}

fn orthographic_scale_for_bounds(
    bounds: EntityBounds,
    orbit: &OrbitCamera,
    aspect_ratio: f32,
) -> f32 {
    let view_rotation = orbit.rotation().inverse();
    let center = bounds.center();
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);

    for corner in bounds.corners() {
        let camera_local = view_rotation * (corner - center);
        min = min.min(camera_local.truncate());
        max = max.max(camera_local.truncate());
    }

    let projected_size = (max - min).abs();
    let visible_height = projected_size
        .y
        .max(projected_size.x / aspect_ratio.max(0.1))
        .max(MIN_ORTHOGRAPHIC_SCALE * ORTHOGRAPHIC_VIEWPORT_HEIGHT);

    (visible_height * FRAME_ORTHOGRAPHIC_PADDING / ORTHOGRAPHIC_VIEWPORT_HEIGHT)
        .max(MIN_ORTHOGRAPHIC_SCALE)
}

impl OrbitCamera {
    fn rotation(&self) -> Quat {
        Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }

    fn navigation_scale(&self) -> f32 {
        match self.projection_mode {
            CameraProjectionMode::Perspective => self.radius.max(MIN_PERSPECTIVE_RADIUS),
            CameraProjectionMode::Isometric => self.orthographic_scale.max(MIN_ORTHOGRAPHIC_SCALE),
        }
    }

    fn transition_projection_mode(&mut self, next_mode: CameraProjectionMode) {
        if self.projection_mode == next_mode {
            return;
        }

        match (self.projection_mode, next_mode) {
            (CameraProjectionMode::Perspective, CameraProjectionMode::Isometric) => {
                self.orthographic_scale =
                    perspective_distance_to_orthographic_scale(self.radius, self.focal_length_mm);
            }
            (CameraProjectionMode::Isometric, CameraProjectionMode::Perspective) => {
                self.radius = orthographic_scale_to_perspective_distance(
                    self.orthographic_scale,
                    self.focal_length_mm,
                );
            }
            _ => {}
        }

        self.projection_mode = next_mode;
    }

    fn apply_view_preset(&mut self, preset: CameraViewPreset) {
        self.transition_projection_mode(CameraProjectionMode::Isometric);
        match preset {
            CameraViewPreset::Isometric => {
                self.yaw = std::f32::consts::FRAC_PI_4;
                self.pitch = TRUE_ISOMETRIC_PITCH;
            }
            CameraViewPreset::Front => {
                self.yaw = 0.0;
                self.pitch = 0.0;
            }
            CameraViewPreset::Back => {
                self.yaw = std::f32::consts::PI;
                self.pitch = 0.0;
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

fn view_preset_command(id: &str, label: &str, icon: &str, hint: &str) -> CommandDescriptor {
    CommandDescriptor {
        id: id.to_string(),
        label: label.to_string(),
        description: format!("Switch the camera to the {label} view."),
        category: CommandCategory::View,
        parameters: None,
        default_shortcut: None,
        icon: Some(icon.to_string()),
        hint: Some(hint.to_string()),
        requires_selection: false,
        show_in_menu: true,
        version: 1,
        activates_tool: None,
        capability_id: None,
    }
}

fn execute_projection_perspective(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_projection_mode(world, CameraProjectionMode::Perspective, "Perspective view")
}

fn execute_projection_orthographic(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_projection_mode(world, CameraProjectionMode::Isometric, "Orthographic view")
}

fn execute_view_isometric(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Isometric, "Isometric view")
}

fn execute_view_front(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Front, "Front view")
}

fn execute_view_back(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Back, "Back view")
}

fn execute_view_top(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Top, "Top view")
}

fn execute_view_bottom(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Bottom, "Bottom view")
}

fn execute_view_left(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Left, "Left view")
}

fn execute_view_right(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    set_view_preset(world, CameraViewPreset::Right, "Right view")
}

fn set_projection_mode(
    world: &mut World,
    mode: CameraProjectionMode,
    feedback: &str,
) -> Result<CommandResult, String> {
    {
        let mut controls = world
            .get_resource_mut::<CameraControlsState>()
            .ok_or_else(|| "Camera controls are unavailable".to_string())?;
        controls.projection_mode = mode;
    }
    set_camera_feedback(world, feedback);
    Ok(CommandResult::empty())
}

fn set_view_preset(
    world: &mut World,
    preset: CameraViewPreset,
    feedback: &str,
) -> Result<CommandResult, String> {
    {
        let mut controls = world
            .get_resource_mut::<CameraControlsState>()
            .ok_or_else(|| "Camera controls are unavailable".to_string())?;
        controls.pending_view_preset = Some(preset);
    }
    set_camera_feedback(world, feedback);
    Ok(CommandResult::empty())
}

fn set_camera_feedback(world: &mut World, feedback: &str) {
    if let Some(mut status_bar_data) = world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
    {
        status_bar_data.set_feedback(feedback.to_string(), 2.0);
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
    fn isometric_projection_uses_explicit_orthographic_scale() {
        let orbit = OrbitCamera {
            projection_mode: CameraProjectionMode::Isometric,
            radius: 18.0,
            orthographic_scale: 4.5,
            ..Default::default()
        };
        let mut projection = Projection::Perspective(PerspectiveProjection::default());
        sync_projection_from_orbit(&orbit, &mut projection);

        match projection {
            Projection::Orthographic(orthographic) => {
                assert_eq!(orthographic.scale, 4.5);
            }
            _ => panic!("expected orthographic projection"),
        }
    }

    #[test]
    fn switching_to_orthographic_preserves_visible_height() {
        let mut orbit = OrbitCamera::default();
        let before = perspective_visible_height(orbit.radius, orbit.focal_length_mm);

        orbit.transition_projection_mode(CameraProjectionMode::Isometric);

        let after = orthographic_visible_height(orbit.orthographic_scale);
        assert!((after - before).abs() < 0.001);
    }

    #[test]
    fn switching_back_to_perspective_preserves_visible_height() {
        let mut orbit = OrbitCamera {
            projection_mode: CameraProjectionMode::Isometric,
            orthographic_scale: 3.25,
            ..Default::default()
        };
        let before = orthographic_visible_height(orbit.orthographic_scale);

        orbit.transition_projection_mode(CameraProjectionMode::Perspective);

        let after = perspective_visible_height(orbit.radius, orbit.focal_length_mm);
        assert!((after - before).abs() < 0.001);
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
        let perspective_visible = perspective_visible_height(orbit.radius, orbit.focal_length_mm);

        orbit.apply_view_preset(CameraViewPreset::Front);
        let front = orbit_transform(&orbit);
        assert!((front.translation.z - orbit.radius).abs() < 0.001);
        assert_eq!(orbit.projection_mode, CameraProjectionMode::Isometric);
        assert!(
            (orthographic_visible_height(orbit.orthographic_scale) - perspective_visible).abs()
                < 0.001
        );

        orbit.apply_view_preset(CameraViewPreset::Back);
        let back = orbit_transform(&orbit);
        assert!((back.translation.z + orbit.radius).abs() < 0.001);

        orbit.apply_view_preset(CameraViewPreset::Top);
        let top = orbit_transform(&orbit);
        assert!((top.translation.y - orbit.radius).abs() < 0.001);

        orbit.apply_view_preset(CameraViewPreset::Bottom);
        let bottom = orbit_transform(&orbit);
        assert!((bottom.translation.y + orbit.radius).abs() < 0.001);
    }
}
