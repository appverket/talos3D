use bevy::{ecs::system::SystemParam, prelude::*};
use talos3d_core::plugins::{
    commands::{
        ApplyEntityChangesCommand, BeginCommandGroup, CreateEntityCommand, EndCommandGroup,
    },
    cursor::CursorWorldPos,
    egui_chrome::EguiWantsInput,
    identity::{ElementId, ElementIdAllocator},
    selection::Selected,
    tools::ActiveTool,
    ui::StatusBarData,
};

use crate::{
    components::{ElevationCurve, ElevationCurveType, TerrainSurface},
    snapshots::{ElevationCurveSnapshot, TerrainSurfaceSnapshot},
};

const PREVIEW_COLOR: Color = Color::srgb(0.20, 0.88, 0.92);
const POINT_COLOR: Color = Color::srgb(1.0, 0.92, 0.28);
const MIN_CURVE_SEGMENT_LENGTH_METRES: f32 = 0.1;

pub struct TerrainToolPlugin;

impl Plugin for TerrainToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(ActiveTool::PlaceTerrainElevationCurve),
            initialize_elevation_curve_tool,
        )
        .add_systems(
            OnExit(ActiveTool::PlaceTerrainElevationCurve),
            cleanup_elevation_curve_tool,
        )
        .add_systems(
            Update,
            (
                cancel_elevation_curve_tool,
                handle_elevation_curve_click,
                finish_elevation_curve_on_enter,
                draw_elevation_curve_tool_preview,
            )
                .run_if(in_state(ActiveTool::PlaceTerrainElevationCurve)),
        )
        .add_systems(
            Update,
            place_spot_elevation_on_click.run_if(in_state(ActiveTool::PlaceTerrainSpotElevation)),
        );
    }
}

#[derive(Resource, Debug, Clone, Default)]
struct ElevationCurveToolState {
    surface_id: Option<ElementId>,
    elevation: Option<f32>,
    points: Vec<Vec3>,
}

#[derive(SystemParam)]
struct TerrainCurveQueue<'w, 's> {
    allocator: Res<'w, ElementIdAllocator>,
    surfaces: Query<
        'w,
        's,
        (
            &'static ElementId,
            &'static TerrainSurface,
            Option<&'static Selected>,
        ),
    >,
    begin_groups: MessageWriter<'w, BeginCommandGroup>,
    create_entities: MessageWriter<'w, CreateEntityCommand>,
    apply_changes: MessageWriter<'w, ApplyEntityChangesCommand>,
    end_groups: MessageWriter<'w, EndCommandGroup>,
}

fn initialize_elevation_curve_tool(mut commands: Commands) {
    commands.insert_resource(ElevationCurveToolState::default());
}

fn cleanup_elevation_curve_tool(mut commands: Commands) {
    commands.remove_resource::<ElevationCurveToolState>();
}

fn cancel_elevation_curve_tool(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut state: ResMut<ElevationCurveToolState>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    if !state.points.is_empty() {
        state.points.clear();
        state.surface_id = None;
        state.elevation = None;
        status_bar_data.hint = "Click terrain points \u{00b7} Enter to finish".to_string();
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn handle_elevation_curve_click(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut state: ResMut<ElevationCurveToolState>,
    surfaces: Query<(&ElementId, &TerrainSurface, Option<&Selected>)>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };
    let Some(surface_id) = resolve_terrain_surface_id(&cursor_world_pos, surfaces.iter()) else {
        status_bar_data.set_feedback(
            "Click a terrain surface to start an elevation curve".to_string(),
            2.0,
        );
        return;
    };

    if state.surface_id.is_none() {
        state.surface_id = Some(surface_id);
        state.elevation = Some(cursor_position.y);
    } else if state.surface_id != Some(surface_id) {
        status_bar_data.set_feedback(
            "Finish the current curve before switching terrain surfaces".to_string(),
            2.0,
        );
        return;
    }

    let elevation = state.elevation.unwrap_or(cursor_position.y);
    let point = Vec3::new(cursor_position.x, elevation, cursor_position.z);
    let should_add = state
        .points
        .last()
        .map(|last| last.xz().distance(point.xz()) >= MIN_CURVE_SEGMENT_LENGTH_METRES)
        .unwrap_or(true);

    if should_add {
        state.points.push(point);
        status_bar_data.hint = format!(
            "{} point{} at {:.2} m \u{00b7} Enter to finish",
            state.points.len(),
            if state.points.len() == 1 { "" } else { "s" },
            elevation
        );
    }
}

fn finish_elevation_curve_on_enter(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut state: ResMut<ElevationCurveToolState>,
    mut queue: TerrainCurveQueue,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Enter) {
        return;
    }
    if state.points.len() < 2 {
        status_bar_data.set_feedback("Elevation curve needs at least two points".to_string(), 2.0);
        return;
    }

    let Some(surface_id) = state.surface_id else {
        return;
    };
    let Some(surface) = queue
        .surfaces
        .iter()
        .find(|(element_id, _, _)| **element_id == surface_id)
        .map(|(_, surface, _)| surface.clone())
    else {
        status_bar_data.set_feedback("Terrain surface was not found".to_string(), 2.0);
        return;
    };

    let elevation = state.elevation.unwrap_or(state.points[0].y);
    let curve = ElevationCurve {
        points: state.points.clone(),
        elevation,
        source_layer: "Terrain Tool".to_string(),
        curve_type: ElevationCurveType::Supplementary,
        survey_source_id: None,
    };
    let curve_id = queue_elevation_curve(
        &mut queue,
        surface_id,
        &surface,
        curve,
        "Add elevation curve",
    );

    status_bar_data.set_feedback(format!("Queued elevation curve {}", curve_id.0), 2.0);
    state.points.clear();
    state.surface_id = None;
    state.elevation = None;
    next_active_tool.set(ActiveTool::Select);
}

fn draw_elevation_curve_tool_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    state: Res<ElevationCurveToolState>,
    mut gizmos: Gizmos,
) {
    for segment in state.points.windows(2) {
        gizmos.line(segment[0], segment[1], PREVIEW_COLOR);
    }

    if let (Some(last), Some(cursor), Some(elevation)) = (
        state.points.last(),
        cursor_world_pos.snapped,
        state.elevation,
    ) {
        gizmos.line(
            *last,
            Vec3::new(cursor.x, elevation, cursor.z),
            PREVIEW_COLOR.with_alpha(0.5),
        );
    }

    for point in &state.points {
        gizmos.sphere(Isometry3d::from_translation(*point), 0.04, POINT_COLOR);
    }
}

fn place_spot_elevation_on_click(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut queue: TerrainCurveQueue,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };
    let Some(surface_id) = resolve_terrain_surface_id(&cursor_world_pos, queue.surfaces.iter())
    else {
        status_bar_data.set_feedback(
            "Click a terrain surface to place a spot elevation".to_string(),
            2.0,
        );
        return;
    };
    let Some(surface) = queue
        .surfaces
        .iter()
        .find(|(element_id, _, _)| **element_id == surface_id)
        .map(|(_, surface, _)| surface.clone())
    else {
        status_bar_data.set_feedback("Terrain surface was not found".to_string(), 2.0);
        return;
    };

    let curve = ElevationCurve {
        points: vec![cursor_position],
        elevation: cursor_position.y,
        source_layer: "Spot Elevations".to_string(),
        curve_type: ElevationCurveType::Supplementary,
        survey_source_id: None,
    };
    let curve_id = queue_elevation_curve(
        &mut queue,
        surface_id,
        &surface,
        curve,
        "Add spot elevation",
    );

    status_bar_data.set_feedback(format!("Queued spot elevation {}", curve_id.0), 2.0);
    next_active_tool.set(ActiveTool::Select);
}

fn resolve_terrain_surface_id<'a>(
    cursor_world_pos: &CursorWorldPos,
    surfaces: impl IntoIterator<Item = (&'a ElementId, &'a TerrainSurface, Option<&'a Selected>)>,
) -> Option<ElementId> {
    let hovered_surface_id = cursor_world_pos.hovered_element_id;
    let mut selected_surface_id = None;
    for (element_id, _, selected) in surfaces {
        if Some(*element_id) == hovered_surface_id {
            return Some(*element_id);
        }
        if selected.is_some() {
            selected_surface_id = Some(*element_id);
        }
    }
    selected_surface_id
}

fn queue_elevation_curve(
    queue: &mut TerrainCurveQueue,
    surface_id: ElementId,
    surface: &TerrainSurface,
    curve: ElevationCurve,
    label: &'static str,
) -> ElementId {
    let curve_id = queue.allocator.next_id();
    let mut updated_surface = surface.clone();
    updated_surface.source_curve_ids.push(curve_id);

    queue.begin_groups.write(BeginCommandGroup { label });
    queue.create_entities.write(CreateEntityCommand {
        snapshot: ElevationCurveSnapshot {
            element_id: curve_id,
            curve,
        }
        .into(),
    });
    queue.apply_changes.write(ApplyEntityChangesCommand {
        label: "Update terrain surface sources",
        before: vec![TerrainSurfaceSnapshot {
            element_id: surface_id,
            surface: surface.clone(),
        }
        .into()],
        after: vec![TerrainSurfaceSnapshot {
            element_id: surface_id,
            surface: updated_surface,
        }
        .into()],
    });
    queue.end_groups.write(EndCommandGroup);

    curve_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_terrain_surface_prefers_hovered_surface_over_selected() {
        let mut world = World::new();
        world.spawn((
            ElementId(1),
            Selected,
            TerrainSurface::new("Selected".to_string(), vec![]),
        ));
        world.spawn((
            ElementId(2),
            TerrainSurface::new("Hovered".to_string(), vec![]),
        ));

        let mut query = world.query::<(&ElementId, &TerrainSurface, Option<&Selected>)>();
        let cursor = CursorWorldPos {
            raw: None,
            snapped: None,
            hovered_element_id: Some(ElementId(2)),
        };

        assert_eq!(
            resolve_terrain_surface_id(&cursor, query.iter(&world)),
            Some(ElementId(2))
        );
    }
}
