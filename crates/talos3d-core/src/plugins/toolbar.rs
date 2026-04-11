use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::{command_registry::CommandCategory, document_properties::DocumentProperties};

pub struct ToolbarPlugin;

impl Plugin for ToolbarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ToolbarRegistry>()
            .init_resource::<ToolbarLayoutState>()
            .init_resource::<FloatingToolbarStates>()
            .add_systems(Update, sync_toolbar_layout_state);
        register_core_toolbar(app);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolbarDock {
    Top,
    Bottom,
    Left,
    Right,
    Floating,
}

impl ToolbarDock {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
            Self::Floating => "floating",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "floating" => Some(Self::Floating),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolbarSection {
    pub label: String,
    pub command_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ToolbarDescriptor {
    pub id: String,
    pub label: String,
    pub default_dock: ToolbarDock,
    pub default_visible: bool,
    pub sections: Vec<ToolbarSection>,
}

#[derive(Resource, Default, Debug, Clone)]
pub struct ToolbarRegistry {
    toolbars: Vec<ToolbarDescriptor>,
}

impl ToolbarRegistry {
    pub fn toolbars(&self) -> impl Iterator<Item = &ToolbarDescriptor> {
        self.toolbars.iter()
    }

    pub(crate) fn register(&mut self, descriptor: ToolbarDescriptor) {
        if let Some(existing) = self
            .toolbars
            .iter_mut()
            .find(|toolbar| toolbar.id == descriptor.id)
        {
            *existing = descriptor;
            return;
        }
        self.toolbars.push(descriptor);
    }
}

pub trait ToolbarRegistryAppExt {
    fn register_toolbar(&mut self, descriptor: ToolbarDescriptor) -> &mut Self;
}

impl ToolbarRegistryAppExt for App {
    fn register_toolbar(&mut self, descriptor: ToolbarDescriptor) -> &mut Self {
        if !self.world().contains_resource::<ToolbarRegistry>() {
            self.init_resource::<ToolbarRegistry>();
        }
        self.world_mut()
            .resource_mut::<ToolbarRegistry>()
            .register(descriptor);
        self
    }
}

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct ToolbarLayoutState {
    pub(crate) entries: HashMap<String, ToolbarLayoutEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarLayoutEntry {
    pub dock: ToolbarDock,
    /// Row/column band within the dock (0 = closest to the edge).
    /// For Top/Bottom: horizontal band index.
    /// For Left/Right: vertical column index.
    pub row: u32,
    /// Position within the row (left-to-right for Top/Bottom, top-to-bottom for Left/Right).
    pub order: u32,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatingToolbarEntry {
    pub position: [f32; 2],
    pub minimized: bool,
}

#[derive(Resource, Default, Debug, Clone)]
pub struct FloatingToolbarStates {
    pub(crate) entries: HashMap<String, FloatingToolbarEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedToolbarLayoutEntry {
    dock: String,
    #[serde(default)]
    row: u32,
    order: u32,
    visible: bool,
}

pub(crate) fn sync_toolbar_layout_state(
    registry: Res<ToolbarRegistry>,
    mut layout_state: ResMut<ToolbarLayoutState>,
    mut floating_states: ResMut<FloatingToolbarStates>,
    mut doc_props: ResMut<DocumentProperties>,
) {
    let saved_layout = toolbar_layout_from_document(&doc_props);
    let mut saved_order_counters = HashMap::<ToolbarDock, u32>::new();

    for descriptor in registry.toolbars() {
        let Some(saved) = saved_layout.get(&descriptor.id) else {
            continue;
        };
        let Some(dock) = ToolbarDock::from_str(&saved.dock) else {
            continue;
        };
        let next = saved.order.saturating_add(1);
        saved_order_counters
            .entry(dock)
            .and_modify(|value| *value = (*value).max(next))
            .or_insert(next);
    }

    let mut next_entries = HashMap::new();
    for descriptor in registry.toolbars() {
        let entry = if let Some(saved) = saved_layout.get(&descriptor.id) {
            ToolbarLayoutEntry {
                dock: ToolbarDock::from_str(&saved.dock).unwrap_or(descriptor.default_dock),
                row: saved.row,
                order: saved.order,
                visible: descriptor.id == "core" || saved.visible,
            }
        } else {
            let order = saved_order_counters
                .entry(descriptor.default_dock)
                .or_insert(0);
            let next_order = *order;
            *order = order.saturating_add(1);
            ToolbarLayoutEntry {
                dock: descriptor.default_dock,
                row: 0,
                order: next_order,
                visible: descriptor.default_visible,
            }
        };
        next_entries.insert(descriptor.id.clone(), entry);
    }

    if layout_state.entries != next_entries {
        layout_state.entries = next_entries;
    }

    let serialized_layout = serialize_toolbar_layout(&layout_state.entries);
    if doc_props.domain_defaults.get("toolbar_layout") != Some(&serialized_layout) {
        doc_props
            .domain_defaults
            .insert("toolbar_layout".to_string(), serialized_layout);
    }

    let saved_floating = floating_states_from_document(&doc_props);
    if floating_states.entries != saved_floating {
        floating_states.entries = saved_floating;
    }
    let serialized_floating = serialize_floating_states(&floating_states.entries);
    if doc_props.domain_defaults.get("floating_toolbar_states") != Some(&serialized_floating) {
        doc_props
            .domain_defaults
            .insert("floating_toolbar_states".to_string(), serialized_floating);
    }
}

/// Applies a precise drag-and-drop insertion into a specific row and position.
///
/// - If `new_row` is true, all existing entries in `dock` with `row >= target_row`
///   are shifted down by 1 before insertion.
/// - `insert_after` names the toolbar that the moved toolbar should follow within
///   the row. `None` means prepend (order 0).
/// - Re-assigns contiguous `order` values to every toolbar in the target row.
pub(crate) fn apply_toolbar_layout_precise(
    layout_state: &mut ToolbarLayoutState,
    doc_props: &mut DocumentProperties,
    toolbar_id: &str,
    dock: ToolbarDock,
    target_row: u32,
    new_row: bool,
    insert_after: Option<&str>,
) {
    // Guard: toolbar must exist in the layout state.
    if !layout_state.entries.contains_key(toolbar_id) {
        return;
    }

    // 1. If inserting a new row, shift all existing entries in this dock down.
    if new_row {
        for (id, entry) in layout_state.entries.iter_mut() {
            if id != toolbar_id && entry.dock == dock && entry.row >= target_row {
                entry.row += 1;
            }
        }
    }

    // 2. Move the toolbar to the target dock/row.
    if let Some(entry) = layout_state.entries.get_mut(toolbar_id) {
        entry.dock = dock;
        entry.row = target_row;
        entry.visible = true;
    }

    // 3. Collect all IDs in the target (dock, row) except the toolbar being moved,
    //    sorted by their current order.
    let mut siblings: Vec<(String, u32)> = layout_state
        .entries
        .iter()
        .filter(|(id, entry)| {
            id.as_str() != toolbar_id && entry.dock == dock && entry.row == target_row
        })
        .map(|(id, entry)| (id.clone(), entry.order))
        .collect();
    siblings.sort_by_key(|(_, order)| *order);

    // 4. Find the insertion index.
    let insert_index = match insert_after {
        None => 0,
        Some(after_id) => siblings
            .iter()
            .position(|(id, _)| id == after_id)
            .map(|pos| pos + 1)
            .unwrap_or(siblings.len()),
    };

    // 5. Build the ordered list: [prefix] + [toolbar_id] + [suffix].
    let mut ordered: Vec<String> = siblings.iter().map(|(id, _)| id.clone()).collect();
    ordered.insert(insert_index, toolbar_id.to_string());

    // 6. Re-assign contiguous order values.
    for (new_order, id) in ordered.iter().enumerate() {
        if let Some(entry) = layout_state.entries.get_mut(id) {
            entry.order = new_order as u32;
        }
    }

    // 7. Persist.
    doc_props.domain_defaults.insert(
        "toolbar_layout".to_string(),
        serialize_toolbar_layout(&layout_state.entries),
    );
}

#[allow(dead_code)] // used by the model-api feature via update_toolbar_layout_entry
pub(crate) fn apply_toolbar_layout_change(
    layout_state: &mut ToolbarLayoutState,
    doc_props: &mut DocumentProperties,
    toolbar_id: &str,
    dock: ToolbarDock,
) {
    let next_order = layout_state
        .entries
        .iter()
        .filter(|(id, entry)| id.as_str() != toolbar_id && entry.dock == dock)
        .map(|(_, entry)| entry.order)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    if let Some(entry) = layout_state.entries.get_mut(toolbar_id) {
        entry.dock = dock;
        entry.row = 0;
        entry.order = next_order;
        entry.visible = true;
    }

    doc_props.domain_defaults.insert(
        "toolbar_layout".to_string(),
        serialize_toolbar_layout(&layout_state.entries),
    );
}

pub(crate) fn set_toolbar_visibility(
    layout_state: &mut ToolbarLayoutState,
    doc_props: &mut DocumentProperties,
    toolbar_id: &str,
    visible: bool,
) {
    if let Some(entry) = layout_state.entries.get_mut(toolbar_id) {
        entry.visible = toolbar_id == "core" || visible;
    }
    doc_props.domain_defaults.insert(
        "toolbar_layout".to_string(),
        serialize_toolbar_layout(&layout_state.entries),
    );
}

#[cfg(feature = "model-api")]
pub(crate) fn update_toolbar_layout_entry(
    layout_state: &mut ToolbarLayoutState,
    doc_props: &mut DocumentProperties,
    toolbar_id: &str,
    dock: Option<ToolbarDock>,
    order: Option<u32>,
    visible: Option<bool>,
) -> Result<(), String> {
    let Some(current_entry) = layout_state.entries.get(toolbar_id).cloned() else {
        return Err(format!("Unknown toolbar: {toolbar_id}"));
    };

    if let Some(next_dock) = dock.filter(|dock| *dock != current_entry.dock) {
        apply_toolbar_layout_change(layout_state, doc_props, toolbar_id, next_dock);
    }

    let Some(entry) = layout_state.entries.get_mut(toolbar_id) else {
        return Err(format!("Unknown toolbar: {toolbar_id}"));
    };
    if let Some(order) = order {
        entry.order = order;
    }
    if let Some(visible) = visible {
        entry.visible = toolbar_id == "core" || visible;
    }

    doc_props.domain_defaults.insert(
        "toolbar_layout".to_string(),
        serialize_toolbar_layout(&layout_state.entries),
    );
    Ok(())
}

fn toolbar_layout_from_document(
    doc_props: &DocumentProperties,
) -> HashMap<String, PersistedToolbarLayoutEntry> {
    doc_props
        .domain_defaults
        .get("toolbar_layout")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn serialize_toolbar_layout(entries: &HashMap<String, ToolbarLayoutEntry>) -> serde_json::Value {
    let mut persisted = HashMap::new();
    for (id, entry) in entries {
        persisted.insert(
            id.clone(),
            PersistedToolbarLayoutEntry {
                dock: entry.dock.as_str().to_string(),
                row: entry.row,
                order: entry.order,
                visible: entry.visible,
            },
        );
    }
    serde_json::to_value(persisted).unwrap_or_else(|_| serde_json::json!({}))
}

fn floating_states_from_document(
    doc_props: &DocumentProperties,
) -> HashMap<String, FloatingToolbarEntry> {
    doc_props
        .domain_defaults
        .get("floating_toolbar_states")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn serialize_floating_states(entries: &HashMap<String, FloatingToolbarEntry>) -> serde_json::Value {
    serde_json::to_value(entries).unwrap_or_else(|_| serde_json::json!({}))
}

pub(crate) fn apply_toolbar_float(
    layout_state: &mut ToolbarLayoutState,
    floating_states: &mut FloatingToolbarStates,
    doc_props: &mut DocumentProperties,
    toolbar_id: &str,
    position: [f32; 2],
) {
    if let Some(entry) = layout_state.entries.get_mut(toolbar_id) {
        entry.dock = ToolbarDock::Floating;
        entry.row = 0;
        entry.order = 0;
        entry.visible = true;
    }
    floating_states.entries.insert(
        toolbar_id.to_string(),
        FloatingToolbarEntry {
            position,
            minimized: false,
        },
    );
    doc_props.domain_defaults.insert(
        "toolbar_layout".to_string(),
        serialize_toolbar_layout(&layout_state.entries),
    );
    doc_props.domain_defaults.insert(
        "floating_toolbar_states".to_string(),
        serialize_floating_states(&floating_states.entries),
    );
}

pub(crate) struct RedockTarget<'a> {
    pub dock: ToolbarDock,
    pub target_row: u32,
    pub new_row: bool,
    pub insert_after: Option<&'a str>,
}

pub(crate) fn redock_toolbar(
    layout_state: &mut ToolbarLayoutState,
    floating_states: &mut FloatingToolbarStates,
    doc_props: &mut DocumentProperties,
    toolbar_id: &str,
    target: RedockTarget<'_>,
) {
    floating_states.entries.remove(toolbar_id);
    apply_toolbar_layout_precise(
        layout_state,
        doc_props,
        toolbar_id,
        target.dock,
        target.target_row,
        target.new_row,
        target.insert_after,
    );
    doc_props.domain_defaults.insert(
        "floating_toolbar_states".to_string(),
        serialize_floating_states(&floating_states.entries),
    );
}

fn register_core_toolbar(app: &mut App) {
    let file_section = ToolbarSection {
        label: CommandCategory::File.label().to_string(),
        command_ids: vec![
            "core.new".to_string(),
            "core.open".to_string(),
            "core.save".to_string(),
            "core.export_drawing".to_string(),
            "core.import".to_string(),
        ],
    };
    let edit_section = ToolbarSection {
        label: CommandCategory::Edit.label().to_string(),
        command_ids: vec![
            "core.undo".to_string(),
            "core.redo".to_string(),
            "core.delete".to_string(),
        ],
    };
    let select_section = ToolbarSection {
        label: "Select".to_string(),
        command_ids: vec![
            "core.select_tool".to_string(),
            "core.select_all".to_string(),
            "core.deselect".to_string(),
        ],
    };

    app.register_toolbar(ToolbarDescriptor {
        id: "core".to_string(),
        label: "Core".to_string(),
        default_dock: ToolbarDock::Top,
        default_visible: true,
        sections: vec![file_section, edit_section, select_section],
    });
}
