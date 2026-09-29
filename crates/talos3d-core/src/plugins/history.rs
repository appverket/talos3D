use std::mem;

use bevy::prelude::*;

use crate::plugins::ui::StatusBarData;
use crate::semantics::{
    components::{SemanticGraph, WorldSemanticContext},
    evaluate, Refusal, SemanticPlan, Verdict,
};

const STATUS_MESSAGE_DURATION_SECONDS: f32 = 2.0;

pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(Update, (HistorySet::Queue, HistorySet::Apply).chain())
            .init_resource::<History>()
            .init_resource::<PendingCommandQueue>()
            .init_resource::<SemanticEnforcement>()
            .add_systems(
                Update,
                apply_pending_history_commands.in_set(HistorySet::Apply),
            );
    }
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistorySet {
    Queue,
    Apply,
}

pub trait EditorCommand: Send + Sync + 'static {
    fn label(&self) -> &'static str;
    fn apply(&mut self, world: &mut World);
    fn undo(&mut self, world: &mut World);

    fn redo(&mut self, world: &mut World) {
        self.apply(world);
    }

    /// This command's semantic intent, evaluated by the admissibility kernel
    /// before [`apply`](Self::apply) runs (ADR-064 §3.1).
    ///
    /// The default declares nothing, so the kernel stays disarmed and
    /// geometry-only commands remain geometry-only. Override this in any
    /// command that assigns, changes, or removes a concept, or that binds a
    /// concept-bearing entity through a registered predicate.
    ///
    /// Because this sits at `PendingCommandQueue` — the one point every
    /// interactive, command, recipe, import, and MCP mutation converges on —
    /// a plan declared here is enforced identically on all of them.
    fn semantic_plan(&self, _world: &World) -> SemanticPlan {
        SemanticPlan::none()
    }
}

struct GroupedCommand {
    label: &'static str,
    commands: Vec<Box<dyn EditorCommand>>,
}

impl EditorCommand for GroupedCommand {
    fn label(&self) -> &'static str {
        self.label
    }

    /// A group is evaluated as one plan, so a refusal anywhere in the group
    /// refuses the whole group and atomicity is preserved (ADR-064 §3.2).
    fn semantic_plan(&self, world: &World) -> SemanticPlan {
        self.commands
            .iter()
            .fold(SemanticPlan::none(), |plan, command| {
                plan.merge(command.semantic_plan(world))
            })
    }

    fn apply(&mut self, world: &mut World) {
        for command in &mut self.commands {
            command.apply(world);
        }
    }

    fn undo(&mut self, world: &mut World) {
        for command in self.commands.iter_mut().rev() {
            command.undo(world);
        }
    }

    fn redo(&mut self, world: &mut World) {
        for command in &mut self.commands {
            command.redo(world);
        }
    }
}

#[derive(Default)]
struct CommandGroupBuilder {
    label: &'static str,
    commands: Vec<Box<dyn EditorCommand>>,
}

/// Transient identity of an authored document and its monotonic edit revision.
/// Replacing a document invalidates proposals even when revision counters match.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ModelRevision {
    pub document_id: String,
    pub revision: u64,
}

#[derive(Resource)]
pub struct History {
    undo_stack: Vec<Box<dyn EditorCommand>>,
    redo_stack: Vec<Box<dyn EditorCommand>>,
    /// The undo stack depth at the last save. `None` means never saved in this session.
    save_point: Option<usize>,
    /// Monotonic revision fence for accepted model mutations. Unlike stack
    /// depth this also advances on undo and redo, so an old preview can never
    /// become accidentally current again.
    model_revision: u64,
    document_id: String,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            save_point: None,
            model_revision: 0,
            document_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl History {
    /// Undo-stack depth. Used by enforcement tests to assert that a refused
    /// command never entered history.
    pub fn undo_stack_len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn model_revision(&self) -> u64 {
        self.model_revision
    }

    pub fn revision_token(&self) -> ModelRevision {
        ModelRevision {
            document_id: self.document_id.clone(),
            revision: self.model_revision,
        }
    }

    pub fn clear(&mut self) {
        self.document_id = uuid::Uuid::new_v4().to_string();
        self.model_revision = self.model_revision.saturating_add(1);
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.save_point = None;
    }

    pub fn mark_save_point(&mut self) {
        self.save_point = Some(self.undo_stack.len());
    }

    pub fn at_save_point(&self) -> bool {
        self.save_point == Some(self.undo_stack.len())
    }
}

/// Roll back commands applied after `target_depth` and discard their redo
/// entries.
///
/// This is the transaction-abort path for compound API operations that must
/// apply intermediate commands in order to calculate later steps. A failed
/// operation must not leave either authored state or a redo-able fragment
/// behind. The model revision still advances for each compensating undo, so a
/// preview created before the failed attempt remains safely stale.
pub(crate) fn rollback_to_undo_depth(world: &mut World, target_depth: usize) -> usize {
    let mut rolled_back = 0;
    while world.resource::<History>().undo_stack_len() > target_depth {
        undo_last_command(world);
        rolled_back += 1;
    }

    let at_save_point = {
        let mut history = world.resource_mut::<History>();
        history.redo_stack.clear();
        history.at_save_point()
    };
    if let Some(mut document_state) =
        world.get_resource_mut::<crate::plugins::document_state::DocumentState>()
    {
        document_state.dirty = !at_save_point;
    }
    rolled_back
}

/// A synchronous command transaction. Intermediate effects are visible to later
/// steps, but prior undo/redo history is kept out of reach until acceptance.
/// Only executors whose effects are entirely represented by EditorCommands may
/// enter. Pending user work is refused before entry, never drained into the edit.
pub(crate) struct HistoryTransaction {
    previous: History,
    document: Option<crate::plugins::document_state::DocumentState>,
    next_element_id: Option<u64>,
}

impl HistoryTransaction {
    pub(crate) fn begin(world: &mut World) -> Result<Self, String> {
        if !world.resource::<PendingCommandQueue>().is_empty() {
            return Err(
                "pending commands or an open command group must finish before commit".into(),
            );
        }
        let previous = world
            .remove_resource::<History>()
            .expect("History installed");
        let working = History {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            save_point: None,
            model_revision: previous.model_revision,
            document_id: previous.document_id.clone(),
        };
        // Preserve a save point on another branch of history on abort; a
        // successful edit invalidates it when that saved future is discarded.
        let document = world
            .get_resource::<crate::plugins::document_state::DocumentState>()
            .cloned();
        let next_element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .map(|a| a.next_value());
        world.insert_resource(working);
        // Retain the original history itself (commands cannot be cloned).
        Ok(Self {
            previous,
            document,
            next_element_id,
        })
    }

    pub(crate) fn finish(mut self, world: &mut World, accept: bool) {
        let mut working = world
            .remove_resource::<History>()
            .expect("transaction history installed");
        self.previous.model_revision = working.model_revision;
        world.insert_resource(History {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            save_point: None,
            model_revision: working.model_revision,
            document_id: self.previous.document_id.clone(),
        });
        let had_effects = !working.undo_stack.is_empty();
        if accept && had_effects {
            if self
                .previous
                .save_point
                .is_some_and(|point| point > self.previous.undo_stack.len())
            {
                self.previous.save_point = None;
            }
            self.previous.undo_stack.push(Box::new(GroupedCommand {
                label: "Procedural session",
                commands: mem::take(&mut working.undo_stack),
            }));
            self.previous.redo_stack.clear();
        } else if !accept {
            for mut command in working.undo_stack.into_iter().rev() {
                command.undo(world);
                self.previous.model_revision = self.previous.model_revision.saturating_add(1);
            }
            world.resource_mut::<PendingCommandQueue>().clear();
            if let (Some(next), Some(mut allocator)) = (
                self.next_element_id,
                world.get_resource_mut::<crate::plugins::identity::ElementIdAllocator>(),
            ) {
                allocator.set_next(next);
            }
            if let Some(document) = self.document {
                world.insert_resource(document);
            }
        }
        let dirty = !self.previous.at_save_point();
        world.insert_resource(self.previous);
        if accept && had_effects {
            if let Some(mut document) =
                world.get_resource_mut::<crate::plugins::document_state::DocumentState>()
            {
                document.dirty = dirty;
            }
        }
    }
}

#[derive(Resource, Default)]
pub struct PendingCommandQueue {
    pub commands: Vec<Box<dyn EditorCommand>>,
    actions: Vec<HistoryAction>,
    open_groups: Vec<CommandGroupBuilder>,
}

impl PendingCommandQueue {
    pub(crate) fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.actions.is_empty() && self.open_groups.is_empty()
    }

    pub fn clear(&mut self) {
        self.commands.clear();
        self.actions.clear();
        self.open_groups.clear();
    }

    pub fn queue_undo(&mut self) {
        self.actions.push(HistoryAction::Undo);
    }

    pub fn queue_redo(&mut self) {
        self.actions.push(HistoryAction::Redo);
    }

    pub fn begin_group(&mut self, label: &'static str) {
        self.open_groups.push(CommandGroupBuilder {
            label,
            commands: Vec::new(),
        });
    }

    pub fn end_group(&mut self) {
        let Some(group) = self.open_groups.pop() else {
            return;
        };

        match group.commands.len() {
            0 => {}
            1 => {
                let mut commands = group.commands;
                if let Some(command) = commands.pop() {
                    self.push_command(command);
                }
            }
            _ => {
                self.push_command(Box::new(GroupedCommand {
                    label: group.label,
                    commands: group.commands,
                }));
            }
        }
    }

    pub fn push_command(&mut self, command: Box<dyn EditorCommand>) {
        if let Some(group) = self.open_groups.last_mut() {
            group.commands.push(command);
        } else {
            self.commands.push(command);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryAction {
    Undo,
    Redo,
}

/// Test-only entry point into the drain, so enforcement tests exercise the
/// real gate rather than a reimplementation of it.
#[cfg(test)]
pub(crate) fn apply_pending_history_commands_for_test(world: &mut World) {
    apply_pending_history_commands(world);
}

pub(crate) fn apply_pending_history_commands(world: &mut World) {
    let (pending_commands, pending_actions) = {
        let mut pending_command_queue = world.resource_mut::<PendingCommandQueue>();
        (
            mem::take(&mut pending_command_queue.commands),
            mem::take(&mut pending_command_queue.actions),
        )
    };

    let had_work = !pending_commands.is_empty() || !pending_actions.is_empty();

    for mut command in pending_commands {
        // ADR-064 §3.2: the kernel evaluates before `apply`. On `Refuse`
        // nothing is applied, nothing enters history, and the world is left
        // untouched — a refusal must not leave partial state behind.
        match evaluate_command_admissibility(world, command.as_ref()) {
            Verdict::Refuse(refusals) => {
                record_refusals(world, command.label(), refusals);
                continue;
            }
            Verdict::AdmitWithObligation(obligations) => {
                world
                    .resource_mut::<SemanticEnforcement>()
                    .obligations
                    .extend(obligations);
            }
            Verdict::Admit => {}
        }

        command.apply(world);

        let mut history = world.resource_mut::<History>();
        history.undo_stack.push(command);
        history.redo_stack.clear();
        history.model_revision = history.model_revision.saturating_add(1);
    }

    for action in pending_actions {
        match action {
            HistoryAction::Undo => undo_last_command(world),
            HistoryAction::Redo => redo_last_command(world),
        }
    }

    if had_work {
        let at_save = world.resource::<History>().at_save_point();
        if let Some(mut doc_state) =
            world.get_resource_mut::<crate::plugins::document_state::DocumentState>()
        {
            doc_state.dirty = !at_save;
        }
    }
}

/// Evaluate one command's declared plan against the compiled concept graph.
///
/// Admits unconditionally when no graph is installed: a build without a domain
/// pack must keep authoring, not refuse everything.
fn evaluate_command_admissibility(world: &World, command: &dyn EditorCommand) -> Verdict {
    let Some(graph) = world.get_resource::<SemanticGraph>() else {
        return Verdict::Admit;
    };
    let plan = command.semantic_plan(world);
    if plan.is_empty() {
        return Verdict::Admit;
    }
    evaluate(graph, &WorldSemanticContext::new(world), &plan)
}

/// Record a refusal for the caller and surface it in the status bar.
///
/// The kernel never repairs; it refuses and hands back what would satisfy it,
/// so the full diagnostic is retained on the resource for MCP and UI to read.
fn record_refusals(world: &mut World, label: &str, refusals: Vec<Refusal>) {
    let message = refusals
        .first()
        .map(|refusal| format!("{label} refused: {}", refusal.summary()))
        .unwrap_or_else(|| format!("{label} refused."));

    if let Some(mut enforcement) = world.get_resource_mut::<SemanticEnforcement>() {
        enforcement.refusals.extend(refusals);
    }
    set_feedback(world, message);
}

/// Verdict output from the most recent drain.
///
/// Preview reads the same kernel through the same [`evaluate`] call without
/// pushing a command, so preview and commit cannot diverge (ADR-064 §3.3).
#[derive(Resource, Debug, Default, Clone)]
pub struct SemanticEnforcement {
    /// Refusals from the last drain. Cleared by whoever presents them.
    pub refusals: Vec<Refusal>,
    /// Obligations admitted during the last drain.
    pub obligations: Vec<crate::semantics::PendingObligation>,
}

impl SemanticEnforcement {
    pub fn clear(&mut self) {
        self.refusals.clear();
        self.obligations.clear();
    }

    pub fn last_refusal(&self) -> Option<&Refusal> {
        self.refusals.last()
    }
}

fn undo_last_command(world: &mut World) {
    let Some(mut command) = ({
        let mut history = world.resource_mut::<History>();
        history.undo_stack.pop()
    }) else {
        return;
    };

    let message = format!("Undo: {}", command.label());
    command.undo(world);

    let mut history = world.resource_mut::<History>();
    history.redo_stack.push(command);
    history.model_revision = history.model_revision.saturating_add(1);
    set_feedback(world, message);
}

fn redo_last_command(world: &mut World) {
    let Some(mut command) = ({
        let mut history = world.resource_mut::<History>();
        history.redo_stack.pop()
    }) else {
        return;
    };

    let message = format!("Redo: {}", command.label());
    command.redo(world);

    let mut history = world.resource_mut::<History>();
    history.undo_stack.push(command);
    history.model_revision = history.model_revision.saturating_add(1);
    set_feedback(world, message);
}

fn set_feedback(world: &mut World, message: String) {
    let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() else {
        return;
    };

    status_bar_data.set_feedback(message, STATUS_MESSAGE_DURATION_SECONDS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct CounterA(i32);

    #[derive(Resource, Default)]
    struct LogB(Vec<&'static str>);

    /// Test-double command mutating one resource, distinct from `AppendLogCommand`.
    struct IncrementCounterCommand;

    impl EditorCommand for IncrementCounterCommand {
        fn label(&self) -> &'static str {
            "Increment counter"
        }

        fn apply(&mut self, world: &mut World) {
            world.resource_mut::<CounterA>().0 += 1;
        }

        fn undo(&mut self, world: &mut World) {
            world.resource_mut::<CounterA>().0 -= 1;
        }
    }

    /// Test-double command mutating a different resource than `IncrementCounterCommand`,
    /// so a group of the two is heterogeneous.
    struct AppendLogCommand(&'static str);

    impl EditorCommand for AppendLogCommand {
        fn label(&self) -> &'static str {
            "Append log"
        }

        fn apply(&mut self, world: &mut World) {
            world.resource_mut::<LogB>().0.push(self.0);
        }

        fn undo(&mut self, world: &mut World) {
            world.resource_mut::<LogB>().0.pop();
        }
    }

    fn world_with_history() -> World {
        let mut world = World::new();
        world.init_resource::<History>();
        world.init_resource::<PendingCommandQueue>();
        world.init_resource::<SemanticEnforcement>();
        world.insert_resource(CounterA::default());
        world.insert_resource(LogB::default());
        world
    }

    /// Pins `begin_group`/`end_group` atomicity: 2+ heterogeneous sub-commands
    /// collapse into a single `GroupedCommand` history entry, and undo/redo
    /// affect both sub-commands together rather than one at a time.
    #[test]
    fn grouped_commands_apply_undo_redo_as_one_atomic_history_entry() {
        let mut world = world_with_history();

        {
            let mut queue = world.resource_mut::<PendingCommandQueue>();
            queue.begin_group("test group");
            queue.push_command(Box::new(IncrementCounterCommand));
            queue.push_command(Box::new(AppendLogCommand("first")));
            queue.end_group();
        }
        apply_pending_history_commands_for_test(&mut world);

        assert_eq!(
            world.resource::<History>().undo_stack_len(),
            1,
            "two heterogeneous sub-commands must collapse into one grouped entry"
        );
        assert_eq!(world.resource::<CounterA>().0, 1);
        assert_eq!(world.resource::<LogB>().0, vec!["first"]);
        assert_eq!(world.resource::<History>().model_revision(), 1);

        world.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut world);

        assert_eq!(
            world.resource::<History>().undo_stack_len(),
            0,
            "one undo action must pop the whole group as a single history entry"
        );
        assert_eq!(
            world.resource::<CounterA>().0,
            0,
            "atomic undo must revert both sub-command effects together"
        );
        assert!(
            world.resource::<LogB>().0.is_empty(),
            "atomic undo must revert both sub-command effects together"
        );
        assert_eq!(world.resource::<History>().model_revision(), 2);

        world.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands_for_test(&mut world);

        assert_eq!(
            world.resource::<History>().undo_stack_len(),
            1,
            "redo must restore the single grouped entry, not split it"
        );
        assert_eq!(world.resource::<CounterA>().0, 1);
        assert_eq!(world.resource::<LogB>().0, vec!["first"]);
        assert_eq!(world.resource::<History>().model_revision(), 3);
    }
}
