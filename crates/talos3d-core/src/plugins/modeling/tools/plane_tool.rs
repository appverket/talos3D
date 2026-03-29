use bevy::prelude::*;

use crate::plugins::{
    commands::CreatePlaneCommand, cursor::CursorWorldPos, egui_chrome::EguiWantsInput,
    tools::ActiveTool, ui::StatusBarData,
};

const PREVIEW_COLOR: Color = Color::srgb(0.45, 0.9, 1.0);
const MIN_PLANE_SIZE_METRES: f32 = 0.1;

pub struct PlaneToolPlugin;

impl Plugin for PlaneToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveTool::PlacePlane), initialize_plane_tool)
            .add_systems(OnExit(ActiveTool::PlacePlane), cleanup_plane_tool)
            .add_systems(
                Update,
                (cancel_plane_tool, handle_plane_clicks, draw_plane_preview)
                    .run_if(in_state(ActiveTool::PlacePlane)),
            );
    }
}

#[derive(Resource, Default)]
struct PlaneToolState {
    first_corner: Option<Vec3>,
}

fn initialize_plane_tool(mut commands: Commands) {
    commands.insert_resource(PlaneToolState::default());
}

fn cleanup_plane_tool(mut commands: Commands) {
    commands.remove_resource::<PlaneToolState>();
}

fn cancel_plane_tool(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn handle_plane_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut plane_tool_state: ResMut<PlaneToolState>,
    mut create_plane_commands: MessageWriter<CreatePlaneCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match plane_tool_state.first_corner {
        None => {
            plane_tool_state.first_corner = Some(cursor_position);
            status_bar_data.hint = "Click to place opposite corner".to_string();
        }
        Some(first_corner) => {
            let size = (cursor_position - first_corner).xz().abs();
            if size.x < MIN_PLANE_SIZE_METRES || size.y < MIN_PLANE_SIZE_METRES {
                return;
            }

            create_plane_commands.write(CreatePlaneCommand {
                corner_a: Vec2::new(first_corner.x, first_corner.z),
                corner_b: Vec2::new(cursor_position.x, cursor_position.z),
                elevation: first_corner.y,
            });

            next_active_tool.set(ActiveTool::Select);
            status_bar_data.hint.clear();
        }
    }
}

fn draw_plane_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    plane_tool_state: Res<PlaneToolState>,
    mut gizmos: Gizmos,
) {
    let Some(first_corner) = plane_tool_state.first_corner else {
        return;
    };
    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    let corners = [
        Vec3::new(first_corner.x, first_corner.y, first_corner.z),
        Vec3::new(first_corner.x, first_corner.y, cursor_position.z),
        Vec3::new(cursor_position.x, first_corner.y, cursor_position.z),
        Vec3::new(cursor_position.x, first_corner.y, first_corner.z),
    ];

    for index in 0..corners.len() {
        let next_index = (index + 1) % corners.len();
        gizmos.line(corners[index], corners[next_index], PREVIEW_COLOR);
    }
}
