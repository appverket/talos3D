use bevy::prelude::*;

use crate::plugins::{
    commands::CreateCylinderCommand, cursor::CursorWorldPos, egui_chrome::EguiWantsInput,
    tools::ActiveTool, ui::StatusBarData,
};

const PREVIEW_COLOR: Color = Color::srgb(0.45, 0.9, 1.0);
const MIN_CYLINDER_DIMENSION_METRES: f32 = 0.1;
const CIRCLE_SEGMENTS: usize = 24;

pub struct CylinderToolPlugin;

impl Plugin for CylinderToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveTool::PlaceCylinder), initialize_cylinder_tool)
            .add_systems(OnExit(ActiveTool::PlaceCylinder), cleanup_cylinder_tool)
            .add_systems(
                Update,
                (
                    cancel_cylinder_tool,
                    handle_cylinder_clicks,
                    draw_cylinder_preview,
                )
                    .run_if(in_state(ActiveTool::PlaceCylinder)),
            );
    }
}

#[derive(Resource, Default)]
struct CylinderToolState {
    phase: CylinderToolPhase,
    base_center: Option<Vec3>,
    radius: Option<f32>,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum CylinderToolPhase {
    #[default]
    Center,
    Radius,
    Height,
}

fn initialize_cylinder_tool(mut commands: Commands) {
    commands.insert_resource(CylinderToolState::default());
}

fn cleanup_cylinder_tool(mut commands: Commands) {
    commands.remove_resource::<CylinderToolState>();
}

fn cancel_cylinder_tool(
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

fn handle_cylinder_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut cylinder_tool_state: ResMut<CylinderToolState>,
    mut create_cylinder_commands: MessageWriter<CreateCylinderCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match cylinder_tool_state.phase {
        CylinderToolPhase::Center => {
            cylinder_tool_state.phase = CylinderToolPhase::Radius;
            cylinder_tool_state.base_center = Some(cursor_position);
            status_bar_data.hint = "Click to confirm radius".to_string();
        }
        CylinderToolPhase::Radius => {
            let Some(base_center) = cylinder_tool_state.base_center else {
                return;
            };

            let radius = cursor_position.xz().distance(base_center.xz());
            if radius < MIN_CYLINDER_DIMENSION_METRES {
                return;
            }

            cylinder_tool_state.phase = CylinderToolPhase::Height;
            cylinder_tool_state.radius = Some(radius);
            status_bar_data.hint = "Click to confirm height".to_string();
        }
        CylinderToolPhase::Height => {
            let Some(base_center) = cylinder_tool_state.base_center else {
                return;
            };
            let Some(radius) = cylinder_tool_state.radius else {
                return;
            };

            let height = cursor_position.xz().distance(base_center.xz());
            if height < MIN_CYLINDER_DIMENSION_METRES {
                return;
            }

            create_cylinder_commands.write(CreateCylinderCommand {
                centre: Vec3::new(base_center.x, height * 0.5, base_center.z),
                radius,
                height,
            });

            next_active_tool.set(ActiveTool::Select);
            status_bar_data.hint.clear();
        }
    }
}

fn draw_cylinder_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    cylinder_tool_state: Res<CylinderToolState>,
    mut gizmos: Gizmos,
) {
    let Some(base_center) = cylinder_tool_state.base_center else {
        return;
    };
    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match cylinder_tool_state.phase {
        CylinderToolPhase::Center => {}
        CylinderToolPhase::Radius => {
            let radius = cursor_position.xz().distance(base_center.xz());
            draw_cylinder_outline(&mut gizmos, base_center + Vec3::Y * 0.05, radius, 0.1);
        }
        CylinderToolPhase::Height => {
            let Some(radius) = cylinder_tool_state.radius else {
                return;
            };
            let height = cursor_position.xz().distance(base_center.xz());
            draw_cylinder_outline(
                &mut gizmos,
                base_center + Vec3::Y * (height * 0.5),
                radius,
                height,
            );
        }
    }
}

fn draw_cylinder_outline(gizmos: &mut Gizmos, centre: Vec3, radius: f32, height: f32) {
    let bottom_y = centre.y - height * 0.5;
    let top_y = centre.y + height * 0.5;

    let mut previous_bottom = None;
    let mut previous_top = None;
    let mut first_bottom = None;
    let mut first_top = None;

    for index in 0..=CIRCLE_SEGMENTS {
        let angle = index as f32 / CIRCLE_SEGMENTS as f32 * std::f32::consts::TAU;
        let offset = Vec3::new(radius * angle.cos(), 0.0, radius * angle.sin());
        let bottom = Vec3::new(centre.x, bottom_y, centre.z) + offset;
        let top = Vec3::new(centre.x, top_y, centre.z) + offset;

        if let Some(previous_bottom) = previous_bottom {
            gizmos.line(previous_bottom, bottom, PREVIEW_COLOR);
        } else {
            first_bottom = Some(bottom);
        }
        if let Some(previous_top) = previous_top {
            gizmos.line(previous_top, top, PREVIEW_COLOR);
        } else {
            first_top = Some(top);
        }

        if index % (CIRCLE_SEGMENTS / 4) == 0 && index < CIRCLE_SEGMENTS {
            gizmos.line(bottom, top, PREVIEW_COLOR);
        }

        previous_bottom = Some(bottom);
        previous_top = Some(top);
    }

    if let (Some(first_bottom), Some(last_bottom)) = (first_bottom, previous_bottom) {
        gizmos.line(last_bottom, first_bottom, PREVIEW_COLOR);
    }
    if let (Some(first_top), Some(last_top)) = (first_top, previous_top) {
        gizmos.line(last_top, first_top, PREVIEW_COLOR);
    }
}
