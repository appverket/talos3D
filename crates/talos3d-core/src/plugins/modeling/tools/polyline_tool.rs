use bevy::{ecs::system::SystemParam, prelude::*};

use crate::plugins::{
    commands::{CreateEntityCommand, CreatePolylineCommand},
    cursor::{CursorWorldPos, DrawingPlane},
    egui_chrome::EguiWantsInput,
    face_edit::FaceEditContext,
    identity::ElementIdAllocator,
    math::project_direction_to_plane,
    modeling::{
        generic_snapshot::PrimitiveSnapshot,
        primitives::ShapeRotation,
        profile::{Profile2d, ProfileExtrusion, ProfileSegment},
        profile_feature::make_face_profile_feature_snapshot,
    },
    snap::SnapResult,
    tools::ActiveTool,
    ui::StatusBarData,
};

const PREVIEW_COLOR: Color = Color::srgb(0.45, 0.9, 1.0);
const CLOSE_COLOR: Color = Color::srgb(0.2, 1.0, 0.4);
const MIN_SEGMENT_LENGTH_METRES: f32 = 0.1;
/// Screen-space close threshold in pixels.
const CLOSE_THRESHOLD_PIXELS: f32 = 15.0;

#[derive(SystemParam)]
struct PolylineClickContext<'w, 's> {
    mouse_buttons: Res<'w, ButtonInput<MouseButton>>,
    egui_wants_input: Res<'w, EguiWantsInput>,
    snap_result: Res<'w, SnapResult>,
    cursor_world_pos: Res<'w, CursorWorldPos>,
    drawing_plane: Res<'w, DrawingPlane>,
    camera_query: Query<'w, 's, (&'static Camera, &'static GlobalTransform)>,
    face_context: ResMut<'w, FaceEditContext>,
    create_polyline: MessageWriter<'w, CreatePolylineCommand>,
    create_entity: MessageWriter<'w, CreateEntityCommand>,
    next_active_tool: ResMut<'w, NextState<ActiveTool>>,
    element_id_allocator: Res<'w, ElementIdAllocator>,
}

#[derive(SystemParam)]
struct PolylineFinishContext<'w> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    egui_wants_input: Res<'w, EguiWantsInput>,
    drawing_plane: Res<'w, DrawingPlane>,
    face_context: ResMut<'w, FaceEditContext>,
    create_polyline: MessageWriter<'w, CreatePolylineCommand>,
    create_entity: MessageWriter<'w, CreateEntityCommand>,
    next_active_tool: ResMut<'w, NextState<ActiveTool>>,
    element_id_allocator: Res<'w, ElementIdAllocator>,
}

pub struct PolylineToolPlugin;

impl Plugin for PolylineToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveTool::PlacePolyline), initialize_polyline_tool)
            .add_systems(OnExit(ActiveTool::PlacePolyline), cleanup_polyline_tool)
            .add_systems(
                Update,
                (
                    cancel_polyline_tool,
                    handle_polyline_axis_lock,
                    handle_polyline_clicks,
                    finish_polyline_on_enter,
                    draw_polyline_preview,
                    update_polyline_tool_hint,
                )
                    .run_if(in_state(ActiveTool::PlacePolyline)),
            );
    }
}

#[derive(Resource, Default)]
struct PolylineToolState {
    points: Vec<Vec3>,
    closed: bool,
    axis_lock: PolylineAxisLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PolylineAxisLock {
    #[default]
    None,
    X,
    Y,
    Z,
}

impl PolylineAxisLock {
    fn toggled(self, axis: Self) -> Self {
        if self == axis {
            Self::None
        } else {
            axis
        }
    }

    fn label(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::X => Some("X"),
            Self::Y => Some("Y"),
            Self::Z => Some("Z"),
        }
    }
}

fn initialize_polyline_tool(mut commands: Commands) {
    commands.insert_resource(PolylineToolState::default());
}

fn cleanup_polyline_tool(mut commands: Commands) {
    commands.remove_resource::<PolylineToolState>();
}

fn cancel_polyline_tool(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut state: ResMut<PolylineToolState>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    if !state.points.is_empty() {
        state.points.clear();
        state.closed = false;
        state.axis_lock = PolylineAxisLock::None;
        status_bar_data.hint.clear();
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn handle_polyline_axis_lock(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut state: ResMut<PolylineToolState>,
) {
    if egui_wants_input.keyboard {
        return;
    }

    if keys.just_pressed(KeyCode::KeyX) {
        state.axis_lock = state.axis_lock.toggled(PolylineAxisLock::X);
    } else if keys.just_pressed(KeyCode::KeyY) {
        state.axis_lock = state.axis_lock.toggled(PolylineAxisLock::Y);
    } else if keys.just_pressed(KeyCode::KeyZ) {
        state.axis_lock = state.axis_lock.toggled(PolylineAxisLock::Z);
    }
}

fn polyline_tool_hint(state: &PolylineToolState) -> String {
    let mut parts = if state.points.len() >= 3 {
        vec![
            "Click to add points".to_string(),
            "close near start".to_string(),
            "Enter to finish".to_string(),
        ]
    } else {
        vec![
            "Click to add points".to_string(),
            "Enter to finish".to_string(),
        ]
    };
    parts.push(match state.axis_lock.label() {
        Some(label) => format!("X/Y/Z lock ({label})"),
        None => "X/Y/Z lock".to_string(),
    });
    parts.join(" \u{00b7} ")
}

fn update_polyline_tool_hint(
    state: Res<PolylineToolState>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    status_bar_data.hint = polyline_tool_hint(&state);
}

/// Compute the close threshold in world-space, scaled by camera distance.
fn close_threshold_world(camera_query: &Query<(&Camera, &GlobalTransform)>, start: Vec3) -> f32 {
    let Some((_camera, cam_tf)) = camera_query.iter().next() else {
        return 0.3; // fallback
    };
    let camera_distance = cam_tf.translation().distance(start);
    // Approximate: at a reasonable FOV (~60°), 15 pixels at 1000px viewport height
    // corresponds to ~0.026 * distance. This scales naturally with zoom.
    let scale = camera_distance * CLOSE_THRESHOLD_PIXELS / 1000.0;
    scale.clamp(0.05, 2.0)
}

fn polyline_axis_direction(
    axis_lock: PolylineAxisLock,
    drawing_plane: &DrawingPlane,
) -> Option<Vec3> {
    let axis = match axis_lock {
        PolylineAxisLock::None => return None,
        PolylineAxisLock::X => Vec3::X,
        PolylineAxisLock::Y => Vec3::Y,
        PolylineAxisLock::Z => Vec3::Z,
    };
    project_direction_to_plane(axis, drawing_plane.normal)
}

fn apply_polyline_axis_lock(
    anchor: Vec3,
    candidate: Vec3,
    drawing_plane: &DrawingPlane,
    axis_lock: PolylineAxisLock,
) -> Vec3 {
    let Some(direction) = polyline_axis_direction(axis_lock, drawing_plane) else {
        return candidate;
    };
    anchor + direction * (candidate - anchor).dot(direction)
}

fn polyline_cursor_position(
    state: &PolylineToolState,
    snap_result: &SnapResult,
    cursor_world_pos: &CursorWorldPos,
    drawing_plane: &DrawingPlane,
) -> Option<Vec3> {
    let cursor = snap_result
        .position
        .or(cursor_world_pos.snapped)
        .or(cursor_world_pos.raw)?;
    state
        .points
        .last()
        .copied()
        .map(|anchor| apply_polyline_axis_lock(anchor, cursor, drawing_plane, state.axis_lock))
        .or(Some(cursor))
}

fn handle_polyline_clicks(mut cx: PolylineClickContext, mut state: ResMut<PolylineToolState>) {
    if cx.egui_wants_input.pointer || !cx.mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = polyline_cursor_position(
        &state,
        &cx.snap_result,
        &cx.cursor_world_pos,
        &cx.drawing_plane,
    ) else {
        return;
    };

    // Check if closing the loop
    if state.points.len() >= 3 {
        let start = state.points[0];
        let threshold = close_threshold_world(&cx.camera_query, start);
        if cursor_position.distance(start) < threshold {
            state.closed = true;
            let parent_element_id = cx.face_context.element_id;
            let new_element_id = finish_shape(
                &state,
                &cx.drawing_plane,
                parent_element_id,
                &mut cx.create_polyline,
                &mut cx.create_entity,
                &cx.element_id_allocator,
            );
            // Register CSG parent and exit face-edit
            if let Some(_child_id) = new_element_id {
                cx.face_context.exit();
            }
            state.points.clear();
            state.closed = false;
            state.axis_lock = PolylineAxisLock::None;
            cx.next_active_tool.set(ActiveTool::Select);
            return;
        }
    }

    let should_add = state
        .points
        .last()
        .map(|last| last.distance(cursor_position) >= MIN_SEGMENT_LENGTH_METRES)
        .unwrap_or(true);

    if should_add {
        state.points.push(cursor_position);
    }
}

fn finish_polyline_on_enter(mut cx: PolylineFinishContext, mut state: ResMut<PolylineToolState>) {
    if cx.egui_wants_input.keyboard
        || !(cx.keys.just_pressed(KeyCode::Enter) || cx.keys.just_pressed(KeyCode::NumpadEnter))
        || state.points.len() < 2
    {
        return;
    }

    if state.points.len() >= 3 {
        state.closed = true;
    }

    let parent_element_id = cx.face_context.element_id;

    let new_element_id = finish_shape(
        &state,
        &cx.drawing_plane,
        parent_element_id,
        &mut cx.create_polyline,
        &mut cx.create_entity,
        &cx.element_id_allocator,
    );
    if let Some(_child_id) = new_element_id {
        cx.face_context.exit();
    }
    state.points.clear();
    state.closed = false;
    state.axis_lock = PolylineAxisLock::None;
    cx.next_active_tool.set(ActiveTool::Select);
}

/// Create the appropriate entity. Returns the ElementId if a ProfileExtrusion
/// was created (so the caller can select it and exit face-edit).
fn finish_shape(
    state: &PolylineToolState,
    drawing_plane: &DrawingPlane,
    parent_element_id: Option<crate::plugins::identity::ElementId>,
    create_polyline: &mut MessageWriter<CreatePolylineCommand>,
    create_entity: &mut MessageWriter<CreateEntityCommand>,
    allocator: &ElementIdAllocator,
) -> Option<crate::plugins::identity::ElementId> {
    if state.points.len() < 2 {
        return None;
    }

    if state.closed && state.points.len() >= 3 {
        Some(create_profile_from_closed_polyline(
            &state.points,
            drawing_plane,
            parent_element_id,
            create_entity,
            allocator,
        ))
    } else {
        create_polyline.write(CreatePolylineCommand {
            points: state.points.clone(),
        });
        None
    }
}

/// Create a ProfileExtrusion from a closed polyline drawn on a plane.
fn create_profile_from_closed_polyline(
    points: &[Vec3],
    plane: &DrawingPlane,
    parent_element_id: Option<crate::plugins::identity::ElementId>,
    create_entity: &mut MessageWriter<CreateEntityCommand>,
    allocator: &ElementIdAllocator,
) -> crate::plugins::identity::ElementId {
    let points_2d: Vec<Vec2> = points.iter().map(|p| plane.project_to_2d(*p)).collect();

    let start = points_2d[0];
    let segments: Vec<ProfileSegment> = points_2d[1..]
        .iter()
        .map(|&to| ProfileSegment::LineTo { to })
        .collect();
    let profile = Profile2d { start, segments };

    let (pmin, pmax) = profile.bounds_2d();
    let mid_2d = (pmin + pmax) * 0.5;
    let centred_profile = profile.translated(-mid_2d);

    let centre_on_plane = plane.to_world(mid_2d);

    // Build rotation from the drawing plane's tangent frame.
    // local X → plane.tangent, local Y → plane.normal, local Z → plane.bitangent
    let rotation = Quat::from_mat3(&Mat3::from_cols(
        plane.tangent,
        plane.normal,
        plane.bitangent,
    ));

    let element_id = allocator.next_id();
    let snapshot = if let Some(parent_id) = parent_element_id {
        make_face_profile_feature_snapshot(
            element_id,
            parent_id,
            centred_profile,
            centre_on_plane,
            rotation,
            None,
        )
        .into()
    } else {
        let height = 0.02;
        PrimitiveSnapshot {
            element_id,
            primitive: ProfileExtrusion {
                centre: centre_on_plane + plane.normal * (height * 0.5),
                profile: centred_profile,
                height,
            },
            rotation: ShapeRotation(rotation),
        }
        .into()
    };

    create_entity.write(CreateEntityCommand { snapshot });

    element_id
}

fn draw_polyline_preview(
    snap_result: Res<SnapResult>,
    cursor_world_pos: Res<CursorWorldPos>,
    drawing_plane: Res<DrawingPlane>,
    camera_query: Query<(&Camera, &GlobalTransform)>,
    state: Res<PolylineToolState>,
    mut gizmos: Gizmos,
) {
    // Draw existing segments
    for segment in state.points.windows(2) {
        gizmos.line(segment[0], segment[1], PREVIEW_COLOR);
    }

    let Some(cursor_position) =
        polyline_cursor_position(&state, &snap_result, &cursor_world_pos, &drawing_plane)
    else {
        return;
    };

    // Line from last point to cursor
    if let Some(last_point) = state.points.last() {
        gizmos.line(*last_point, cursor_position, PREVIEW_COLOR.with_alpha(0.5));
    }

    // Close preview and snap indicator
    if state.points.len() >= 3 {
        let start = state.points[0];
        let threshold = close_threshold_world(&camera_query, start);

        gizmos.line(cursor_position, start, PREVIEW_COLOR.with_alpha(0.3));

        if cursor_position.distance(start) < threshold {
            gizmos.sphere(
                Isometry3d::from_translation(start),
                threshold * 0.3,
                CLOSE_COLOR,
            );
        }
    }

    // Point dots
    for p in &state.points {
        gizmos.sphere(Isometry3d::from_translation(*p), 0.04, PREVIEW_COLOR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyline_axis_lock_projects_world_axis_onto_current_plane() {
        let drawing_plane = DrawingPlane::from_face(Vec3::ZERO, Vec3::Z);
        let anchor = Vec3::new(1.0, 2.0, 0.0);
        let candidate = Vec3::new(4.0, 7.0, 3.0);

        assert_eq!(
            apply_polyline_axis_lock(anchor, candidate, &drawing_plane, PolylineAxisLock::X),
            Vec3::new(4.0, 2.0, 0.0)
        );
        assert_eq!(
            apply_polyline_axis_lock(anchor, candidate, &drawing_plane, PolylineAxisLock::Y),
            Vec3::new(1.0, 7.0, 0.0)
        );
    }

    #[test]
    fn polyline_axis_lock_ignores_axis_orthogonal_to_current_plane() {
        let drawing_plane = DrawingPlane::from_face(Vec3::ZERO, Vec3::Y);
        let candidate = Vec3::new(2.0, 5.0, -3.0);

        assert_eq!(
            apply_polyline_axis_lock(Vec3::ZERO, candidate, &drawing_plane, PolylineAxisLock::Y,),
            candidate
        );
    }
}
