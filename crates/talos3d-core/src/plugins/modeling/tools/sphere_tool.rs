use bevy::prelude::*;

use crate::plugins::{
    commands::CreateSphereCommand, cursor::CursorWorldPos, egui_chrome::EguiWantsInput,
    tools::ActiveTool, ui::StatusBarData,
};

const PREVIEW_COLOR: Color = Color::srgb(0.45, 0.9, 1.0);
const MIN_SPHERE_RADIUS_METRES: f32 = 0.1;

pub struct SphereToolPlugin;

impl Plugin for SphereToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveTool::PlaceSphere), initialize_sphere_tool)
            .add_systems(OnExit(ActiveTool::PlaceSphere), cleanup_sphere_tool)
            .add_systems(
                Update,
                (
                    cancel_sphere_tool,
                    handle_sphere_clicks,
                    draw_sphere_preview,
                )
                    .run_if(in_state(ActiveTool::PlaceSphere)),
            );
    }
}

#[derive(Resource, Default)]
struct SphereToolState {
    base_center: Option<Vec3>,
}

fn initialize_sphere_tool(mut commands: Commands) {
    commands.insert_resource(SphereToolState::default());
}

fn cleanup_sphere_tool(mut commands: Commands) {
    commands.remove_resource::<SphereToolState>();
}

fn cancel_sphere_tool(
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

fn handle_sphere_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut sphere_tool_state: ResMut<SphereToolState>,
    mut create_sphere_commands: MessageWriter<CreateSphereCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match sphere_tool_state.base_center {
        None => {
            sphere_tool_state.base_center = Some(cursor_position);
            status_bar_data.hint = "Click to confirm radius".to_string();
        }
        Some(base_center) => {
            let radius = cursor_position.xz().distance(base_center.xz());
            if radius < MIN_SPHERE_RADIUS_METRES {
                return;
            }

            create_sphere_commands.write(CreateSphereCommand {
                centre: Vec3::new(base_center.x, radius, base_center.z),
                radius,
            });

            next_active_tool.set(ActiveTool::Select);
            status_bar_data.hint.clear();
        }
    }
}

fn draw_sphere_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    sphere_tool_state: Res<SphereToolState>,
    mut gizmos: Gizmos,
) {
    let Some(base_center) = sphere_tool_state.base_center else {
        return;
    };
    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    let radius = cursor_position.xz().distance(base_center.xz());
    if radius < MIN_SPHERE_RADIUS_METRES {
        return;
    }

    gizmos.sphere(
        Isometry3d::from_translation(Vec3::new(base_center.x, radius, base_center.z)),
        radius,
        PREVIEW_COLOR,
    );
}
