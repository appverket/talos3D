use bevy::prelude::*;
use bevy_egui::egui;

use crate::plugins::{cursor::CursorWorldPos, document_properties::DocumentProperties};

// ---------------------------------------------------------------------------
// Tool-window geometry helpers
// These are shared by all floating panel windows (Definition browser, Materials
// browser, etc.).  They ensure panels stay within the visible viewport area.
// ---------------------------------------------------------------------------

const TOOL_WINDOW_MARGIN: f32 = 20.0;

/// Returns the usable viewport rect, shrunk by the standard margin.
pub(crate) fn tool_window_bounds(ctx: &egui::Context) -> egui::Rect {
    ctx.content_rect().shrink(TOOL_WINDOW_MARGIN)
}

/// Clamps `preferred` to fit inside the viewport bounds.
pub(crate) fn tool_window_max_size(ctx: &egui::Context, preferred: egui::Vec2) -> egui::Vec2 {
    let bounds = tool_window_bounds(ctx);
    egui::vec2(
        preferred.x.min(bounds.width().max(240.0)),
        preferred.y.min(bounds.height().max(200.0)),
    )
}

/// Returns a default `Rect` for a floating tool window, clamped to the viewport.
pub(crate) fn tool_window_rect(
    ctx: &egui::Context,
    default_pos: egui::Pos2,
    preferred_size: egui::Vec2,
) -> egui::Rect {
    let bounds = tool_window_bounds(ctx);
    let size = tool_window_max_size(ctx, preferred_size);
    let max_x = (bounds.max.x - size.x).max(bounds.min.x);
    let max_y = (bounds.max.y - size.y).max(bounds.min.y);
    let pos = egui::pos2(
        default_pos.x.clamp(bounds.min.x, max_x),
        default_pos.y.clamp(bounds.min.y, max_y),
    );
    egui::Rect::from_min_size(pos, size)
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StatusBarData>()
            .add_systems(Update, advance_status_feedback);
    }
}

#[derive(Resource, Debug)]
pub struct StatusBarData {
    pub tool_name: String,
    pub hint: String,
    pub selection_summary: String,
    pub property_text: Option<String>,
    pub command_hint: Option<String>,
    feedback: Option<StatusFeedback>,
}

impl Default for StatusBarData {
    fn default() -> Self {
        Self {
            tool_name: "Select".to_string(),
            hint: String::new(),
            selection_summary: String::new(),
            property_text: None,
            command_hint: None,
            feedback: None,
        }
    }
}

impl StatusBarData {
    pub fn set_feedback(&mut self, message: String, duration_seconds: f32) {
        self.feedback = Some(StatusFeedback {
            message,
            timer: Timer::from_seconds(duration_seconds, TimerMode::Once),
        });
    }
}

#[derive(Debug)]
struct StatusFeedback {
    message: String,
    timer: Timer,
}

pub(crate) fn coordinate_text(
    cursor_world_pos: &CursorWorldPos,
    doc_props: &DocumentProperties,
) -> String {
    match cursor_world_pos.snapped {
        Some(position) => {
            let x = doc_props.format_coordinate("X", position.x);
            let z = doc_props.format_coordinate("Z", position.z);
            format!("{x}  {z}")
        }
        None => "\u{2014}".to_string(),
    }
}

pub(crate) fn hint_text(status_bar_data: &StatusBarData) -> String {
    status_bar_data
        .feedback
        .as_ref()
        .map(|feedback| feedback.message.clone())
        .or_else(|| status_bar_data.property_text.clone())
        .or_else(|| status_bar_data.command_hint.clone())
        .or_else(|| {
            if status_bar_data.hint.is_empty() {
                None
            } else {
                Some(status_bar_data.hint.clone())
            }
        })
        .unwrap_or_else(|| status_bar_data.selection_summary.clone())
}

pub(crate) fn advance_status_feedback(time: Res<Time>, mut status_bar_data: ResMut<StatusBarData>) {
    let Some(feedback) = status_bar_data.feedback.as_mut() else {
        return;
    };

    feedback.timer.tick(time.delta());
    if feedback.timer.is_finished() {
        status_bar_data.feedback = None;
    }
}
