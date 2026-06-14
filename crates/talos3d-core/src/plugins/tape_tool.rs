use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts};
use serde_json::Value;

use crate::plugins::{
    command_registry::{
        activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        CommandResult,
    },
    cursor::CursorWorldPos,
    document_properties::DocumentProperties,
    drawing_export::ViewportExportState,
    egui_chrome::EguiWantsInput,
    snap::SnapSystems,
    tools::ActiveTool,
    ui::StatusBarData,
};

const TAPE_COLOR: Color = Color::srgb(1.0, 0.82, 0.18);
const TAPE_LAST_COLOR: Color = Color::srgba(1.0, 0.82, 0.18, 0.72);
const TAPE_DOT_RADIUS: f32 = 0.055;
const TAPE_MIN_DISTANCE_METRES: f32 = 0.001;

pub struct TapeToolPlugin;

impl Plugin for TapeToolPlugin {
    fn build(&self, app: &mut App) {
        app.register_command(
            CommandDescriptor {
                id: "tools.tape".to_string(),
                label: "Tape".to_string(),
                description: "Measure point-to-point distances".to_string(),
                category: CommandCategory::Tools,
                parameters: None,
                default_shortcut: Some("Shift+T".to_string()),
                icon: None,
                hint: Some("Click start point, then click end point to measure".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: Some("Tape".to_string()),
                capability_id: None,
            },
            execute_tape_tool,
        )
        .add_systems(OnEnter(ActiveTool::Tape), initialize_tape_tool)
        .add_systems(OnExit(ActiveTool::Tape), cleanup_tape_tool)
        .add_systems(
            Update,
            (
                handle_tape_input,
                draw_tape_gizmos,
                draw_tape_overlay.after(draw_tape_gizmos),
            )
                .after(SnapSystems::Resolve)
                .run_if(in_state(ActiveTool::Tape)),
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct TapeMeasurement {
    start: Vec3,
    end: Vec3,
}

impl TapeMeasurement {
    fn distance(self) -> f32 {
        self.start.distance(self.end)
    }

    fn midpoint(self) -> Vec3 {
        (self.start + self.end) * 0.5
    }
}

#[derive(Resource, Default)]
struct TapeToolState {
    start: Option<Vec3>,
    last_measurement: Option<TapeMeasurement>,
}

fn execute_tape_tool(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::Tape)
}

fn initialize_tape_tool(mut commands: Commands, mut status: ResMut<StatusBarData>) {
    commands.insert_resource(TapeToolState::default());
    status.tool_name = "Tape".to_string();
    status.hint = "Click start point".to_string();
}

fn cleanup_tape_tool(mut commands: Commands) {
    commands.remove_resource::<TapeToolState>();
}

fn handle_tape_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    doc_props: Res<DocumentProperties>,
    mut state: ResMut<TapeToolState>,
    mut status: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if !egui_wants_input.keyboard && keys.just_pressed(KeyCode::Escape) {
        if state.start.is_some() || state.last_measurement.is_some() {
            state.start = None;
            state.last_measurement = None;
            status.hint = "Click start point".to_string();
        } else {
            next_active_tool.set(ActiveTool::Select);
        }
        return;
    }

    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor) = tape_cursor_point(&cursor_world_pos) else {
        return;
    };

    match state.start {
        None => {
            state.start = Some(cursor);
            state.last_measurement = None;
            status.hint = "Click end point".to_string();
        }
        Some(start) => {
            let Some(measurement) = tape_measurement(start, cursor) else {
                status.set_feedback("Tape points are too close".to_string(), 1.5);
                return;
            };
            let text = tape_distance_label(measurement.distance(), &doc_props);
            state.start = None;
            state.last_measurement = Some(measurement);
            status.hint = "Click start point".to_string();
            status.set_feedback(format!("Distance: {text}"), 3.0);
        }
    }
}

fn draw_tape_gizmos(
    cursor_world_pos: Res<CursorWorldPos>,
    state: Option<Res<TapeToolState>>,
    mut gizmos: Gizmos,
) {
    let Some(state) = state else {
        return;
    };

    if let Some(start) = state.start {
        if let Some(cursor) = tape_cursor_point(&cursor_world_pos) {
            draw_tape_measurement_gizmo(&mut gizmos, start, cursor, TAPE_COLOR);
        } else {
            gizmos.sphere(
                Isometry3d::from_translation(start),
                TAPE_DOT_RADIUS,
                TAPE_COLOR,
            );
        }
        return;
    }

    if let Some(measurement) = state.last_measurement {
        draw_tape_measurement_gizmo(
            &mut gizmos,
            measurement.start,
            measurement.end,
            TAPE_LAST_COLOR,
        );
    }
}

fn draw_tape_measurement_gizmo(gizmos: &mut Gizmos, start: Vec3, end: Vec3, color: Color) {
    gizmos.line(start, end, color);
    gizmos.sphere(Isometry3d::from_translation(start), TAPE_DOT_RADIUS, color);
    gizmos.sphere(Isometry3d::from_translation(end), TAPE_DOT_RADIUS, color);
}

fn draw_tape_overlay(
    mut contexts: EguiContexts,
    viewport_export_state: Res<ViewportExportState>,
    cursor_world_pos: Res<CursorWorldPos>,
    doc_props: Res<DocumentProperties>,
    state: Option<Res<TapeToolState>>,
    camera_query: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    window_query: Query<&Window, With<PrimaryWindow>>,
) {
    if viewport_export_state.annotation_overlays_suppressed() {
        return;
    }
    let Some(state) = state else {
        return;
    };
    let Some(measurement) = current_tape_measurement(&state, &cursor_world_pos) else {
        return;
    };
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    let Ok(window) = window_query.single() else {
        return;
    };
    let Ok(ctx_ref) = contexts.ctx_mut() else {
        return;
    };
    let Ok(screen_pos) = camera.world_to_viewport(camera_transform, measurement.midpoint()) else {
        return;
    };
    if screen_pos.x < 0.0
        || screen_pos.y < 0.0
        || screen_pos.x > window.width()
        || screen_pos.y > window.height()
    {
        return;
    }

    let text = tape_distance_label(measurement.distance(), &doc_props);
    let pos = egui::pos2(screen_pos.x, screen_pos.y - 18.0);
    let rect = egui::Rect::from_center_size(
        pos,
        egui::vec2(text.chars().count() as f32 * 8.0 + 18.0, 24.0),
    );
    let painter = ctx_ref.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("tape_measure_overlay"),
    ));
    painter.rect_filled(rect, 5.0, egui::Color32::from_black_alpha(185));
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 210, 46)),
        egui::StrokeKind::Outside,
    );
    painter.text(
        pos,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(13.0),
        egui::Color32::WHITE,
    );
}

fn current_tape_measurement(
    state: &TapeToolState,
    cursor_world_pos: &CursorWorldPos,
) -> Option<TapeMeasurement> {
    if let Some(start) = state.start {
        return tape_measurement(start, tape_cursor_point(cursor_world_pos)?);
    }
    state.last_measurement
}

fn tape_cursor_point(cursor_world_pos: &CursorWorldPos) -> Option<Vec3> {
    cursor_world_pos.snapped.or(cursor_world_pos.raw)
}

fn tape_measurement(start: Vec3, end: Vec3) -> Option<TapeMeasurement> {
    (start.distance(end) >= TAPE_MIN_DISTANCE_METRES).then_some(TapeMeasurement { start, end })
}

fn tape_distance_label(distance_metres: f32, doc_props: &DocumentProperties) -> String {
    doc_props
        .display_unit
        .format_value(distance_metres, doc_props.precision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::units::DisplayUnit;

    #[test]
    fn tape_rejects_coincident_points() {
        assert!(tape_measurement(Vec3::ZERO, Vec3::splat(0.0001)).is_none());
        assert!(tape_measurement(Vec3::ZERO, Vec3::X).is_some());
    }

    #[test]
    fn tape_distance_label_uses_document_units() {
        let doc_props = DocumentProperties {
            display_unit: DisplayUnit::Millimetres,
            precision: 0,
            ..Default::default()
        };
        assert_eq!(tape_distance_label(1.25, &doc_props), "1250mm");
    }
}
