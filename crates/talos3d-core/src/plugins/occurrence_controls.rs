//! Property-inspector presentation for the registered occurrence edit request.
//! The UI captures intent only; the same planner used by MCP owns validation,
//! dependent snapshots, freshness and command history.
use crate::plugins::{
    authored_edit_plan::{
        queue_captured_plan,
        requests::{self, EditRequestRegistry},
        PlanId,
    },
    design_explanation::DesignExplanation,
    history::{History, ModelRevision},
    property_edit::PropertyPanelData,
};
use bevy::prelude::*;
use bevy_egui::egui;
use serde_json::json;

#[derive(Clone, Default)]
pub struct OccurrenceControlEditor {
    pub available: bool,
    pub session: Option<ControlSession>,
    pub(crate) action: Option<Action>,
}

#[derive(Clone)]
pub struct ControlSession {
    pub element_id: u64,
    pub parameter: String,
    pub buffer: String,
    pub original_value: serde_json::Value,
    pub base_revision: ModelRevision,
    pub plan: Option<PlanId>,
    pub feedback: String,
}

#[derive(Clone, Copy)]
pub(crate) enum Action {
    Preview,
    Apply,
}

pub fn draw(
    ui: &mut egui::Ui,
    explanation: &DesignExplanation,
    editor: &mut OccurrenceControlEditor,
) {
    if !editor.available {
        return;
    }
    let Some(revision) = &explanation.model_revision else {
        return;
    };
    egui::CollapsingHeader::new("Edit occurrence controls").show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("occurrence_control_list")
            .max_height(210.0)
            .show(ui, |ui| {
                for row in explanation
                    .sections
                    .iter()
                    .flat_map(|s| &s.rows)
                    .filter(|r| {
                        r.details["authority"] == "definition_parameter"
                            && r.details["editable"] == true
                    })
                {
                    let Some(parameter) = row.details["parameter"].as_str() else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        ui.label(format!("{}: {}", row.label, row.text));
                        if ui.small_button("Edit").clicked() {
                            editor.session = Some(ControlSession {
                                element_id: explanation.element_id,
                                parameter: parameter.into(),
                                buffer: row.details["value"]
                                    .as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| row.details["value"].to_string()),
                                original_value: row.details["value"].clone(),
                                base_revision: revision.clone(),
                                plan: None,
                                feedback: String::new(),
                            });
                        }
                    });
                }
            });
        if let Some(session) = editor.session.as_mut() {
            ui.separator();
            ui.label(session.parameter.replace('_', " "));
            if ui.text_edit_singleline(&mut session.buffer).changed() {
                session.plan = None;
                session.feedback.clear();
            }
            ui.weak("Enter the value in its displayed unit.");
            ui.horizontal(|ui| {
                if ui.button("Preview change").clicked() {
                    editor.action = Some(Action::Preview);
                }
                if ui
                    .add_enabled(session.plan.is_some(), egui::Button::new("Apply change"))
                    .clicked()
                {
                    editor.action = Some(Action::Apply);
                }
            });
            if !session.feedback.is_empty() {
                ui.label(&session.feedback);
            }
        }
    });
}

pub(crate) fn process(world: &mut World, selected_id: Option<u64>) {
    let mut editor = std::mem::take(&mut world.resource_mut::<PropertyPanelData>().control_editor);
    editor.available = world
        .get_resource::<EditRequestRegistry>()
        .is_some_and(|r| r.contains("core.occurrence_parameters"));
    if editor
        .session
        .as_ref()
        .is_some_and(|s| Some(s.element_id) != selected_id)
    {
        editor.session = None;
        editor.action = None;
    }
    if let (Some(action), Some(session)) = (editor.action.take(), editor.session.as_mut()) {
        let result = match action {
            Action::Preview => {
                session.plan = None;
                if world
                    .get_resource::<History>()
                    .map(History::revision_token)
                    .as_ref()
                    != Some(&session.base_revision)
                {
                    Err("The design changed while this value was being edited. Select Edit again to use the current value.".into())
                } else {
                    (if session.original_value.is_string() {
                        Ok(serde_json::Value::String(session.buffer.clone()))
                    } else {
                        serde_json::from_str::<serde_json::Value>(&session.buffer).map_err(|e|format!("Invalid value: {e}"))
                    })
                        .and_then(|value| requests::preview(world,"core.occurrence_parameters",json!({"element_id":session.element_id,"overrides":{(session.parameter.clone()):value}})))
                        .and_then(|plan| {
                            if !plan.can_commit() { return Err("The proposed change has unresolved validation refusals.".into()); }
                            session.plan=Some(plan.plan_id().clone());
                            Ok(format!("Validated proposed value: {}. The design has not changed. Apply to commit.",session.buffer))
                        })
                }
            }
            Action::Apply => session
                .plan
                .take()
                .ok_or_else(|| "Preview the change before applying.".to_string())
                .and_then(|id| queue_captured_plan(world, &id).map_err(|e| e.to_string()))
                .map(|_| {
                    "Change queued. The control value updates when the command commits.".to_string()
                }),
        };
        session.feedback = result.unwrap_or_else(|error| error);
    }
    world.resource_mut::<PropertyPanelData>().control_editor = editor;
}
