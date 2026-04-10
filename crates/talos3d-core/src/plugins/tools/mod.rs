use bevy::prelude::*;

use crate::plugins::{
    input_ownership::{InputOwnership, InputPhase},
    ui::StatusBarData,
};

pub struct ToolPlugin;

impl Plugin for ToolPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<ActiveTool>()
            .add_systems(Update, activate_tools.in_set(InputPhase::ToolInput))
            .add_systems(OnEnter(ActiveTool::Select), enter_select_tool)
            .add_systems(
                OnEnter(ActiveTool::PlaceDimensionLine),
                enter_dimension_line_tool,
            )
            .add_systems(OnEnter(ActiveTool::PlaceGuideLine), enter_guide_line_tool)
            .add_systems(OnEnter(ActiveTool::PlaceWall), enter_wall_tool)
            .add_systems(OnEnter(ActiveTool::PlaceOpening), enter_opening_tool)
            .add_systems(OnEnter(ActiveTool::PlaceBox), enter_box_tool)
            .add_systems(OnEnter(ActiveTool::PlaceCylinder), enter_cylinder_tool)
            .add_systems(OnEnter(ActiveTool::PlaceSphere), enter_sphere_tool)
            .add_systems(OnEnter(ActiveTool::PlacePlane), enter_plane_tool)
            .add_systems(OnEnter(ActiveTool::PlacePolyline), enter_polyline_tool);
    }
}

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActiveTool {
    #[default]
    Select,
    PlaceDimensionLine,
    PlaceGuideLine,
    PlaceWall,
    PlaceOpening,
    PlaceBox,
    PlaceCylinder,
    PlaceSphere,
    PlacePlane,
    PlacePolyline,
}

#[derive(Component)]
pub struct Preview;

fn activate_tools(
    keys: Res<ButtonInput<KeyCode>>,
    active_tool: Res<State<ActiveTool>>,
    ownership: Res<InputOwnership>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if !ownership.is_idle() {
        return;
    }

    let primary_modifier_pressed = if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    };
    if primary_modifier_pressed {
        return;
    }

    let active_tool = active_tool.get();

    if keys.just_pressed(KeyCode::KeyD) && *active_tool != ActiveTool::PlaceDimensionLine {
        next_active_tool.set(ActiveTool::PlaceDimensionLine);
    } else if keys.just_pressed(KeyCode::KeyG) && *active_tool != ActiveTool::PlaceGuideLine {
        next_active_tool.set(ActiveTool::PlaceGuideLine);
    } else if keys.just_pressed(KeyCode::KeyW) && *active_tool != ActiveTool::PlaceWall {
        next_active_tool.set(ActiveTool::PlaceWall);
    } else if keys.just_pressed(KeyCode::KeyO) && *active_tool != ActiveTool::PlaceOpening {
        next_active_tool.set(ActiveTool::PlaceOpening);
    } else if keys.just_pressed(KeyCode::KeyB) && *active_tool != ActiveTool::PlaceBox {
        next_active_tool.set(ActiveTool::PlaceBox);
    } else if keys.just_pressed(KeyCode::KeyC) && *active_tool != ActiveTool::PlaceCylinder {
        next_active_tool.set(ActiveTool::PlaceCylinder);
    } else if keys.just_pressed(KeyCode::KeyP) && *active_tool != ActiveTool::PlacePlane {
        next_active_tool.set(ActiveTool::PlacePlane);
    } else if keys.just_pressed(KeyCode::KeyL) && *active_tool != ActiveTool::PlacePolyline {
        next_active_tool.set(ActiveTool::PlacePolyline);
    }
}

fn enter_select_tool(mut status_bar_data: ResMut<StatusBarData>) {
    status_bar_data.tool_name.clear();
    status_bar_data.tool_name.push_str("Select");
    status_bar_data.hint.clear();
}

fn enter_guide_line_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Guide Line",
        "Click anchor point, then click again to set direction",
    );
}

fn enter_dimension_line_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Dimension",
        "Click a start point, then click an end point to create a dimension",
    );
}

fn enter_wall_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(&mut status_bar_data, "Wall", "Click to place start point");
}

fn enter_opening_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Opening",
        "Hover a wall and click to place opening",
    );
}

fn enter_box_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(&mut status_bar_data, "Box", "Click to place centre");
}

fn enter_cylinder_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(&mut status_bar_data, "Cylinder", "Click to place centre");
}

fn enter_sphere_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(&mut status_bar_data, "Sphere", "Click to place centre");
}

fn enter_plane_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(&mut status_bar_data, "Plane", "Click to place first corner");
}

fn enter_polyline_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Polyline",
        "Click to add points \u{00b7} Enter to finish",
    );
}

fn set_tool_status(status_bar_data: &mut StatusBarData, tool_name: &str, hint: &str) {
    status_bar_data.tool_name.clear();
    status_bar_data.tool_name.push_str(tool_name);
    status_bar_data.hint.clear();
    status_bar_data.hint.push_str(hint);
}
