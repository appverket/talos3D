use std::mem;

use bevy::prelude::*;

use crate::plugins::ui::StatusBarData;

const STATUS_MESSAGE_DURATION_SECONDS: f32 = 2.0;

pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(Update, (HistorySet::Queue, HistorySet::Apply).chain())
            .init_resource::<History>()
            .init_resource::<PendingCommandQueue>()
            .add_systems(Update, queue_history_shortcuts.in_set(HistorySet::Queue))
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
}

struct GroupedCommand {
    label: &'static str,
    commands: Vec<Box<dyn EditorCommand>>,
}

impl EditorCommand for GroupedCommand {
    fn label(&self) -> &'static str {
        self.label
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

#[derive(Resource, Default)]
pub struct History {
    undo_stack: Vec<Box<dyn EditorCommand>>,
    redo_stack: Vec<Box<dyn EditorCommand>>,
    /// The undo stack depth at the last save. `None` means never saved in this session.
    save_point: Option<usize>,
}

impl History {
    pub fn clear(&mut self) {
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

#[derive(Resource, Default)]
pub struct PendingCommandQueue {
    pub commands: Vec<Box<dyn EditorCommand>>,
    actions: Vec<HistoryAction>,
    open_groups: Vec<CommandGroupBuilder>,
}

impl PendingCommandQueue {
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

fn queue_history_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut pending_command_queue: ResMut<PendingCommandQueue>,
) {
    let primary_modifier_pressed = if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    };

    if !primary_modifier_pressed || !keys.just_pressed(KeyCode::KeyZ) {
        return;
    }

    let shift_pressed = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if shift_pressed {
        pending_command_queue.queue_redo();
    } else {
        pending_command_queue.queue_undo();
    }
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
        command.apply(world);

        let mut history = world.resource_mut::<History>();
        history.undo_stack.push(command);
        history.redo_stack.clear();
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

fn undo_last_command(world: &mut World) {
    let Some(mut command) = ({
        let mut history = world.resource_mut::<History>();
        history.undo_stack.pop()
    }) else {
        return;
    };

    let message = format!("Undo: {}", command.label());
    command.undo(world);

    world.resource_mut::<History>().redo_stack.push(command);
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

    world.resource_mut::<History>().undo_stack.push(command);
    set_feedback(world, message);
}

fn set_feedback(world: &mut World, message: String) {
    let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() else {
        return;
    };

    status_bar_data.set_feedback(message, STATUS_MESSAGE_DURATION_SECONDS);
}
