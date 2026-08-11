//! Pointer presentation for authored Draft primitives.
//!
//! This module owns no authoring semantics. It collects points in the shared
//! `DrawingPlane`, then queues the same registered commands used by MCP.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use serde_json::json;

use crate::plugins::{
    command_registry::{queue_command_invocation_resource, PendingCommandInvocations},
    cursor::{CursorWorldPos, DrawingPlane},
    input_ownership::InputOwnership,
    tools::ActiveTool,
    ui::StatusBarData,
};

use super::DraftingWorkspaceState;

const PREVIEW_COLOR: Color = Color::srgb(0.1, 0.35, 0.85);
const MIN_SIZE: f32 = 1e-4;

pub(crate) struct DraftPrimitiveToolPlugin;

impl Plugin for DraftPrimitiveToolPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DraftPrimitiveToolState>();
        for tool in draft_tools() {
            app.add_systems(OnEnter(tool.clone()), begin_tool)
                .add_systems(OnExit(tool), clear_tool);
        }
        app.add_systems(
            Update,
            (
                cancel_tool,
                handle_pointer_click,
                finish_polyline,
                draw_preview,
                show_text_dialog,
            )
                .run_if(draft_tool_active),
        );
    }
}

fn draft_tools() -> [ActiveTool; 5] {
    [
        ActiveTool::PlaceDraftLine,
        ActiveTool::PlaceDraftPolyline,
        ActiveTool::PlaceDraftRectangle,
        ActiveTool::PlaceDraftCircle,
        ActiveTool::PlaceDraftText,
    ]
}

fn draft_tool_active(active: Res<State<ActiveTool>>) -> bool {
    draft_tools().contains(active.get())
}

#[derive(Resource, Default)]
struct DraftPrimitiveToolState {
    points: Vec<Vec2>,
    text_anchor: Option<Vec2>,
    text_buffer: String,
}

struct DraftCommandInvocation {
    id: &'static str,
    parameters: serde_json::Value,
}

fn begin_tool(
    active: Res<State<ActiveTool>>,
    mut state: ResMut<DraftPrimitiveToolState>,
    mut status: ResMut<StatusBarData>,
) {
    *state = DraftPrimitiveToolState::default();
    let (name, hint) = match active.get() {
        ActiveTool::PlaceDraftLine => ("Draft Line", "Click start, then end"),
        ActiveTool::PlaceDraftPolyline => {
            ("Draft Polyline", "Click points, then press Enter to finish")
        }
        ActiveTool::PlaceDraftRectangle => ("Draft Rectangle", "Click two opposite corners"),
        ActiveTool::PlaceDraftCircle => ("Draft Circle", "Click center, then radius"),
        ActiveTool::PlaceDraftText => ("Draft Text", "Click an anchor, then enter text"),
        _ => return,
    };
    status.tool_name = name.to_string();
    status.hint = hint.to_string();
}

fn clear_tool(mut state: ResMut<DraftPrimitiveToolState>) {
    *state = DraftPrimitiveToolState::default();
}

fn cancel_tool(
    keys: Res<ButtonInput<KeyCode>>,
    ownership: Res<InputOwnership>,
    mut state: ResMut<DraftPrimitiveToolState>,
    mut next_tool: ResMut<NextState<ActiveTool>>,
) {
    if !ownership.is_idle() || !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    if state.points.is_empty() && state.text_anchor.is_none() {
        next_tool.set(ActiveTool::Select);
    } else {
        *state = DraftPrimitiveToolState::default();
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_pointer_click(
    active: Res<State<ActiveTool>>,
    mouse: Res<ButtonInput<MouseButton>>,
    ownership: Res<InputOwnership>,
    cursor: Res<CursorWorldPos>,
    plane: Res<DrawingPlane>,
    workspace: Res<DraftingWorkspaceState>,
    mut state: ResMut<DraftPrimitiveToolState>,
    mut pending: ResMut<PendingCommandInvocations>,
    mut next_tool: ResMut<NextState<ActiveTool>>,
) {
    if !ownership.is_idle()
        || !mouse.just_pressed(MouseButton::Left)
        || !workspace.is_active()
        || workspace.active_draft_id().is_none()
    {
        return;
    }
    let Some(world_point) = cursor.snapped else {
        return;
    };
    let point = plane.project_to_2d(world_point);
    match active.get() {
        ActiveTool::PlaceDraftText => {
            state.text_anchor = Some(point);
            state.text_buffer.clear();
        }
        ActiveTool::PlaceDraftPolyline => {
            if state
                .points
                .last()
                .is_none_or(|last| last.distance(point) >= MIN_SIZE)
            {
                state.points.push(point);
            }
        }
        ActiveTool::PlaceDraftLine
        | ActiveTool::PlaceDraftRectangle
        | ActiveTool::PlaceDraftCircle => {
            state.points.push(point);
            if state.points.len() < 2 {
                return;
            }
            let a = state.points[0];
            let b = state.points[1];
            let invocation = two_point_invocation(active.get(), a, b);
            if let Some(invocation) = invocation {
                queue_command_invocation_resource(
                    &mut pending,
                    invocation.id,
                    invocation.parameters,
                );
                next_tool.set(ActiveTool::Select);
            }
        }
        _ => {}
    }
}

fn two_point_invocation(tool: &ActiveTool, a: Vec2, b: Vec2) -> Option<DraftCommandInvocation> {
    match tool {
        ActiveTool::PlaceDraftLine if a.distance(b) >= MIN_SIZE => Some(DraftCommandInvocation {
            id: "drafting.create_line",
            parameters: json!({"a":a,"b":b}),
        }),
        ActiveTool::PlaceDraftRectangle if (b - a).abs().cmpgt(Vec2::splat(MIN_SIZE)).all() => {
            Some(DraftCommandInvocation {
                id: "drafting.create_rectangle",
                parameters: json!({"a":a,"b":b}),
            })
        }
        ActiveTool::PlaceDraftCircle if a.distance(b) >= MIN_SIZE => Some(DraftCommandInvocation {
            id: "drafting.create_circle",
            parameters: json!({"center":a,"radius":a.distance(b)}),
        }),
        _ => None,
    }
}

fn polyline_invocation(points: &[Vec2]) -> Option<DraftCommandInvocation> {
    (points.len() >= 2).then(|| DraftCommandInvocation {
        id: "drafting.create_polyline",
        parameters: json!({"points":points,"closed":false}),
    })
}

fn text_invocation(anchor: Vec2, content: &str) -> Option<DraftCommandInvocation> {
    let content = content.trim();
    (!content.is_empty()).then(|| DraftCommandInvocation {
        id: "drafting.create_text",
        parameters: json!({"anchor":anchor,"content":content}),
    })
}

fn finish_polyline(
    active: Res<State<ActiveTool>>,
    keys: Res<ButtonInput<KeyCode>>,
    ownership: Res<InputOwnership>,
    state: Res<DraftPrimitiveToolState>,
    mut pending: ResMut<PendingCommandInvocations>,
    mut next_tool: ResMut<NextState<ActiveTool>>,
) {
    if *active.get() != ActiveTool::PlaceDraftPolyline
        || !ownership.is_idle()
        || !keys.just_pressed(KeyCode::Enter)
        || state.points.len() < 2
    {
        return;
    }
    let Some(invocation) = polyline_invocation(&state.points) else {
        return;
    };
    queue_command_invocation_resource(&mut pending, invocation.id, invocation.parameters);
    next_tool.set(ActiveTool::Select);
}

fn draw_preview(
    active: Res<State<ActiveTool>>,
    state: Res<DraftPrimitiveToolState>,
    cursor: Res<CursorWorldPos>,
    plane: Res<DrawingPlane>,
    mut gizmos: Gizmos,
) {
    let cursor_local = cursor.snapped.map(|point| plane.project_to_2d(point));
    let mut segment = |a: Vec2, b: Vec2| {
        gizmos.line(plane.to_world(a), plane.to_world(b), PREVIEW_COLOR);
    };
    match active.get() {
        ActiveTool::PlaceDraftLine => {
            if let (Some(a), Some(b)) = (state.points.first(), cursor_local) {
                segment(*a, b);
            }
        }
        ActiveTool::PlaceDraftPolyline => {
            for pair in state.points.windows(2) {
                segment(pair[0], pair[1]);
            }
            if let (Some(a), Some(b)) = (state.points.last(), cursor_local) {
                segment(*a, b);
            }
        }
        ActiveTool::PlaceDraftRectangle => {
            if let (Some(a), Some(b)) = (state.points.first(), cursor_local) {
                let c = Vec2::new(b.x, a.y);
                let d = Vec2::new(a.x, b.y);
                for (p, q) in [(a, &c), (&c, &b), (&b, &d), (&d, a)] {
                    segment(*p, *q);
                }
            }
        }
        ActiveTool::PlaceDraftCircle => {
            if let (Some(center), Some(edge)) = (state.points.first(), cursor_local) {
                let radius = center.distance(edge);
                for index in 0..64 {
                    let a = std::f32::consts::TAU * index as f32 / 64.0;
                    let b = std::f32::consts::TAU * (index + 1) as f32 / 64.0;
                    segment(
                        *center + Vec2::new(a.cos(), a.sin()) * radius,
                        *center + Vec2::new(b.cos(), b.sin()) * radius,
                    );
                }
            }
        }
        ActiveTool::PlaceDraftText => {
            if let Some(anchor) = state.text_anchor.or(cursor_local) {
                let center = plane.to_world(anchor);
                gizmos.line(
                    center - plane.tangent * 0.1,
                    center + plane.tangent * 0.1,
                    PREVIEW_COLOR,
                );
                gizmos.line(
                    center - plane.bitangent * 0.1,
                    center + plane.bitangent * 0.1,
                    PREVIEW_COLOR,
                );
            }
        }
        _ => {}
    }
}

fn show_text_dialog(
    active: Res<State<ActiveTool>>,
    mut contexts: EguiContexts,
    mut state: ResMut<DraftPrimitiveToolState>,
    mut pending: ResMut<PendingCommandInvocations>,
    mut next_tool: ResMut<NextState<ActiveTool>>,
) {
    if *active.get() != ActiveTool::PlaceDraftText || state.text_anchor.is_none() {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut submit = false;
    let mut cancel = false;
    egui::Window::new("Draft text")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut state.text_buffer)
                    .hint_text("Annotation text")
                    .desired_width(320.0),
            );
            response.request_focus();
            submit |=
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                cancel |= ui.button("Cancel").clicked();
                submit |= ui
                    .add_enabled(
                        !state.text_buffer.trim().is_empty(),
                        egui::Button::new("Create"),
                    )
                    .clicked();
            });
        });
    if cancel {
        next_tool.set(ActiveTool::Select);
    } else if submit {
        if let Some(invocation) = text_invocation(state.text_anchor.unwrap(), &state.text_buffer) {
            queue_command_invocation_resource(&mut pending, invocation.id, invocation.parameters);
            next_tool.set(ActiveTool::Select);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_adapters_emit_the_registered_semantic_commands() {
        let a = Vec2::new(1.0, 2.0);
        let b = Vec2::new(4.0, 6.0);
        let line = two_point_invocation(&ActiveTool::PlaceDraftLine, a, b).unwrap();
        assert_eq!(line.id, "drafting.create_line");
        assert_eq!(line.parameters, json!({"a":a,"b":b}));

        let rectangle = two_point_invocation(&ActiveTool::PlaceDraftRectangle, a, b).unwrap();
        assert_eq!(rectangle.id, "drafting.create_rectangle");
        let circle = two_point_invocation(&ActiveTool::PlaceDraftCircle, a, b).unwrap();
        assert_eq!(circle.id, "drafting.create_circle");
        assert_eq!(circle.parameters["radius"], 5.0);

        let polyline = polyline_invocation(&[a, b]).unwrap();
        assert_eq!(polyline.id, "drafting.create_polyline");
        let text = text_invocation(a, " Note ").unwrap();
        assert_eq!(text.id, "drafting.create_text");
        assert_eq!(text.parameters["content"], "Note");
    }
}
