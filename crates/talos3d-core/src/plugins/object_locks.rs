//! Authored object locks and a change-driven, constant-time interaction filter.
use std::collections::{BTreeSet, HashMap, HashSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    commands::find_entity_by_element_id_readonly,
    history::{EditorCommand, PendingCommandQueue},
    identity::ElementId,
    layers::LayerVisibilityExempt,
    modeling::{
        group::GroupMembers, occurrence::GeneratedOccurrencePart,
        profile_feature::FaceProfileFeature,
    },
    selection::{Selected, SelectionProxy},
};

/// Explicit locks, independent of visibility and keyed by stable document IDs.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectLocks {
    pub locked: BTreeSet<ElementId>,
}

/// Effective locks, including descendants and generated geometry owners.
#[derive(Resource, Debug, Default)]
pub struct DocumentLocks {
    pub locked: HashSet<ElementId>,
}

impl DocumentLocks {
    pub fn excludes(&self, id: Option<&ElementId>, owner: Option<ElementId>) -> bool {
        id.is_some_and(|id| self.locked.contains(id))
            || owner.is_some_and(|id| self.locked.contains(&id))
    }
}

pub fn entity_locked(world: &World, entity: Entity) -> bool {
    world.get_resource::<DocumentLocks>().is_some_and(|state| {
        !state.locked.is_empty()
            && state.excludes(
                world.get::<ElementId>(entity),
                world
                    .get::<GeneratedOccurrencePart>(entity)
                    .map(|part| part.owner),
            )
    })
}

pub struct ObjectLockPlugin;

impl Plugin for ObjectLockPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ObjectLocks>()
            .init_resource::<DocumentLocks>()
            .register_command(CommandDescriptor {
                id: "view.set_object_locked".into(),
                label: "Lock or unlock objects".into(),
                description: "Locked objects stay visible but ignore viewport interaction. Group locks include descendants. Unlock from the Outliner; child locks remain independent.".into(),
                category: CommandCategory::View,
                parameters: Some(serde_json::json!({"type":"object","properties":{
                    "element_ids":{"type":"array","items":{"type":"integer"}},
                    "locked":{"type":"boolean"}},"required":["element_ids","locked"]})),
                default_shortcut: None, icon: None, hint: None, requires_selection: false,
                show_in_menu: false, version: 1, activates_tool: None, capability_id: None,
            }, execute_set_object_locked)
            .register_command(CommandDescriptor {
                id: "view.lock_selected".into(),
                label: "Lock Selected".into(),
                description: "Lock the selected objects against viewport interaction. Unlock them from the Outliner.".into(),
                category: CommandCategory::View,
                parameters: None, default_shortcut: None, icon: None, hint: None,
                requires_selection: true, show_in_menu: true, version: 1,
                activates_tool: None, capability_id: None,
            }, execute_lock_selected)
            .add_systems(PreUpdate, refresh_changed_locks);
    }
}

fn execute_lock_selected(
    world: &mut World,
    _: &serde_json::Value,
) -> Result<CommandResult, String> {
    let ids: BTreeSet<_> = world
        .query_filtered::<(Option<&ElementId>, Option<&GeneratedOccurrencePart>), With<Selected>>()
        .iter(world)
        .filter_map(|(id, part)| part.map(|part| part.owner).or(id.copied()))
        .collect();
    execute_set_object_locked(world, &serde_json::json!({"element_ids":ids,"locked":true}))
}

fn refresh_changed_locks(
    state: Res<ObjectLocks>,
    resolved: Res<DocumentLocks>,
    changed: Query<(), Or<(Changed<ElementId>, Changed<SelectionProxy>)>>,
    groups: Query<(Entity, &GroupMembers), Changed<GroupMembers>>,
    mut membership: Local<HashMap<Entity, Vec<ElementId>>>,
    features: Query<(Entity, &FaceProfileFeature), Changed<FaceProfileFeature>>,
    mut feature_parents: Local<HashMap<Entity, ElementId>>,
    mut removed_features: RemovedComponents<FaceProfileFeature>,
    mut removed: RemovedComponents<GroupMembers>,
    mut removed_proxies: RemovedComponents<SelectionProxy>,
    mut commands: Commands,
) {
    let mut topology_changed = false;
    for entity in removed.read() {
        membership.remove(&entity);
        topology_changed = true;
    }
    let removed_proxy = removed_proxies.read().next().is_some();
    for entity in removed_features.read() {
        feature_parents.remove(&entity);
        topology_changed = true;
    }
    if state.locked.is_empty() && resolved.locked.is_empty() {
        membership.clear();
        feature_parents.clear();
        return;
    }
    // Group frames also change during rigid gestures. Only membership changes
    // invalidate inheritance; pointer motion must not rebuild the scene index.
    for (entity, group) in &groups {
        if membership.get(&entity) != Some(&group.member_ids) {
            membership.insert(entity, group.member_ids.clone());
            topology_changed = true;
        }
    }
    for (entity, feature) in &features {
        if feature_parents.get(&entity) != Some(&feature.parent) {
            feature_parents.insert(entity, feature.parent);
            topology_changed = true;
        }
    }
    if state.is_changed() || !changed.is_empty() || topology_changed || removed_proxy {
        commands.queue(refresh_object_locks);
    }
}

/// Rebuild only on lock/topology edits, never on pointer movement. Commands call
/// this synchronously so a lock takes effect before the next input consumer.
pub(crate) fn refresh_object_locks(world: &mut World) {
    let mut pending: Vec<_> = world
        .resource::<ObjectLocks>()
        .locked
        .iter()
        .copied()
        .collect();
    let mut children: HashMap<ElementId, Vec<ElementId>> = HashMap::new();
    for (id, group) in world.query::<(&ElementId, &GroupMembers)>().iter(world) {
        children
            .entry(*id)
            .or_default()
            .extend_from_slice(&group.member_ids);
    }
    // A proxy's visible geometry must not intercept clicks on a locked owner.
    for (id, proxy) in world.query::<(&ElementId, &SelectionProxy)>().iter(world) {
        children.entry(proxy.0).or_default().push(*id);
    }
    // Evaluated face features render their supporting body as well as the operand.
    for (id, feature) in world
        .query::<(&ElementId, &FaceProfileFeature)>()
        .iter(world)
    {
        children.entry(feature.parent).or_default().push(*id);
    }
    let mut locked = HashSet::new();
    while let Some(id) = pending.pop() {
        if locked.insert(id) {
            if let Some(members) = children.get(&id) {
                pending.extend(members);
            }
        }
    }
    if world
        .get_resource::<DocumentLocks>()
        .is_none_or(|state| state.locked != locked)
    {
        world.insert_resource(DocumentLocks { locked });
    }
    let group_depth = world
        .get_resource::<super::modeling::group::GroupEditContext>()
        .and_then(|context| {
            context
                .stack
                .iter()
                .position(|id| world.resource::<DocumentLocks>().locked.contains(id))
        });
    if let Some(depth) = group_depth {
        world
            .resource_mut::<super::modeling::group::GroupEditContext>()
            .stack
            .truncate(depth);
    }
    let occurrence_depth = world
        .get_resource::<super::selection::OccurrenceEditContext>()
        .and_then(|context| {
            context
                .stack
                .iter()
                .position(|id| world.resource::<DocumentLocks>().locked.contains(id))
        });
    if let Some(depth) = occurrence_depth {
        world
            .resource_mut::<super::selection::OccurrenceEditContext>()
            .stack
            .truncate(depth);
    }

    let selected: Vec<_> = world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .filter(|entity| entity_locked(world, *entity))
        .collect();
    for entity in selected {
        world.entity_mut(entity).remove::<Selected>();
    }
    let editing = world
        .get_resource::<super::face_edit::FaceEditContext>()
        .and_then(|context| context.entity);
    if editing.is_some_and(|entity| entity_locked(world, entity)) {
        world
            .resource_mut::<super::face_edit::FaceEditContext>()
            .exit();
        if let Some(mut selection) =
            world.get_resource_mut::<crate::capability_registry::SubobjectSelection>()
        {
            selection.refs.clear();
        }
    }
}

struct SetObjectLocked {
    before: Vec<(ElementId, bool)>,
    locked: bool,
}

impl EditorCommand for SetObjectLocked {
    fn label(&self) -> &'static str {
        if self.locked {
            "Lock objects"
        } else {
            "Unlock objects"
        }
    }
    fn apply(&mut self, world: &mut World) {
        let mut state = world.resource_mut::<ObjectLocks>();
        for (id, _) in &self.before {
            if self.locked {
                state.locked.insert(*id);
            } else {
                state.locked.remove(id);
            }
        }
        refresh_object_locks(world);
    }
    fn undo(&mut self, world: &mut World) {
        let mut state = world.resource_mut::<ObjectLocks>();
        for (id, locked) in &self.before {
            if *locked {
                state.locked.insert(*id);
            } else {
                state.locked.remove(id);
            }
        }
        refresh_object_locks(world);
    }
}

pub fn execute_set_object_locked(
    world: &mut World,
    parameters: &serde_json::Value,
) -> Result<CommandResult, String> {
    let ids: BTreeSet<ElementId> =
        serde_json::from_value(parameters.get("element_ids").cloned().unwrap_or_default())
            .map_err(|_| "element_ids must be an array of object ids")?;
    let locked = parameters
        .get("locked")
        .and_then(|value| value.as_bool())
        .ok_or("locked must be true or false")?;
    if ids.is_empty() {
        return Err("Choose at least one object".into());
    }
    for id in &ids {
        let entity = find_entity_by_element_id_readonly(world, *id)
            .ok_or_else(|| format!("Object {} does not exist", id.0))?;
        if world.get::<LayerVisibilityExempt>(entity).is_some() {
            return Err("Scene infrastructure is not a lockable model object".into());
        }
    }
    let state = world.resource::<ObjectLocks>();
    let before: Vec<_> = ids
        .iter()
        .map(|id| (*id, state.locked.contains(id)))
        .filter(|(_, previous)| *previous != locked)
        .collect();
    if !before.is_empty() {
        world
            .resource_mut::<PendingCommandQueue>()
            .push_command(Box::new(SetObjectLocked { before, locked }));
    }
    Ok(CommandResult {
        output: Some(serde_json::json!({"element_ids":ids,"locked":locked})),
        ..Default::default()
    })
}
