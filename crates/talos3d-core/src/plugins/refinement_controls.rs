//! Human refinement controls: non-mutating display lenses and explicit,
//! previewed Develop / Simplify actions.
//!
//! This plugin is product-composed rather than installed by core by default.
//! It reuses the exact revision-fenced goal plans exposed over MCP; the GUI is
//! presentation over that plan, not a second refinement implementation.

use std::collections::BTreeMap;

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    capability_registry::{CapabilityRegistry, ElementClassAssignment},
    plugins::{
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        egui_chrome::EguiChromeSystems,
        identity::ElementId,
        model_api::{
            flush_model_api_write_pipeline, handle_apply_refinement_goal,
            handle_create_refinement_goal, CreateRefinementGoalRequest, RefinementGoalInfo,
            RefinementGoalTargetRequest,
        },
        refinement::{
            apply_refinement_demotion_plan, build_refinement_demotion_plan,
            is_parked_refinement_entity, RefinementBranch, RefinementBranchStatus,
            RefinementDemotionPlan, RefinementGoalScope, RefinementGoalScopeKind, RefinementState,
            RefinementStateComponent,
        },
        selection::Selected,
    },
};

pub struct RefinementControlsPlugin;

impl Plugin for RefinementControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RefinementLens>()
            .init_resource::<RefinementLensRuntime>()
            .init_resource::<RefinementControlPanel>()
            .init_resource::<PendingRefinementControlActions>()
            .register_toggle_command(
                lens_command(
                    "refinement.view_massing",
                    "View as Massing",
                    RefinementLensBand::Massing,
                ),
                set_massing_lens,
                massing_lens_active,
            )
            .register_toggle_command(
                lens_command(
                    "refinement.view_design",
                    "View as Design",
                    RefinementLensBand::Design,
                ),
                set_design_lens,
                design_lens_active,
            )
            .register_toggle_command(
                lens_command(
                    "refinement.view_build",
                    "View as Build",
                    RefinementLensBand::Build,
                ),
                set_build_lens,
                build_lens_active,
            )
            .register_command(
                action_command(
                    "refinement.develop_selected",
                    "Develop selected…",
                    "Preview a scoped refinement goal for the selected architectural artifacts.",
                    true,
                ),
                open_develop_selected,
            )
            .register_command(
                action_command(
                    "refinement.develop_model",
                    "Develop model…",
                    "Open an editable default-all plan for refinable architectural roots.",
                    false,
                ),
                open_develop_model,
            )
            .register_command(
                action_command(
                    "refinement.demote_selected",
                    "Simplify / Demote selected…",
                    "Preview lowering active state and parking higher-detail branches.",
                    true,
                ),
                open_demote_selected,
            )
            .register_viewport_context_command("refinement.develop_selected")
            .register_viewport_context_command("refinement.demote_selected")
            .add_systems(
                Update,
                (
                    process_refinement_control_actions,
                    reconcile_refinement_lens_visibility,
                    draw_refinement_control_panel.after(EguiChromeSystems),
                ),
            );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefinementLensBand {
    Massing,
    Design,
    Build,
}

impl RefinementLensBand {
    fn label(self) -> &'static str {
        match self {
            Self::Massing => "Massing",
            Self::Design => "Design",
            Self::Build => "Build",
        }
    }

    fn shows(self, state: RefinementState) -> bool {
        match self {
            Self::Massing => state <= RefinementState::Conceptual,
            Self::Design => state <= RefinementState::Schematic,
            // Build is the human-facing doorway to Constructible and also the
            // way back to requested Detailed/FabricationReady content.
            Self::Build => true,
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct RefinementLens {
    pub band: RefinementLensBand,
    revision: u64,
}

impl Default for RefinementLens {
    fn default() -> Self {
        Self {
            band: RefinementLensBand::Build,
            revision: 0,
        }
    }
}

#[derive(Resource, Debug, Default)]
struct RefinementLensRuntime {
    applied_revision: Option<u64>,
}

/// Presentation-only override. It is not an authored entity and is never
/// persisted, validated, scheduled, exported, or entered into history.
#[derive(Component, Debug, Clone, Copy)]
struct RefinementLensHidden {
    previous_visibility: Visibility,
}

fn lens_command(id: &str, label: &str, band: RefinementLensBand) -> CommandDescriptor {
    CommandDescriptor {
        id: id.into(),
        label: label.into(),
        description: format!(
            "Switch to the non-mutating {} presentation lens; authored refinement truth is unchanged.",
            band.label()
        ),
        category: CommandCategory::View,
        parameters: None,
        version: 1,
        default_shortcut: None,
        icon: None,
        hint: Some("Display only — does not promote, demote, or park content".into()),
        requires_selection: false,
        show_in_menu: true,
        activates_tool: None,
        capability_id: None,
    }
}

fn action_command(id: &str, label: &str, description: &str, selection: bool) -> CommandDescriptor {
    CommandDescriptor {
        id: id.into(),
        label: label.into(),
        description: description.into(),
        category: CommandCategory::Edit,
        parameters: None,
        version: 1,
        default_shortcut: None,
        icon: None,
        hint: Some("Opens a revision-fenced preview; no model change occurs until Apply".into()),
        requires_selection: selection,
        show_in_menu: true,
        activates_tool: None,
        capability_id: None,
    }
}

fn set_lens(world: &mut World, band: RefinementLensBand) -> Result<CommandResult, String> {
    let mut lens = world.resource_mut::<RefinementLens>();
    lens.band = band;
    lens.revision = lens.revision.wrapping_add(1);
    Ok(CommandResult {
        output: Some(json!({
            "lens": band,
            "model_mutated": false,
            "active_refinement_state_unchanged": true,
        })),
        ..default()
    })
}

fn set_massing_lens(world: &mut World, _: &serde_json::Value) -> Result<CommandResult, String> {
    set_lens(world, RefinementLensBand::Massing)
}

fn set_design_lens(world: &mut World, _: &serde_json::Value) -> Result<CommandResult, String> {
    set_lens(world, RefinementLensBand::Design)
}

fn set_build_lens(world: &mut World, _: &serde_json::Value) -> Result<CommandResult, String> {
    set_lens(world, RefinementLensBand::Build)
}

fn massing_lens_active(world: &World) -> bool {
    world.resource::<RefinementLens>().band == RefinementLensBand::Massing
}

fn design_lens_active(world: &World) -> bool {
    world.resource::<RefinementLens>().band == RefinementLensBand::Design
}

fn build_lens_active(world: &World) -> bool {
    world.resource::<RefinementLens>().band == RefinementLensBand::Build
}

/// Change-driven reconciliation. A full authored-world scan happens only when
/// the user changes lens or refinement branches change, never every frame.
fn reconcile_refinement_lens_visibility(world: &mut World) {
    let lens_revision = world.resource::<RefinementLens>().revision;
    let branch_changed = world
        .try_query_filtered::<Entity, Changed<RefinementBranch>>()
        .is_some_and(|mut query| query.iter(world).next().is_some());
    let state_changed = world
        .try_query_filtered::<Entity, Changed<RefinementStateComponent>>()
        .is_some_and(|mut query| query.iter(world).next().is_some());
    let already_applied = world.resource::<RefinementLensRuntime>().applied_revision;
    if already_applied == Some(lens_revision) && !branch_changed && !state_changed {
        return;
    }

    let band = world.resource::<RefinementLens>().band;
    let mut branch_children = BTreeMap::<u64, RefinementBranchStatus>::new();
    if let Some(mut query) = world.try_query::<&RefinementBranch>() {
        for branch in query.iter(world) {
            branch_children.insert(branch.child_element_id, branch.status);
        }
    }

    let mut candidates = Vec::new();
    if let Some(mut query) = world.try_query::<(
        Entity,
        &ElementId,
        &RefinementStateComponent,
        Option<&Visibility>,
    )>() {
        for (entity, id, state, visibility) in query.iter(world) {
            candidates.push((
                entity,
                id.0,
                state.state,
                visibility.copied().unwrap_or(Visibility::Inherited),
            ));
        }
    }

    for (entity, element_id, state, visibility) in candidates {
        let hidden = world.get::<RefinementLensHidden>(entity).copied();
        let branch_status = branch_children.get(&element_id).copied();
        let parked = branch_status == Some(RefinementBranchStatus::Parked)
            || is_parked_refinement_entity(world, ElementId(element_id));
        let should_hide =
            branch_status == Some(RefinementBranchStatus::Active) && !parked && !band.shows(state);
        match (should_hide, hidden) {
            (true, None) => {
                world.entity_mut(entity).insert((
                    Visibility::Hidden,
                    RefinementLensHidden {
                        previous_visibility: visibility,
                    },
                ));
            }
            (true, Some(_)) => {
                world.entity_mut(entity).insert(Visibility::Hidden);
            }
            (false, Some(_previous)) if parked => {
                // Parking owns Hidden now. Drop only our marker.
                world.entity_mut(entity).remove::<RefinementLensHidden>();
            }
            (false, Some(previous)) => {
                world
                    .entity_mut(entity)
                    .insert(previous.previous_visibility)
                    .remove::<RefinementLensHidden>();
            }
            (false, None) => {}
        }
    }
    world
        .resource_mut::<RefinementLensRuntime>()
        .applied_revision = Some(lens_revision);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefinementPanelMode {
    Develop,
    Demote,
}

#[derive(Debug, Clone)]
struct RefinementPanelCandidate {
    element_id: u64,
    element_class: String,
    current_state: RefinementState,
    included: bool,
}

#[derive(Resource, Debug)]
struct RefinementControlPanel {
    open: bool,
    mode: RefinementPanelMode,
    candidates: Vec<RefinementPanelCandidate>,
    target_state: RefinementState,
    goal_preview: Option<RefinementGoalInfo>,
    demotion_preview: Option<RefinementDemotionPlan>,
    message: Option<String>,
}

impl Default for RefinementControlPanel {
    fn default() -> Self {
        Self {
            open: false,
            mode: RefinementPanelMode::Develop,
            candidates: Vec::new(),
            target_state: RefinementState::Schematic,
            goal_preview: None,
            demotion_preview: None,
            message: None,
        }
    }
}

#[derive(Debug, Clone)]
enum RefinementControlAction {
    PreviewDevelop {
        element_ids: Vec<u64>,
        target_state: RefinementState,
    },
    ApplyDevelop {
        goal_id: String,
    },
    PreviewDemotion {
        element_ids: Vec<u64>,
        target_state: RefinementState,
    },
    ApplyDemotion {
        plan: RefinementDemotionPlan,
    },
}

#[derive(Resource, Debug, Default)]
struct PendingRefinementControlActions(Vec<RefinementControlAction>);

fn refinable_artifact_candidates(
    world: &World,
    selected_only: bool,
) -> Vec<RefinementPanelCandidate> {
    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    if let Some(mut query) = world.try_query::<(
        Entity,
        &ElementId,
        &ElementClassAssignment,
        &RefinementStateComponent,
        Option<&Selected>,
    )>() {
        for (_entity, id, assignment, state, selected) in query.iter(world) {
            if selected_only && selected.is_none() {
                continue;
            }
            if is_parked_refinement_entity(world, *id) {
                continue;
            }
            let Some(class) = registry.element_class_descriptor(&assignment.element_class) else {
                continue;
            };
            let has_ladder = class
                .class_min_obligations
                .values()
                .any(|obligations| !obligations.is_empty())
                && class
                    .class_min_promotion_critical_paths
                    .values()
                    .any(|paths| !paths.is_empty());
            if !has_ladder {
                continue;
            }
            if !selected_only
                && crate::plugins::refinement::query_refinement_of(world, *id).is_some()
            {
                continue;
            }
            candidates.push(RefinementPanelCandidate {
                element_id: id.0,
                element_class: assignment.element_class.0.clone(),
                current_state: state.state,
                included: true,
            });
        }
    }
    candidates.sort_by_key(|candidate| candidate.element_id);
    candidates
}

fn open_panel(
    world: &mut World,
    mode: RefinementPanelMode,
    selected_only: bool,
) -> Result<CommandResult, String> {
    let candidates = refinable_artifact_candidates(world, selected_only);
    if candidates.is_empty() {
        return Err(if selected_only {
            "Selection contains no active architectural artifact with an enforceable refinement ladder"
                .into()
        } else {
            "Model contains no refinable architectural roots".into()
        });
    }
    let default_target = match mode {
        RefinementPanelMode::Develop => {
            if candidates
                .iter()
                .any(|candidate| candidate.current_state < RefinementState::Schematic)
            {
                RefinementState::Schematic
            } else {
                RefinementState::Constructible
            }
        }
        RefinementPanelMode::Demote => RefinementState::Conceptual,
    };
    *world.resource_mut::<RefinementControlPanel>() = RefinementControlPanel {
        open: true,
        mode,
        candidates,
        target_state: default_target,
        goal_preview: None,
        demotion_preview: None,
        message: None,
    };
    Ok(CommandResult {
        output: Some(json!({"preview_opened": true, "model_mutated": false})),
        ..default()
    })
}

fn open_develop_selected(
    world: &mut World,
    _: &serde_json::Value,
) -> Result<CommandResult, String> {
    open_panel(world, RefinementPanelMode::Develop, true)
}

fn open_develop_model(world: &mut World, _: &serde_json::Value) -> Result<CommandResult, String> {
    open_panel(world, RefinementPanelMode::Develop, false)
}

fn open_demote_selected(world: &mut World, _: &serde_json::Value) -> Result<CommandResult, String> {
    open_panel(world, RefinementPanelMode::Demote, true)
}

fn process_refinement_control_actions(world: &mut World) {
    let actions = std::mem::take(&mut world.resource_mut::<PendingRefinementControlActions>().0);
    for action in actions {
        let result = match action {
            RefinementControlAction::PreviewDevelop {
                element_ids,
                target_state,
            } => {
                let targets = element_ids
                    .into_iter()
                    .map(|element_id| RefinementGoalTargetRequest {
                        scope: RefinementGoalScope {
                            kind: RefinementGoalScopeKind::RefinementSubtree,
                            root_element_id: element_id,
                        },
                        element_class: None,
                        target_state: target_state.as_str().into(),
                        recipe_id: None,
                        overrides: serde_json::Value::Null,
                    })
                    .collect();
                handle_create_refinement_goal(
                    world,
                    CreateRefinementGoalRequest {
                        requested_outcomes: vec![format!(
                            "develop selected architectural scope to {}",
                            target_state.as_str()
                        )],
                        targets,
                        inference_evidence: Vec::new(),
                        inference_confidence: 1.0,
                        inference_alternatives: Vec::new(),
                        assumption_refs: Vec::new(),
                    },
                )
                .map(|preview| {
                    world.resource_mut::<RefinementControlPanel>().goal_preview = Some(preview);
                    "Promotion preview captured from the live capability snapshot".to_string()
                })
            }
            RefinementControlAction::ApplyDevelop { goal_id } => {
                handle_apply_refinement_goal(world, &goal_id).map(|result| {
                    format!(
                        "Develop result: {} ({} applied, {} blocked)",
                        result.status,
                        result.applied_targets.len(),
                        result.blocked_targets.len()
                    )
                })
            }
            RefinementControlAction::PreviewDemotion {
                element_ids,
                target_state,
            } => build_refinement_demotion_plan(world, element_ids, target_state).map(|preview| {
                world
                    .resource_mut::<RefinementControlPanel>()
                    .demotion_preview = Some(preview);
                "Demotion preview captured; higher detail will be parked, not deleted".to_string()
            }),
            RefinementControlAction::ApplyDemotion { plan } => {
                apply_refinement_demotion_plan(world, &plan).map(|applied| {
                    flush_model_api_write_pipeline(world);
                    format!(
                        "Demoted {} target(s); higher branches are parked",
                        applied.len()
                    )
                })
            }
        };
        let panel = &mut *world.resource_mut::<RefinementControlPanel>();
        panel.message = Some(result.unwrap_or_else(|error| format!("Refused: {error}")));
    }
}

fn draw_refinement_control_panel(
    mut contexts: EguiContexts,
    mut panel: ResMut<RefinementControlPanel>,
    mut pending: ResMut<PendingRefinementControlActions>,
) {
    if !panel.open {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = panel.open;
    egui::Window::new(match panel.mode {
        RefinementPanelMode::Develop => "Develop architectural artifacts",
        RefinementPanelMode::Demote => "Simplify / Demote architectural artifacts",
    })
    .open(&mut open)
    .default_width(520.0)
    .show(ctx, |ui| {
        ui.label(match panel.mode {
            RefinementPanelMode::Develop => {
                "Choose an explicit scope and target. Preview and Apply use the same revision-fenced capability plans."
            }
            RefinementPanelMode::Demote => {
                "Demotion lowers active truth and parks higher detail. Changing the lens back upward will not reactivate it."
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Target:");
            match panel.mode {
                RefinementPanelMode::Develop => {
                    ui.selectable_value(&mut panel.target_state, RefinementState::Schematic, "Design");
                    ui.selectable_value(
                        &mut panel.target_state,
                        RefinementState::Constructible,
                        "Build",
                    );
                }
                RefinementPanelMode::Demote => {
                    ui.selectable_value(
                        &mut panel.target_state,
                        RefinementState::Conceptual,
                        "Massing",
                    );
                    ui.selectable_value(&mut panel.target_state, RefinementState::Schematic, "Design");
                }
            }
        });
        ui.label("Scope (editable; model scope starts with all eligible roots selected):");
        let mode = panel.mode;
        let target_state = panel.target_state;
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            for candidate in &mut panel.candidates {
                let eligible = match mode {
                    RefinementPanelMode::Develop => candidate.current_state < target_state,
                    RefinementPanelMode::Demote => candidate.current_state > target_state,
                };
                ui.add_enabled_ui(eligible, |ui| {
                    ui.checkbox(
                        &mut candidate.included,
                        format!(
                            "{} — {} ({})",
                            candidate.element_id,
                            candidate.element_class,
                            candidate.current_state.as_str()
                        ),
                    );
                });
                if !eligible {
                    candidate.included = false;
                }
            }
        });
        let selected = panel
            .candidates
            .iter()
            .filter(|candidate| candidate.included)
            .map(|candidate| candidate.element_id)
            .collect::<Vec<_>>();
        if ui
            .add_enabled(!selected.is_empty(), egui::Button::new("Preview plan"))
            .clicked()
        {
            panel.goal_preview = None;
            panel.demotion_preview = None;
            let action = match panel.mode {
                RefinementPanelMode::Develop => RefinementControlAction::PreviewDevelop {
                    element_ids: selected,
                    target_state: panel.target_state,
                },
                RefinementPanelMode::Demote => RefinementControlAction::PreviewDemotion {
                    element_ids: selected,
                    target_state: panel.target_state,
                },
            };
            pending.0.push(action);
        }

        if let Some(preview) = &panel.goal_preview {
            ui.separator();
            ui.label(&preview.readback);
            let all_reachable = preview.target_plans.iter().all(|plan| plan.can_commit)
                && !preview.target_plans.is_empty();
            for plan in &preview.target_plans {
                if plan.can_commit {
                    ui.label(format!(
                        "✓ {} → {} is reachable",
                        plan.target.root_element_id, plan.target_state
                    ));
                } else {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!(
                            "{} → {} unavailable for this scope",
                            plan.target.root_element_id, plan.target_state
                        ),
                    );
                    for limitation in &plan.missing_inputs {
                        ui.label(format!("  • {limitation}"));
                    }
                    ui.label("Remedy: keep the coarse handle and Develop this scope after its curated path/corpus gap is resolved.");
                }
            }
            if ui
                .add_enabled(all_reachable, egui::Button::new("Apply Develop"))
                .clicked()
            {
                pending.0.push(RefinementControlAction::ApplyDevelop {
                    goal_id: preview.goal.goal_id.0.clone(),
                });
            }
        }
        if let Some(preview) = &panel.demotion_preview {
            ui.separator();
            for target in &preview.targets {
                ui.label(format!(
                    "{}: {} → {}; park {:?}",
                    target.element_id,
                    target.current_state.as_str(),
                    preview.target_state.as_str(),
                    target.branch_element_ids_to_park
                ));
            }
            if ui.button("Apply Demotion").clicked() {
                pending
                    .0
                    .push(RefinementControlAction::ApplyDemotion { plan: preview.clone() });
            }
        }
        if let Some(message) = &panel.message {
            ui.separator();
            ui.label(message);
        }
    });
    panel.open = open;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        history::{apply_pending_history_commands_for_test, History, PendingCommandQueue},
        identity::ElementIdAllocator,
        refinement::{
            create_refinement_relation_pair, query_parked_refined_into, query_refined_into,
            DemoteRefinementRequest,
        },
    };

    fn test_world() -> World {
        let mut world = World::new();
        world.init_resource::<ElementIdAllocator>();
        world.resource_mut::<ElementIdAllocator>().set_next(10_000);
        world.init_resource::<Messages<BeginCommandGroup>>();
        world.init_resource::<Messages<ApplyEntityChangesCommand>>();
        world.init_resource::<Messages<EndCommandGroup>>();
        world.init_resource::<Messages<CreateEntityCommand>>();
        world.init_resource::<Messages<CreateBoxCommand>>();
        world.init_resource::<Messages<CreateCylinderCommand>>();
        world.init_resource::<Messages<CreateSphereCommand>>();
        world.init_resource::<Messages<CreatePlaneCommand>>();
        world.init_resource::<Messages<CreatePolylineCommand>>();
        world.init_resource::<Messages<CreateTriangleMeshCommand>>();
        world.init_resource::<Messages<DeleteEntitiesCommand>>();
        world.init_resource::<Messages<ResolvedDeleteEntitiesCommand>>();
        world.init_resource::<PendingCommandQueue>();
        world.init_resource::<History>();
        world.init_resource::<CapabilityRegistry>();
        world.init_resource::<RefinementLens>();
        world.init_resource::<RefinementLensRuntime>();
        world
    }

    #[test]
    fn lens_round_trip_changes_only_presentation_and_not_model_revision() {
        let mut world = test_world();
        world.spawn((
            ElementId(1),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
            Visibility::Visible,
        ));
        world.spawn((
            ElementId(2),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
            Visibility::Visible,
        ));
        create_refinement_relation_pair(
            &mut world,
            ElementId(1),
            ElementId(2),
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );
        let revision = world.resource::<History>().model_revision();

        set_lens(&mut world, RefinementLensBand::Massing).unwrap();
        reconcile_refinement_lens_visibility(&mut world);
        let child =
            crate::plugins::commands::find_entity_by_element_id_readonly(&world, ElementId(2))
                .unwrap();
        assert_eq!(world.get::<Visibility>(child), Some(&Visibility::Hidden));
        assert_eq!(
            world
                .get::<RefinementStateComponent>(child)
                .map(|state| state.state),
            Some(RefinementState::Constructible)
        );
        assert_eq!(world.resource::<History>().model_revision(), revision);

        set_lens(&mut world, RefinementLensBand::Build).unwrap();
        reconcile_refinement_lens_visibility(&mut world);
        assert_eq!(world.get::<Visibility>(child), Some(&Visibility::Visible));
        assert!(world.get::<RefinementLensHidden>(child).is_none());
        assert_eq!(world.resource::<History>().model_revision(), revision);
        assert_eq!(query_refined_into(&world, ElementId(1)), vec![ElementId(2)]);
    }

    #[test]
    fn demotion_plan_parks_detail_and_undo_restores_active_branch() {
        let mut world = test_world();
        world.spawn((
            ElementId(1),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
        ));
        world.spawn((
            ElementId(2),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
        ));
        create_refinement_relation_pair(
            &mut world,
            ElementId(1),
            ElementId(2),
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );
        let plan =
            build_refinement_demotion_plan(&world, vec![1], RefinementState::Conceptual).unwrap();
        assert_eq!(plan.targets[0].branch_element_ids_to_park, vec![2]);
        apply_refinement_demotion_plan(&mut world, &plan).unwrap();
        crate::plugins::commands::queue_command_events(&mut world);
        apply_pending_history_commands_for_test(&mut world);
        assert_eq!(
            query_parked_refined_into(&world, ElementId(1)),
            vec![ElementId(2)]
        );

        world.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut world);
        assert_eq!(query_refined_into(&world, ElementId(1)), vec![ElementId(2)]);
        assert!(query_parked_refined_into(&world, ElementId(1)).is_empty());
    }

    #[test]
    fn stale_demotion_plan_refuses_without_partial_mutation() {
        let mut world = test_world();
        world.spawn((
            ElementId(1),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
        ));
        let plan =
            build_refinement_demotion_plan(&world, vec![1], RefinementState::Conceptual).unwrap();
        world.resource_mut::<History>().clear();
        // A second, accepted command would advance the revision in production;
        // exercise the stronger captured-state fence directly here.
        let entity =
            crate::plugins::commands::find_entity_by_element_id_readonly(&world, ElementId(1))
                .unwrap();
        world.entity_mut(entity).insert(RefinementStateComponent {
            state: RefinementState::Detailed,
        });
        assert!(apply_refinement_demotion_plan(&mut world, &plan).is_err());
        assert_eq!(
            world
                .get::<RefinementStateComponent>(entity)
                .map(|state| state.state),
            Some(RefinementState::Detailed)
        );
    }

    #[test]
    fn direct_demote_request_still_uses_the_shared_parking_semantics() {
        let mut world = test_world();
        world.spawn((
            ElementId(1),
            RefinementStateComponent {
                state: RefinementState::Schematic,
            },
        ));
        crate::plugins::refinement::apply_demote_refinement(
            &mut world,
            DemoteRefinementRequest {
                entity_element_id: 1,
                target_state: RefinementState::Conceptual,
            },
        )
        .unwrap();
    }
}
