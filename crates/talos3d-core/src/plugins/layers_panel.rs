//! Layers panel — the unified view of which objects live on which layer.
//!
//! The panel is a *view* of the live [`LayerRegistry`] plus every authored
//! entity's [`LayerAssignment`]. Each frame, while the panel is open,
//! [`build_layers_panel_data`] flattens both into the [`LayersPanelData`] arena;
//! the egui renderer ([`draw_layers_window`]) reads that arena and returns a
//! [`LayersPanelActions`] batch that `egui_chrome` applies against the registry,
//! the active-layer state, and per-entity assignments.
//!
//! Objects can be dragged between layers: each member row is an egui
//! drag-source carrying a [`LayerMemberDrag`], and each layer header is a
//! drop-zone that reassigns the dropped object to that layer.
//!
//! This replaces the earlier import-staging "Layers" window — the same panel
//! now serves both authored and imported layers, and is toggled like the
//! Outliner rather than auto-appearing.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_egui::egui;
use serde_json::Value;

use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    egui_chrome::EguiChromeSystems,
    entity_labels::entity_label,
    identity::ElementId,
    layers::{LayerAssignment, LayerRegistry, LayerState, DEFAULT_LAYER_NAME},
    selection::apply_click_selection,
    ui::StatusBarData,
};

const PANEL_DEFAULT_WIDTH: f32 = 300.0;
const PANEL_DEFAULT_HEIGHT: f32 = 440.0;
const INDENT: f32 = 16.0;

/// Drag payload carried from a member row to a layer drop-zone.
#[derive(Debug, Clone, Copy)]
pub struct LayerMemberDrag {
    pub entity: Entity,
    pub element_id: u64,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct LayersPanelState {
    pub visible: bool,
    /// Layer names whose member list is collapsed.
    pub collapsed: HashSet<String>,
    /// The layer currently being renamed inline, plus the edit buffer.
    pub renaming: Option<String>,
    pub rename_buffer: String,
}

#[derive(Debug, Clone)]
pub struct LayerRow {
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub color: Option<[f32; 4]>,
    pub active: bool,
    pub member_count: usize,
}

#[derive(Debug, Clone)]
pub struct MemberRow {
    pub entity: Entity,
    pub element_id: u64,
    pub label: String,
}

/// Flattened, owned snapshot of layers + membership, rebuilt each frame while
/// the panel is visible.
#[derive(Resource, Debug, Clone, Default)]
pub struct LayersPanelData {
    pub layers: Vec<LayerRow>,
    pub members_by_layer: HashMap<String, Vec<MemberRow>>,
}

impl LayersPanelData {
    fn clear(&mut self) {
        self.layers.clear();
        self.members_by_layer.clear();
    }
}

/// The batch of edits a single frame of the panel requested. `egui_chrome`
/// drains this against the registry / state / ECS.
#[derive(Debug, Default)]
pub struct LayersPanelActions {
    pub select: Option<(Entity, bool)>,
    pub set_active: Option<String>,
    pub set_visible: Option<(String, bool)>,
    pub set_locked: Option<(String, bool)>,
    pub set_color: Option<(String, Option<[f32; 4]>)>,
    /// (entity, target layer) reassignments produced by drag-and-drop.
    pub reassign: Vec<(Entity, String)>,
    pub create: bool,
    pub rename: Option<(String, String)>,
    pub delete: Option<String>,
}

impl LayersPanelActions {
    pub fn is_empty(&self) -> bool {
        self.select.is_none()
            && self.set_active.is_none()
            && self.set_visible.is_none()
            && self.set_locked.is_none()
            && self.set_color.is_none()
            && self.reassign.is_empty()
            && !self.create
            && self.rename.is_none()
            && self.delete.is_none()
    }
}

pub struct LayersPanelPlugin;

impl Plugin for LayersPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LayersPanelState>()
            .init_resource::<LayersPanelData>()
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_layers".to_string(),
                    label: "Toggle Layers".to_string(),
                    description: "Show or hide the layers panel.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+Shift+L".to_string()),
                    icon: None,
                    hint: Some("Show or hide the layers panel".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_layers,
            )
            .add_systems(Update, build_layers_panel_data.before(EguiChromeSystems));
    }
}

pub fn execute_toggle_layers(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let visible = {
        let mut state = world.resource_mut::<LayersPanelState>();
        state.visible = !state.visible;
        state.visible
    };
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        let message = if visible {
            "Layers panel opened"
        } else {
            "Layers panel closed"
        };
        status.set_feedback(message.to_string(), 2.0);
    }
    Ok(CommandResult::empty())
}

/// Rebuild [`LayersPanelData`] from the registry + per-entity assignments while
/// the panel is visible; otherwise clear it.
pub fn build_layers_panel_data(world: &mut World) {
    let visible = world
        .get_resource::<LayersPanelState>()
        .map(|state| state.visible)
        .unwrap_or(false);
    if !visible {
        if let Some(mut data) = world.get_resource_mut::<LayersPanelData>() {
            if !data.layers.is_empty() || !data.members_by_layer.is_empty() {
                data.clear();
            }
        }
        return;
    }

    // Members: every authored entity, grouped by its assigned layer.
    let raw: Vec<(Entity, u64, String)> = {
        let mut query = world.query::<(Entity, &ElementId, Option<&LayerAssignment>)>();
        query
            .iter(world)
            .map(|(entity, element_id, assignment)| {
                let layer = assignment
                    .map(|a| a.layer.clone())
                    .unwrap_or_else(|| DEFAULT_LAYER_NAME.to_string());
                (entity, element_id.0, layer)
            })
            .collect()
    };
    let mut members_by_layer: HashMap<String, Vec<MemberRow>> = HashMap::new();
    for (entity, element_id, layer) in raw {
        let label = entity_label(world, entity).unwrap_or_else(|| format!("#{element_id}"));
        members_by_layer.entry(layer).or_default().push(MemberRow {
            entity,
            element_id,
            label,
        });
    }
    for members in members_by_layer.values_mut() {
        members.sort_by(|a, b| a.label.cmp(&b.label).then(a.element_id.cmp(&b.element_id)));
    }

    let registry = world.resource::<LayerRegistry>();
    let active = world.resource::<LayerState>().active_layer.clone();
    let layers: Vec<LayerRow> = registry
        .sorted_layers()
        .into_iter()
        .map(|def| LayerRow {
            name: def.name.clone(),
            visible: def.visible,
            locked: def.locked,
            color: def.color,
            active: def.name == active,
            member_count: members_by_layer.get(&def.name).map(Vec::len).unwrap_or(0),
        })
        .collect();

    let mut data = world.resource_mut::<LayersPanelData>();
    data.layers = layers;
    data.members_by_layer = members_by_layer;
}

/// Render the Layers window, returning the batch of edits to apply this frame.
pub fn draw_layers_window(
    ctx: &egui::Context,
    state: &mut LayersPanelState,
    data: &LayersPanelData,
    selected: &HashSet<Entity>,
) -> LayersPanelActions {
    let mut actions = LayersPanelActions::default();
    if !state.visible {
        return actions;
    }

    let mut open = state.visible;
    egui::Window::new("Layers")
        .id(egui::Id::new("talos_layers_window"))
        .default_width(PANEL_DEFAULT_WIDTH)
        .default_height(PANEL_DEFAULT_HEIGHT)
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for layer in &data.layers {
                        render_layer(ui, layer, state, data, selected, &mut actions);
                    }
                });
            ui.separator();
            if ui.button("+ Add Layer").clicked() {
                actions.create = true;
            }
        });
    state.visible = open;
    actions
}

fn render_layer(
    ui: &mut egui::Ui,
    layer: &LayerRow,
    state: &mut LayersPanelState,
    data: &LayersPanelData,
    selected: &HashSet<Entity>,
    actions: &mut LayersPanelActions,
) {
    let expanded = !state.collapsed.contains(&layer.name);
    let is_renaming = state.renaming.as_deref() == Some(layer.name.as_str());

    // The header is a drop-zone: dragging an object onto it reassigns it here.
    let (_, dropped) = ui.dnd_drop_zone::<LayerMemberDrag, _>(egui::Frame::new(), |ui| {
        ui.horizontal(|ui| {
            // Disclosure triangle (only when the layer has members).
            if layer.member_count > 0 {
                if disclosure(ui, expanded) {
                    if expanded {
                        state.collapsed.insert(layer.name.clone());
                    } else {
                        state.collapsed.remove(&layer.name);
                    }
                }
            } else {
                ui.add_space(INDENT);
            }

            // Visibility toggle.
            let mut visible = layer.visible;
            if ui
                .add(egui::Checkbox::without_text(&mut visible))
                .on_hover_text("Visible")
                .changed()
            {
                actions.set_visible = Some((layer.name.clone(), visible));
            }

            // Lock toggle.
            let lock_glyph = if layer.locked { "🔒" } else { "🔓" };
            if ui
                .small_button(lock_glyph)
                .on_hover_text(if layer.locked { "Unlock" } else { "Lock" })
                .clicked()
            {
                actions.set_locked = Some((layer.name.clone(), !layer.locked));
            }

            // Color swatch (editable).
            let mut rgba = layer.color.unwrap_or([0.7, 0.7, 0.7, 1.0]);
            if ui
                .color_edit_button_rgba_unmultiplied(&mut rgba)
                .on_hover_text("Layer color")
                .changed()
            {
                actions.set_color = Some((layer.name.clone(), Some(rgba)));
            }

            // Name (inline rename, or click to activate).
            if is_renaming {
                let response = ui.text_edit_singleline(&mut state.rename_buffer);
                let commit = response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if commit {
                    let new_name = state.rename_buffer.trim().to_string();
                    if !new_name.is_empty() && new_name != layer.name {
                        actions.rename = Some((layer.name.clone(), new_name));
                    }
                    state.renaming = None;
                } else if response.lost_focus() {
                    state.renaming = None;
                }
            } else {
                let title = format!("{}  ({})", layer.name, layer.member_count);
                if ui
                    .selectable_label(layer.active, title)
                    .on_hover_text("Click to make active")
                    .clicked()
                {
                    actions.set_active = Some(layer.name.clone());
                }
            }

            // Per-layer controls, right aligned. The Default layer can't be
            // renamed or deleted.
            if layer.name != DEFAULT_LAYER_NAME && !is_renaming {
                if ui.small_button("🗑").on_hover_text("Delete layer").clicked() {
                    actions.delete = Some(layer.name.clone());
                }
                if ui.small_button("✎").on_hover_text("Rename layer").clicked() {
                    state.renaming = Some(layer.name.clone());
                    state.rename_buffer = layer.name.clone();
                }
            }
        });
    });
    if let Some(payload) = dropped {
        actions.reassign.push((payload.entity, layer.name.clone()));
    }

    // Members.
    if expanded {
        if let Some(members) = data.members_by_layer.get(&layer.name) {
            for member in members {
                render_member(ui, member, selected, actions);
            }
        }
    }
}

fn render_member(
    ui: &mut egui::Ui,
    member: &MemberRow,
    selected: &HashSet<Entity>,
    actions: &mut LayersPanelActions,
) {
    ui.horizontal(|ui| {
        ui.add_space(INDENT * 1.5);
        let drag_id = egui::Id::new(("talos_layer_member", member.element_id));
        let payload = LayerMemberDrag {
            entity: member.entity,
            element_id: member.element_id,
        };
        let is_selected = selected.contains(&member.entity);
        let label = format!("- {}", member.label);
        let inner = ui
            .dnd_drag_source(drag_id, payload, |ui| ui.selectable_label(is_selected, label))
            .inner;
        if inner.clicked() {
            let additive = ui.input(|input| input.modifiers.command || input.modifiers.shift);
            actions.select = Some((member.entity, additive));
        }
    });
}

/// Paint a small disclosure triangle; returns whether it was clicked.
fn disclosure(ui: &mut egui::Ui, expanded: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(INDENT, 14.0), egui::Sense::click());
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let c = rect.center();
    let points = if expanded {
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(4.0, -2.0),
            c + egui::vec2(0.0, 4.0),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -4.0),
            c + egui::vec2(4.0, 0.0),
            c + egui::vec2(-2.0, 4.0),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
    response.clicked()
}

/// Apply a panel action batch against the registry, active-layer state, and ECS.
/// Lives here (rather than in `egui_chrome`) so the panel's view and its writes
/// stay in one module. `members_by_layer` is the current snapshot, used to
/// re-home objects when a layer is renamed or deleted.
pub fn apply_layers_actions(
    actions: &LayersPanelActions,
    registry: &mut LayerRegistry,
    layer_state: &mut LayerState,
    members_by_layer: &HashMap<String, Vec<MemberRow>>,
    current_selection: &HashSet<Entity>,
    commands: &mut Commands,
    status: &mut StatusBarData,
) {
    if let Some((entity, additive)) = actions.select {
        apply_click_selection(commands, current_selection, entity, additive);
    }
    if let Some(name) = &actions.set_active {
        layer_state.set_active(name.clone(), registry);
    }
    if let Some((name, visible)) = &actions.set_visible {
        if let Some(def) = registry.layers.get_mut(name) {
            def.visible = *visible;
        }
    }
    if let Some((name, locked)) = &actions.set_locked {
        if let Some(def) = registry.layers.get_mut(name) {
            def.locked = *locked;
        }
    }
    if let Some((name, color)) = &actions.set_color {
        if let Some(def) = registry.layers.get_mut(name) {
            def.color = *color;
        }
    }
    for (entity, layer) in &actions.reassign {
        registry.ensure_layer(layer);
        commands.entity(*entity).insert(LayerAssignment::new(layer));
        status.set_feedback(format!("Moved object to layer '{layer}'"), 2.0);
    }
    if actions.create {
        let name = registry.generate_unique_name();
        registry.create_layer(name);
    }
    if let Some((old, new)) = &actions.rename {
        match registry.rename_layer(old, new.clone()) {
            Ok(()) => {
                // Re-home the renamed layer's members onto the new name.
                if let Some(members) = members_by_layer.get(old) {
                    for member in members {
                        commands
                            .entity(member.entity)
                            .insert(LayerAssignment::new(new));
                    }
                }
                if layer_state.active_layer == *old {
                    layer_state.active_layer = new.clone();
                }
            }
            Err(err) => status.set_feedback(err, 3.0),
        }
    }
    if let Some(name) = &actions.delete {
        // Move members to Default before removing the layer.
        if let Some(members) = members_by_layer.get(name) {
            for member in members {
                commands
                    .entity(member.entity)
                    .insert(LayerAssignment::default_layer());
            }
        }
        match registry.delete_layer(name) {
            Ok(()) => {
                if layer_state.active_layer == *name {
                    layer_state.active_layer = DEFAULT_LAYER_NAME.to_string();
                }
            }
            Err(err) => status.set_feedback(err, 3.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::CapabilityRegistry;

    fn boot(world: &mut World) {
        world.init_resource::<LayerRegistry>();
        world.init_resource::<LayerState>();
        world.init_resource::<CapabilityRegistry>();
        world.insert_resource(LayersPanelState {
            visible: true,
            ..Default::default()
        });
        world.init_resource::<LayersPanelData>();
    }

    #[test]
    fn build_groups_members_by_layer() {
        let mut world = World::new();
        boot(&mut world);
        world
            .resource_mut::<LayerRegistry>()
            .create_layer("Walls".to_string());
        world.spawn((ElementId(1), LayerAssignment::new("Walls")));
        world.spawn((ElementId(2), LayerAssignment::new("Walls")));
        // No assignment → Default.
        world.spawn(ElementId(3));

        build_layers_panel_data(&mut world);
        let data = world.resource::<LayersPanelData>();
        let walls = data
            .layers
            .iter()
            .find(|l| l.name == "Walls")
            .expect("walls layer");
        assert_eq!(walls.member_count, 2);
        assert_eq!(data.members_by_layer.get("Walls").unwrap().len(), 2);
        assert_eq!(data.members_by_layer.get(DEFAULT_LAYER_NAME).unwrap().len(), 1);
    }

    #[test]
    fn hidden_panel_clears_data() {
        let mut world = World::new();
        boot(&mut world);
        world.resource_mut::<LayersPanelState>().visible = false;
        world.spawn(ElementId(1));
        build_layers_panel_data(&mut world);
        assert!(world.resource::<LayersPanelData>().layers.is_empty());
    }

    #[test]
    fn toggle_command_flips_visibility() {
        let mut world = World::new();
        world.init_resource::<LayersPanelState>();
        execute_toggle_layers(&mut world, &Value::Null).unwrap();
        assert!(world.resource::<LayersPanelState>().visible);
        execute_toggle_layers(&mut world, &Value::Null).unwrap();
        assert!(!world.resource::<LayersPanelState>().visible);
    }

    #[test]
    fn reassign_action_updates_assignment_and_layer_exists() {
        let mut world = World::new();
        boot(&mut world);
        let entity = world.spawn(ElementId(1)).id();
        let members: HashMap<String, Vec<MemberRow>> = HashMap::new();
        let actions = LayersPanelActions {
            reassign: vec![(entity, "Roof".to_string())],
            ..Default::default()
        };
        world.resource_scope(|world, mut registry: Mut<LayerRegistry>| {
            world.resource_scope(|world, mut layer_state: Mut<LayerState>| {
                let mut status = StatusBarData::default();
                let mut commands_queue = bevy::ecs::world::CommandQueue::default();
                let mut commands = Commands::new(&mut commands_queue, world);
                let selection = HashSet::new();
                apply_layers_actions(
                    &actions,
                    &mut registry,
                    &mut layer_state,
                    &members,
                    &selection,
                    &mut commands,
                    &mut status,
                );
                commands_queue.apply(world);
            });
        });
        assert!(world.resource::<LayerRegistry>().layers.contains_key("Roof"));
        assert_eq!(
            world.get::<LayerAssignment>(entity).map(|a| a.layer.clone()),
            Some("Roof".to_string())
        );
    }
}
