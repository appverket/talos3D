use bevy::prelude::*;

use crate::plugins::{
    commands::CreateBoxCommand, cursor::CursorWorldPos, egui_chrome::EguiWantsInput,
    tools::ActiveTool, ui::StatusBarData,
};

const PREVIEW_COLOR: Color = Color::srgb(0.45, 0.9, 1.0);
const MIN_BOX_DIMENSION_METRES: f32 = 0.1;

pub struct BoxToolPlugin;

impl Plugin for BoxToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveTool::PlaceBox), initialize_box_tool)
            .add_systems(OnExit(ActiveTool::PlaceBox), cleanup_box_tool)
            .add_systems(
                Update,
                (cancel_box_tool, handle_box_clicks, draw_box_preview)
                    .run_if(in_state(ActiveTool::PlaceBox)),
            );
    }
}

#[derive(Resource, Default)]
struct BoxToolState {
    phase: BoxToolPhase,
    base_center: Option<Vec3>,
    half_extents_xz: Option<Vec2>,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum BoxToolPhase {
    #[default]
    Center,
    Base,
    Height,
}

fn initialize_box_tool(mut commands: Commands) {
    commands.insert_resource(BoxToolState::default());
}

fn cleanup_box_tool(mut commands: Commands) {
    commands.remove_resource::<BoxToolState>();
}

fn cancel_box_tool(
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

fn handle_box_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut box_tool_state: ResMut<BoxToolState>,
    mut create_box_commands: MessageWriter<CreateBoxCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match box_tool_state.phase {
        BoxToolPhase::Center => {
            box_tool_state.phase = BoxToolPhase::Base;
            box_tool_state.base_center = Some(cursor_position);
            status_bar_data.hint = "Click to confirm width/depth".to_string();
        }
        BoxToolPhase::Base => {
            let Some(base_center) = box_tool_state.base_center else {
                return;
            };

            let half_extents_xz = (cursor_position - base_center).xz().abs();
            if half_extents_xz.x < MIN_BOX_DIMENSION_METRES
                || half_extents_xz.y < MIN_BOX_DIMENSION_METRES
            {
                return;
            }

            box_tool_state.phase = BoxToolPhase::Height;
            box_tool_state.half_extents_xz = Some(half_extents_xz);
            status_bar_data.hint = "Click to confirm height".to_string();
        }
        BoxToolPhase::Height => {
            let Some(base_center) = box_tool_state.base_center else {
                return;
            };
            let Some(half_extents_xz) = box_tool_state.half_extents_xz else {
                return;
            };

            let half_height = cursor_position.xz().distance(base_center.xz()) * 0.5;
            if half_height < MIN_BOX_DIMENSION_METRES {
                return;
            }

            create_box_commands.write(CreateBoxCommand {
                centre: Vec3::new(base_center.x, half_height, base_center.z),
                half_extents: Vec3::new(half_extents_xz.x, half_height, half_extents_xz.y),
            });

            next_active_tool.set(ActiveTool::Select);
            status_bar_data.hint.clear();
        }
    }
}

fn draw_box_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    box_tool_state: Res<BoxToolState>,
    mut gizmos: Gizmos,
) {
    let Some(base_center) = box_tool_state.base_center else {
        return;
    };
    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match box_tool_state.phase {
        BoxToolPhase::Center => {}
        BoxToolPhase::Base => {
            let half_extents = (cursor_position - base_center).xz().abs();
            draw_box_outline(
                &mut gizmos,
                Vec3::new(base_center.x, 0.05, base_center.z),
                Vec3::new(half_extents.x, 0.05, half_extents.y),
            );
        }
        BoxToolPhase::Height => {
            let Some(half_extents_xz) = box_tool_state.half_extents_xz else {
                return;
            };
            let half_height = cursor_position.xz().distance(base_center.xz()) * 0.5;
            draw_box_outline(
                &mut gizmos,
                Vec3::new(base_center.x, half_height, base_center.z),
                Vec3::new(half_extents_xz.x, half_height, half_extents_xz.y),
            );
        }
    }
}

fn draw_box_outline(gizmos: &mut Gizmos, centre: Vec3, half_extents: Vec3) {
    let min = centre - half_extents;
    let max = centre + half_extents;
    let bottom = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, min.y, min.z),
    ];
    let top = [
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(max.x, max.y, min.z),
    ];

    for index in 0..bottom.len() {
        let next_index = (index + 1) % bottom.len();
        gizmos.line(bottom[index], bottom[next_index], PREVIEW_COLOR);
        gizmos.line(top[index], top[next_index], PREVIEW_COLOR);
        gizmos.line(bottom[index], top[index], PREVIEW_COLOR);
    }
}
