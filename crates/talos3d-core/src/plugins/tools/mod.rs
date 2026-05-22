use bevy::prelude::*;
use serde_json::Value;

use crate::plugins::{
    command_registry::{
        activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        CommandResult,
    },
    ui::StatusBarData,
};

pub struct ToolPlugin;

impl Plugin for ToolPlugin {
    fn build(&self, app: &mut App) {
        // Wall/Opening previously had keyboard-only activation with no command.
        // Register them so their shortcuts flow through the unified keymap and
        // are covered by conflict detection like every other tool.
        app.register_command(
            CommandDescriptor {
                id: "architectural.place_wall".to_string(),
                label: "Wall".to_string(),
                description: "Activate wall placement".to_string(),
                category: CommandCategory::Create,
                parameters: None,
                default_shortcut: None,
                icon: None,
                hint: Some("Click to place start point".to_string()),
                requires_selection: false,
                show_in_menu: false,
                version: 1,
                activates_tool: Some("PlaceWall".to_string()),
                capability_id: None,
            },
            execute_place_wall,
        )
        .register_command(
            CommandDescriptor {
                id: "architectural.place_opening".to_string(),
                label: "Place Opening".to_string(),
                description: "Activate opening placement".to_string(),
                category: CommandCategory::Create,
                parameters: None,
                default_shortcut: Some("O".to_string()),
                icon: Some("icon.architectural.opening".to_string()),
                hint: Some("Hover a wall and click to place an opening".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: Some("PlaceOpening".to_string()),
                capability_id: Some("architectural".to_string()),
            },
            execute_place_opening,
        );

        app.init_state::<ActiveTool>()
            .add_systems(OnEnter(ActiveTool::Select), enter_select_tool)
            .add_systems(
                OnEnter(ActiveTool::PlaceDimensionLine),
                enter_dimension_line_tool,
            )
            .add_systems(OnEnter(ActiveTool::PlaceGuideLine), enter_guide_line_tool)
            .add_systems(OnEnter(ActiveTool::PlaceWall), enter_wall_tool)
            .add_systems(OnEnter(ActiveTool::PlaceOpening), enter_opening_tool)
            .add_systems(
                OnEnter(ActiveTool::PlaceBuildingPad),
                enter_building_pad_tool,
            )
            .add_systems(OnEnter(ActiveTool::PlaceBox), enter_box_tool)
            .add_systems(OnEnter(ActiveTool::PlaceCylinder), enter_cylinder_tool)
            .add_systems(OnEnter(ActiveTool::PlaceSphere), enter_sphere_tool)
            .add_systems(OnEnter(ActiveTool::PlacePlane), enter_plane_tool)
            .add_systems(OnEnter(ActiveTool::PlacePolyline), enter_polyline_tool)
            .add_systems(
                OnEnter(ActiveTool::PlaceTerrainElevationCurve),
                enter_terrain_elevation_curve_tool,
            )
            .add_systems(
                OnEnter(ActiveTool::PlaceTerrainSpotElevation),
                enter_terrain_spot_elevation_tool,
            );
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
    PlaceBuildingPad,
    PlaceBox,
    PlaceCylinder,
    PlaceSphere,
    PlacePlane,
    PlacePolyline,
    PlaceTerrainElevationCurve,
    PlaceTerrainSpotElevation,
}

#[derive(Component)]
pub struct Preview;

fn execute_place_wall(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceWall)
}

fn execute_place_opening(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceOpening)
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
        "Click anchor or edge, then drag/click to place the guide · X/Y/Z locks axis · type angle + Enter",
    );
}

fn enter_dimension_line_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Dimension",
        "Click start, click end, then click to place the offset dimension line",
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

fn enter_building_pad_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Building Pad",
        "Click vertices \u{00b7} Enter to close",
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

fn enter_terrain_elevation_curve_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Elevation Curve",
        "Click terrain points \u{00b7} Enter to finish",
    );
}

fn enter_terrain_spot_elevation_tool(mut status_bar_data: ResMut<StatusBarData>) {
    set_tool_status(
        &mut status_bar_data,
        "Spot Elevation",
        "Click terrain to place spot elevation",
    );
}

fn set_tool_status(status_bar_data: &mut StatusBarData, tool_name: &str, hint: &str) {
    status_bar_data.tool_name.clear();
    status_bar_data.tool_name.push_str(tool_name);
    status_bar_data.hint.clear();
    status_bar_data.hint.push_str(hint);
}
