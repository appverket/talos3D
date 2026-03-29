use bevy::{
    ecs::system::SystemParam,
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
    input::touch::TouchPhase,
    prelude::*,
};

use crate::authored_entity::EntityBounds;
use crate::plugins::{
    cursor::ViewportUiInset, egui_chrome::EguiWantsInput, input_ownership::InputPhase,
};

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TrackpadState>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
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

/// Orbit camera state stored as a component.
#[derive(Component)]
pub struct OrbitCamera {
    pub focus: Vec3,
    pub radius: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            radius: 15.0,
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: -std::f32::consts::FRAC_PI_6, // negative = above the grid
        }
    }
}

fn spawn_camera(mut commands: Commands) {
    let orbit = OrbitCamera::default();
    let transform = orbit_transform(&orbit);
    commands.spawn((Camera3d::default(), transform, orbit));
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
    query: Query<'w, 's, (&'static mut OrbitCamera, &'static mut Transform)>,
}

fn orbit_camera(mut input: OrbitCameraInput) {
    if input.egui_wants_input.pointer {
        input.motion.clear();
        input.scroll.clear();
        input.touch_events.clear();
        input.trackpad.prev_centroid = None;
        return;
    }

    let Ok((mut orbit, mut transform)) = input.query.single_mut() else {
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

    // --- Right-mouse-drag orbit / Shift+right-drag pan ---
    let shift = input.keys.pressed(KeyCode::ShiftLeft) || input.keys.pressed(KeyCode::ShiftRight);
    let right_pressed = input.mouse_buttons.pressed(MouseButton::Right);
    let orbiting = right_pressed && !shift;
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

    *transform = orbit_transform(&orbit);
}

fn orbit_transform(orbit: &OrbitCamera) -> Transform {
    let rotation = Quat::from_euler(EulerRot::YXZ, orbit.yaw, orbit.pitch, 0.0);
    let offset = rotation * Vec3::new(0.0, 0.0, orbit.radius);
    Transform::from_translation(orbit.focus + offset).looking_at(orbit.focus, Vec3::Y)
}

pub(crate) fn frame_orbit_camera(
    orbit: &mut OrbitCamera,
    transform: &mut Transform,
    focus: Vec3,
    radius: f32,
) {
    orbit.focus = focus;
    orbit.radius = radius.max(0.5);
    *transform = orbit_transform(orbit);
}

pub fn focus_orbit_camera_on_bounds(world: &mut World, bounds: EntityBounds) -> bool {
    let mut query = world.query::<(&mut OrbitCamera, &mut Transform, Entity)>();
    let Some((mut orbit, mut transform, camera_entity)) = query.iter_mut(world).next() else {
        return false;
    };
    let radius = orbit_frame_radius(bounds);
    frame_orbit_camera(&mut orbit, &mut transform, bounds.center(), radius);
    let required_far = radius * 4.0;
    if required_far > 1000.0 {
        world
            .entity_mut(camera_entity)
            .insert(Projection::Perspective(PerspectiveProjection {
                far: required_far,
                ..Default::default()
            }));
    }
    true
}

pub fn orbit_frame_radius(bounds: EntityBounds) -> f32 {
    let extents = bounds.max - bounds.min;
    extents.length().max(10.0) * 1.25
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
