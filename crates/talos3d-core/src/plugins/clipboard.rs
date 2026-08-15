//! Cut / Copy / Paste / Duplicate for authored entities.
//!
//! Built on the same snapshot round-trip persistence uses
//! (`AuthoredEntity::to_persisted_json` -> `AuthoredEntityFactory::from_persisted_json`),
//! so every authored type is copyable the moment it is persistable — no type
//! ever has to opt in, and the clipboard cannot drift from what a saved document
//! preserves.
//!
//! Copies flow through `CreateEntityCommand`, so paste and duplicate are
//! undoable like any other edit.
//!
//! **Aggregates come along.** Copying a group copies its members too: a group
//! snapshot references members by `ElementId`, so pasting the group alone would
//! produce a group pointing at the *originals*. The copied set is therefore
//! expanded through group membership, and every id reference inside the copied
//! JSON is remapped to the newly allocated ids.

use bevy::prelude::*;
use serde_json::Value;

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    commands::{BeginCommandGroup, CreateEntityCommand, DeleteEntitiesCommand, EndCommandGroup},
    identity::{ElementId, ElementIdAllocator},
    modeling::group::{collect_group_members_recursive, GroupMembers},
    selection::Selected,
    ui::StatusBarData,
};

pub struct ClipboardPlugin;

/// One copied entity: the factory that can rebuild it, plus its persisted form.
#[derive(Debug, Clone)]
struct ClipboardEntry {
    type_name: String,
    /// Id of the entity this was captured from.
    ///
    /// Taken from the snapshot rather than read back out of `data`: a snapshot
    /// is free to nest its id (a group serializes through a tagged enum, so its
    /// `element_id` is one level down). Reading the JSON missed those, no new id
    /// was allocated, and rebuilding the snapshot *overwrote the original* —
    /// duplicating a group emptied the group it was copied from.
    source_id: ElementId,
    data: Value,
}

/// Entities copied by the last Cut or Copy, in persisted form.
///
/// Held as JSON rather than as live snapshots so the clipboard survives the
/// originals being deleted — which Cut does immediately.
#[derive(Resource, Default, Debug, Clone)]
pub struct Clipboard {
    entries: Vec<ClipboardEntry>,
}

impl Clipboard {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Ids awaiting selection once their entities exist.
///
/// `CreateEntityCommand` is a message applied by a later system, so pasted
/// entities are not in the world yet when the paste command returns — selecting
/// them inline silently selected nothing.
#[derive(Resource, Default, Debug, Clone)]
struct PendingPasteSelection {
    element_ids: Vec<ElementId>,
}

/// Select pasted entities on the first frame they exist.
fn apply_pending_paste_selection(world: &mut World) {
    let pending = world
        .resource::<PendingPasteSelection>()
        .element_ids
        .clone();
    if pending.is_empty() {
        return;
    }
    // Wait until the whole batch has landed, so a partially-applied paste never
    // leaves half of it selected.
    if pending
        .iter()
        .any(|element_id| find_entity(world, *element_id).is_none())
    {
        return;
    }
    select_only(world, &pending);
    world
        .resource_mut::<PendingPasteSelection>()
        .element_ids
        .clear();
}

impl Plugin for ClipboardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Clipboard>()
            .init_resource::<PendingPasteSelection>()
            .add_systems(Update, apply_pending_paste_selection)
            .register_command(
                CommandDescriptor {
                    id: "core.copy".to_string(),
                    label: "Copy".to_string(),
                    description: "Copy the selected entities to the clipboard.".to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+C".to_string()),
                    icon: None,
                    hint: Some("Copy the selection".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_copy,
            )
            .register_command(
                CommandDescriptor {
                    id: "core.cut".to_string(),
                    label: "Cut".to_string(),
                    description: "Copy the selected entities to the clipboard and delete them."
                        .to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+X".to_string()),
                    icon: None,
                    hint: Some("Cut the selection".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_cut,
            )
            .register_command(
                CommandDescriptor {
                    id: "core.paste".to_string(),
                    label: "Paste".to_string(),
                    description: "Paste the clipboard contents as new entities.".to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+V".to_string()),
                    icon: None,
                    hint: Some("Paste the clipboard".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_paste,
            )
            .register_command(
                CommandDescriptor {
                    id: "core.duplicate".to_string(),
                    label: "Duplicate".to_string(),
                    description: "Duplicate the selected entities in place, leaving the \
                                  clipboard untouched."
                        .to_string(),
                    category: CommandCategory::Edit,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+D".to_string()),
                    icon: None,
                    hint: Some("Duplicate the selection".to_string()),
                    requires_selection: true,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_duplicate,
            );
    }
}

/// Capture the selection, expanded through group membership, in persisted form.
fn capture_selection(world: &mut World) -> Result<Vec<ClipboardEntry>, String> {
    let selected: Vec<ElementId> = {
        let mut query = world.query_filtered::<&ElementId, With<Selected>>();
        query.iter(world).copied().collect()
    };
    if selected.is_empty() {
        return Err("Select something to copy".to_string());
    }

    // A group's snapshot names its members by id, so the members have to travel
    // with it or the pasted group would adopt the originals.
    let mut wanted: Vec<ElementId> = Vec::new();
    for element_id in &selected {
        if !wanted.contains(element_id) {
            wanted.push(*element_id);
        }
        if is_group(world, *element_id) {
            for member in collect_group_members_recursive(world, *element_id) {
                if !wanted.contains(&member) {
                    wanted.push(member);
                }
            }
        }
    }

    let entities: Vec<Entity> = wanted
        .iter()
        .filter_map(|element_id| find_entity(world, *element_id))
        .collect();
    let registry = world.resource::<CapabilityRegistry>();
    let mut entries = Vec::new();
    for entity in entities {
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        if snapshot.scope() != crate::authored_entity::EntityScope::AuthoredModel {
            continue;
        }
        entries.push(ClipboardEntry {
            type_name: snapshot.type_name().to_string(),
            source_id: snapshot.element_id(),
            data: snapshot.to_persisted_json(),
        });
    }

    if entries.is_empty() {
        return Err("The selection has nothing that can be copied".to_string());
    }
    Ok(entries)
}

fn is_group(world: &mut World, element_id: ElementId) -> bool {
    find_entity(world, element_id)
        .and_then(|entity| world.get::<GroupMembers>(entity))
        .is_some()
}

fn find_entity(world: &mut World, element_id: ElementId) -> Option<Entity> {
    let mut query = world.query::<(Entity, &ElementId)>();
    query
        .iter(world)
        .find_map(|(entity, id)| (*id == element_id).then_some(entity))
}

/// Rebuild `entries` as new entities with freshly allocated ids.
///
/// Returns the ids of everything created, so the caller can select the result.
fn instantiate(world: &mut World, entries: &[ClipboardEntry]) -> Result<Vec<ElementId>, String> {
    // Allocate the new ids first: remapping needs the complete old -> new map
    // before any snapshot is rebuilt, or a group would be rewritten before its
    // members had ids.
    let mut id_map: Vec<(u64, u64)> = Vec::new();
    for entry in entries {
        let new = world.resource::<ElementIdAllocator>().next_id();
        id_map.push((entry.source_id.0, new.0));
    }

    let registry = world.resource::<CapabilityRegistry>();
    let mut snapshots = Vec::new();
    for entry in entries {
        let mut data = entry.data.clone();
        remap_ids(&mut data, &id_map);
        let factory = registry
            .factory_for(&entry.type_name)
            .ok_or_else(|| format!("No factory can rebuild a `{}`", entry.type_name))?;
        let snapshot = factory.from_persisted_json(&data)?;
        // Applying a snapshot that still carries the source id updates the
        // original in place instead of creating a copy, which is destructive and
        // silent. Refuse rather than risk it.
        if snapshot.element_id() == entry.source_id {
            return Err(format!(
                "Cannot copy `{}`: its id was not remapped, so pasting would \
                 overwrite the original",
                entry.type_name
            ));
        }
        snapshots.push(snapshot);
    }

    let created: Vec<ElementId> = snapshots.iter().map(|s| s.element_id()).collect();
    world.write_message(BeginCommandGroup { label: "Paste" });
    for snapshot in snapshots {
        world.write_message(CreateEntityCommand { snapshot });
    }
    world.write_message(EndCommandGroup);
    Ok(created)
}

/// Rewrite every id reference in a persisted snapshot to its new value.
///
/// Only values under keys that name an id are considered, so ordinary numeric
/// data (counts, coordinates, thicknesses) can never be rewritten by collision
/// with an element id.
fn remap_ids(value: &mut Value, id_map: &[(u64, u64)]) {
    fn key_holds_ids(key: &str) -> bool {
        key == "element_id"
            || key == "member_ids"
            || key == "members"
            || key == "source"
            || key == "target"
            || key == "owner"
            || key == "host"
            || key.ends_with("_id")
            || key.ends_with("_ids")
    }

    fn remap_scalar(value: &mut Value, id_map: &[(u64, u64)]) {
        let Some(current) = value.as_u64() else {
            return;
        };
        if let Some((_, new)) = id_map.iter().find(|(old, _)| *old == current) {
            *value = Value::from(*new);
        }
    }

    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if key_holds_ids(key) {
                    if let Value::Array(items) = child {
                        for item in items.iter_mut() {
                            remap_scalar(item, id_map);
                        }
                    } else {
                        remap_scalar(child, id_map);
                    }
                }
                remap_ids(child, id_map);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                remap_ids(item, id_map);
            }
        }
        _ => {}
    }
}

fn select_only(world: &mut World, element_ids: &[ElementId]) {
    let previously_selected: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect()
    };
    for entity in previously_selected {
        world.entity_mut(entity).remove::<Selected>();
    }
    for element_id in element_ids {
        if let Some(entity) = find_entity(world, *element_id) {
            world.entity_mut(entity).insert(Selected);
        }
    }
}

fn feedback(world: &mut World, message: String) {
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(message, 2.0);
    }
}

fn execute_copy(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let entries = capture_selection(world)?;
    let count = entries.len();
    world.insert_resource(Clipboard { entries });
    feedback(world, format!("Copied {count} element(s)"));
    Ok(CommandResult::empty())
}

fn execute_cut(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let entries = capture_selection(world)?;
    let count = entries.len();
    let removed: Vec<ElementId> = entries.iter().map(|entry| entry.source_id).collect();
    world.insert_resource(Clipboard { entries });
    world.write_message(DeleteEntitiesCommand {
        element_ids: removed.clone(),
    });
    feedback(world, format!("Cut {count} element(s)"));
    Ok(CommandResult {
        deleted: removed.iter().map(|id| id.0).collect(),
        ..Default::default()
    })
}

fn execute_paste(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let entries = world.resource::<Clipboard>().entries.clone();
    if entries.is_empty() {
        return Err("The clipboard is empty".to_string());
    }
    let created = instantiate(world, &entries)?;
    // Select the copies, not the originals: the next action is almost always to
    // move what was just pasted. Deferred, because they do not exist yet.
    world.resource_mut::<PendingPasteSelection>().element_ids = created.clone();
    feedback(world, format!("Pasted {} element(s)", created.len()));
    Ok(CommandResult {
        created: created.iter().map(|id| id.0).collect(),
        ..Default::default()
    })
}

fn execute_duplicate(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    // Duplicate deliberately does not touch the clipboard — duplicating
    // something should not discard what the user copied earlier.
    let entries = capture_selection(world)?;
    let created = instantiate(world, &entries)?;
    world.resource_mut::<PendingPasteSelection>().element_ids = created.clone();
    feedback(world, format!("Duplicated {} element(s)", created.len()));
    Ok(CommandResult {
        created: created.iter().map(|id| id.0).collect(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_remapped_only_where_a_key_names_an_id() {
        let mut value = serde_json::json!({
            "element_id": 10,
            "member_ids": [10, 11, 99],
            "count": 10,
            "centre": [10.0, 11.0, 12.0],
            "half_extents": [10, 11, 12],
            "nested": { "source": 11, "spacing": 10 }
        });
        remap_ids(&mut value, &[(10, 500), (11, 501)]);

        assert_eq!(value["element_id"], serde_json::json!(500));
        assert_eq!(value["member_ids"], serde_json::json!([500, 501, 99]));
        // Plain data that happens to equal an id must survive untouched.
        assert_eq!(value["count"], serde_json::json!(10));
        assert_eq!(value["centre"], serde_json::json!([10.0, 11.0, 12.0]));
        assert_eq!(value["half_extents"], serde_json::json!([10, 11, 12]));
        assert_eq!(value["nested"]["source"], serde_json::json!(501));
        assert_eq!(value["nested"]["spacing"], serde_json::json!(10));
    }

    /// A group serializes through a tagged enum, so its `element_id` sits one
    /// level down. Remapping has to reach it — when it did not, the rebuilt
    /// group kept the source id and overwrote the group it was copied from.
    #[test]
    fn nested_ids_are_remapped_too() {
        let mut value = serde_json::json!({
            "Group": {
                "element_id": 120,
                "name": "Roof",
                "member_ids": [86, 87],
                "frame": {"translation": [0.0, 3.64, 0.0]}
            }
        });
        remap_ids(&mut value, &[(120, 300), (86, 301), (87, 302)]);

        assert_eq!(value["Group"]["element_id"], serde_json::json!(300));
        assert_eq!(value["Group"]["member_ids"], serde_json::json!([301, 302]));
        assert_eq!(
            value["Group"]["frame"]["translation"],
            serde_json::json!([0.0, 3.64, 0.0]),
            "geometry must not be touched"
        );
    }

    #[test]
    fn unknown_ids_are_left_alone() {
        let mut value = serde_json::json!({"element_id": 7, "target_id": 42});
        remap_ids(&mut value, &[(7, 70)]);
        assert_eq!(value["element_id"], serde_json::json!(70));
        assert_eq!(value["target_id"], serde_json::json!(42));
    }
}
