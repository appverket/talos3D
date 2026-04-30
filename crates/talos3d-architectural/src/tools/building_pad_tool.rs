use bevy::{ecs::system::SystemState, prelude::*};
use talos3d_core::{
    capability_registry::TerrainProviderRegistry,
    plugins::{
        commands::CreateEntityCommand, cursor::CursorWorldPos, egui_chrome::EguiWantsInput,
        identity::ElementIdAllocator, snap::SnapResult, tools::ActiveTool, ui::StatusBarData,
    },
};

use crate::{components::BuildingPad, snapshots::BuildingPadSnapshot};

const PAD_PREVIEW_COLOR: Color = Color::srgb(0.22, 0.82, 0.76);
const PAD_CURRENT_EDGE_COLOR: Color = Color::srgba(0.22, 0.82, 0.76, 0.45);
const PAD_POINT_COLOR: Color = Color::srgb(1.0, 0.9, 0.28);
const DEPTH_CUT_COLOR: Color = Color::srgb(0.92, 0.22, 0.12);
const DEPTH_FILL_COLOR: Color = Color::srgb(0.18, 0.48, 0.95);
const MIN_PAD_EDGE_LENGTH_METRES: f32 = 0.1;

pub struct BuildingPadToolPlugin;

impl Plugin for BuildingPadToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(ActiveTool::PlaceBuildingPad),
            initialize_building_pad_tool,
        )
        .add_systems(
            OnExit(ActiveTool::PlaceBuildingPad),
            cleanup_building_pad_tool,
        )
        .add_systems(
            Update,
            (
                cancel_building_pad_tool,
                handle_building_pad_clicks,
                finish_building_pad_on_enter,
                draw_building_pad_preview,
            )
                .run_if(in_state(ActiveTool::PlaceBuildingPad)),
        );
    }
}

#[derive(Resource, Debug, Clone, Default)]
struct BuildingPadToolState {
    boundary: Vec<Vec2>,
    pad_elevation: Option<f32>,
}

fn initialize_building_pad_tool(mut commands: Commands) {
    commands.insert_resource(BuildingPadToolState::default());
}

fn cleanup_building_pad_tool(mut commands: Commands) {
    commands.remove_resource::<BuildingPadToolState>();
}

fn cancel_building_pad_tool(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut state: ResMut<BuildingPadToolState>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    if !state.boundary.is_empty() {
        state.boundary.clear();
        state.pad_elevation = None;
        status_bar_data.hint = "Click vertices \u{00b7} Enter to close".to_string();
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn handle_building_pad_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    snap_result: Option<Res<SnapResult>>,
    mut state: ResMut<BuildingPadToolState>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(position) = pointer_position(&cursor_world_pos, snap_result.as_deref()) else {
        return;
    };
    let point = position.xz();
    let should_add = state
        .boundary
        .last()
        .map(|last| last.distance(point) >= MIN_PAD_EDGE_LENGTH_METRES)
        .unwrap_or(true);
    if !should_add {
        return;
    }

    if state.boundary.is_empty() {
        state.pad_elevation = Some(position.y);
    }
    state.boundary.push(point);
    status_bar_data.hint = format!(
        "{} vertices \u{00b7} Enter to close \u{00b7} pad elevation {:.2} m",
        state.boundary.len(),
        state.pad_elevation.unwrap_or(position.y)
    );
}

fn finish_building_pad_on_enter(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut state: ResMut<BuildingPadToolState>,
    allocator: Res<ElementIdAllocator>,
    mut create_entities: MessageWriter<CreateEntityCommand>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Enter) {
        return;
    }
    if state.boundary.len() < 3 {
        status_bar_data.set_feedback(
            "Building pad needs at least three vertices".to_string(),
            2.0,
        );
        return;
    }
    if polygon_area_abs(&state.boundary) <= f32::EPSILON {
        status_bar_data.set_feedback("Building pad boundary must enclose area".to_string(), 2.0);
        return;
    }

    let element_id = allocator.next_id();
    let pad_elevation = state.pad_elevation.unwrap_or(0.0);
    create_entities.write(CreateEntityCommand {
        snapshot: BuildingPadSnapshot {
            element_id,
            building_pad: BuildingPad {
                boundary: state.boundary.clone(),
                pad_elevation,
            },
            excavation_volume: None,
            material_assignment: None,
        }
        .into(),
    });

    status_bar_data.set_feedback(format!("Queued building pad {}", element_id.0), 2.0);
    state.boundary.clear();
    state.pad_elevation = None;
    next_active_tool.set(ActiveTool::Select);
}

fn draw_building_pad_preview(world: &mut World) {
    let (state, current_position, provider) = {
        let mut system_state: SystemState<(
            Option<Res<BuildingPadToolState>>,
            Res<CursorWorldPos>,
            Option<Res<SnapResult>>,
            Option<Res<TerrainProviderRegistry>>,
        )> = SystemState::new(world);
        let (state, cursor_world_pos, snap_result, terrain_registry) = system_state.get_mut(world);
        let Some(state) = state.map(|state| state.clone()) else {
            return;
        };
        let current_position = pointer_position(&cursor_world_pos, snap_result.as_deref());
        let provider = terrain_registry.and_then(|registry| registry.provider());
        (state, current_position, provider)
    };

    let pad_elevation = state
        .pad_elevation
        .or_else(|| current_position.map(|position| position.y))
        .unwrap_or(0.0);
    let mut preview_boundary = state.boundary.clone();
    if let Some(position) = current_position {
        preview_boundary.push(position.xz());
    }

    let depth_markers = provider
        .map(|provider| {
            preview_boundary
                .iter()
                .filter_map(|point| {
                    provider
                        .elevation_at(world, point.x, point.y)
                        .map(|terrain_y| (*point, terrain_y))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut system_state: SystemState<Gizmos> = SystemState::new(world);
    let mut gizmos = system_state.get_mut(world);
    draw_pad_outline(&mut gizmos, &preview_boundary, pad_elevation);
    for (point, terrain_y) in depth_markers {
        draw_corner_depth_marker(&mut gizmos, point, pad_elevation, terrain_y);
    }
}

fn pointer_position(
    cursor_world_pos: &CursorWorldPos,
    snap_result: Option<&SnapResult>,
) -> Option<Vec3> {
    snap_result
        .and_then(|snap| snap.position)
        .or(cursor_world_pos.snapped)
        .or(cursor_world_pos.raw)
}

fn draw_pad_outline(gizmos: &mut Gizmos, boundary: &[Vec2], pad_elevation: f32) {
    for segment in boundary.windows(2) {
        gizmos.line(
            Vec3::new(segment[0].x, pad_elevation, segment[0].y),
            Vec3::new(segment[1].x, pad_elevation, segment[1].y),
            PAD_PREVIEW_COLOR,
        );
    }
    if boundary.len() >= 3 {
        gizmos.line(
            Vec3::new(
                boundary[boundary.len() - 1].x,
                pad_elevation,
                boundary[boundary.len() - 1].y,
            ),
            Vec3::new(boundary[0].x, pad_elevation, boundary[0].y),
            PAD_CURRENT_EDGE_COLOR,
        );
    }
    for point in boundary {
        gizmos.sphere(
            Isometry3d::from_translation(Vec3::new(point.x, pad_elevation, point.y)),
            0.05,
            PAD_POINT_COLOR,
        );
    }
}

fn draw_corner_depth_marker(gizmos: &mut Gizmos, point: Vec2, pad_elevation: f32, terrain_y: f32) {
    let pad_point = Vec3::new(point.x, pad_elevation, point.y);
    let terrain_point = Vec3::new(point.x, terrain_y, point.y);
    let color = if terrain_y >= pad_elevation {
        DEPTH_CUT_COLOR
    } else {
        DEPTH_FILL_COLOR
    };
    gizmos.line(pad_point, terrain_point, color);
    gizmos.sphere(Isometry3d::from_translation(terrain_point), 0.035, color);
}

fn polygon_area_abs(points: &[Vec2]) -> f32 {
    let mut area = 0.0;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }
    area.abs() * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_position_prefers_snap_result() {
        let cursor = CursorWorldPos {
            raw: Some(Vec3::new(1.0, 0.0, 1.0)),
            snapped: Some(Vec3::new(2.0, 0.0, 2.0)),
            hovered_element_id: None,
        };
        let snap = SnapResult {
            raw_position: cursor.raw,
            position: Some(Vec3::new(3.0, 0.0, 3.0)),
            target: Some(Vec3::new(3.0, 0.0, 3.0)),
            kind: Default::default(),
        };

        assert_eq!(pointer_position(&cursor, Some(&snap)), snap.position);
    }

    #[test]
    fn polygon_area_rejects_collinear_boundary() {
        let area = polygon_area_abs(&[
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(2.0, 0.0),
        ]);

        assert_eq!(area, 0.0);
    }
}
