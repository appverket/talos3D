use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::{command_registry::CommandCategory, document_properties::DocumentProperties};

pub struct ToolbarPlugin;

impl Plugin for ToolbarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ToolbarRegistry>()
            .init_resource::<ToolbarLayoutState>()
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
}

impl ToolbarDock {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
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
    pub order: u32,
    pub visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedToolbarLayoutEntry {
    dock: String,
    order: u32,
    visible: bool,
}

pub(crate) fn sync_toolbar_layout_state(
    registry: Res<ToolbarRegistry>,
    mut layout_state: ResMut<ToolbarLayoutState>,
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
                order: next_order,
                visible: true,
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
}

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
                order: entry.order,
                visible: entry.visible,
            },
        );
    }
    serde_json::to_value(persisted).unwrap_or_else(|_| serde_json::json!({}))
}

fn register_core_toolbar(app: &mut App) {
    let file_section = ToolbarSection {
        label: CommandCategory::File.label().to_string(),
        command_ids: vec![
            "core.new".to_string(),
            "core.open".to_string(),
            "core.save".to_string(),
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
        sections: vec![file_section, edit_section, select_section],
    });
}
