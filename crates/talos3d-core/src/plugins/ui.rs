use bevy::prelude::*;

use crate::plugins::{cursor::CursorWorldPos, document_properties::DocumentProperties};

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
