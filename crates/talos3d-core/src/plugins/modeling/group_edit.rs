//! Atomic hierarchy edits shared by menu commands and agent command execution.
use super::group::{compute_group_bounds_from_world, GroupMembers, GroupSnapshot};
use crate::{
    authored_entity::BoxedEntity,
    plugins::{
        authored_edit_plan::{
            capture_snapshot, queue_captured_plan, AuthoredEditPlan, AuthoredEditPlanRegistry,
            PlanContext,
        },
        commands::find_entity_by_element_id_readonly,
        history::{apply_pending_history_commands, History},
        identity::{ElementId, ElementIdAllocator},
        selection::Selected,
    },
    semantics::SemanticPlan,
};
use bevy::prelude::*;
use std::collections::BTreeMap;

fn selected_ids(world: &mut World) -> Vec<ElementId> {
    let mut ids: Vec<_> = world
        .query_filtered::<&ElementId, With<Selected>>()
        .iter(world)
        .copied()
        .collect();
    ids.sort();
    ids
}

fn group_snapshot(world: &World, id: ElementId) -> Result<GroupSnapshot, String> {
    capture_snapshot(world, id)
        .and_then(|s| s.0.as_any().downcast_ref::<GroupSnapshot>().cloned())
        .ok_or_else(|| format!("Group {} is unavailable", id.0))
}

fn commit(
    world: &mut World,
    intent: &str,
    before: Vec<BoxedEntity>,
    after: Vec<BoxedEntity>,
    selection: &[ElementId],
) -> Result<(), String> {
    let base = world.resource::<History>().revision_token();
    let plan = AuthoredEditPlan::capture(
        world,
        None,
        base.clone(),
        PlanContext {
            planner_id: "core.group_membership".into(),
            planner_version: 1,
            request_kind: intent.into(),
            intent: intent.into(),
            ..Default::default()
        },
        before,
        after,
        SemanticPlan::none(),
    )
    .map_err(|e| e.to_string())?;
    let plan = world
        .resource_mut::<AuthoredEditPlanRegistry>()
        .publish(plan)
        .map_err(|e| e.to_string())?;
    queue_captured_plan(world, plan.plan_id()).map_err(|e| e.to_string())?;
    // Finish the ordinary history gate before selecting the result. Applying a
    // speculative entity first lets selection normalization discard it before
    // its containing group's membership has been committed.
    apply_pending_history_commands(world);
    if world.resource::<History>().revision_token().revision != base.revision + 1 {
        return Err("Hierarchy edit was refused".into());
    }
    let old: Vec<_> = world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .collect();
    for entity in old {
        world.entity_mut(entity).remove::<Selected>();
    }
    for id in selection {
        if let Some(entity) = find_entity_by_element_id_readonly(world, *id) {
            world.entity_mut(entity).insert(Selected);
        }
    }
    Ok(())
}

pub(super) fn group_selection(world: &mut World) -> Result<(), String> {
    let ids = selected_ids(world);
    if ids.is_empty() {
        return Err("No entities selected to group".into());
    }
    let parents: Vec<_> = world
        .query::<(&ElementId, &GroupMembers)>()
        .iter(world)
        .filter(|(_, group)| group.member_ids.iter().any(|id| ids.contains(id)))
        .map(|(id, group)| (*id, group.member_ids.clone()))
        .collect();
    if parents.len() > 1
        || parents
            .iter()
            .any(|(_, members)| ids.iter().any(|id| !members.contains(id)))
    {
        return Err("Group entities within the same component".into());
    }
    let group_id = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = GroupSnapshot {
        element_id: group_id,
        name: "Group".into(),
        member_ids: ids.clone(),
        frame: Default::default(),
        composite: None,
        linked_model: None,
        cached_bounds: compute_group_bounds_from_world(world, &ids),
    };
    let mut before = Vec::new();
    let mut after = vec![snapshot.into()];
    if let Some((parent, _)) = parents.first() {
        let old = group_snapshot(world, *parent)?;
        let mut new = old.clone();
        new.member_ids.retain(|id| !ids.contains(id));
        new.member_ids.push(group_id);
        before.push(old.into());
        after.push(new.into());
    }
    use crate::plugins::drafting::authoring::{
        active_draft_membership_target, draft_membership_add_snapshots,
    };
    if let Some((old, new)) = active_draft_membership_target(world)
        .and_then(|id| draft_membership_add_snapshots(world, id, group_id))
    {
        before.push(old);
        after.push(new);
    }
    commit(world, "Group selection", before, after, &[group_id])
}

pub(super) fn ungroup_selection(world: &mut World) -> Result<(), String> {
    let groups: BTreeMap<_, _> = selected_ids(world)
        .into_iter()
        .filter_map(|id| group_snapshot(world, id).ok().map(|group| (id, group)))
        .collect();
    if groups.is_empty() {
        return Err("No groups selected to ungroup".into());
    }
    if groups.values().any(|group| group.linked_model.is_some()) {
        return Err("Cannot ungroup a linked model; open it to edit its contents".into());
    }
    let mut before: Vec<BoxedEntity> = groups.values().cloned().map(Into::into).collect();
    let mut after = Vec::new();
    let parents: Vec<_> = world
        .query::<(&ElementId, &GroupMembers)>()
        .iter(world)
        .filter(|(id, group)| {
            !groups.contains_key(id) && group.member_ids.iter().any(|id| groups.contains_key(id))
        })
        .map(|(id, _)| *id)
        .collect();
    let expand = |members: &[ElementId]| {
        let mut result = Vec::new();
        for id in members {
            let ids = groups
                .get(id)
                .map(|group| group.member_ids.as_slice())
                .unwrap_or(std::slice::from_ref(id));
            for id in ids {
                if !result.contains(id) {
                    result.push(*id);
                }
            }
        }
        result
    };
    for parent in parents {
        let old = group_snapshot(world, parent)?;
        let mut new = old.clone();
        new.member_ids = expand(&old.member_ids);
        before.push(old.into());
        after.push(new.into());
    }
    use crate::plugins::drafting::draft::{DraftNode, DraftSnapshot};
    let drafts: Vec<_> = world
        .query::<(&ElementId, &DraftNode)>()
        .iter(world)
        .filter(|(_, node)| node.members.iter().any(|id| groups.contains_key(id)))
        .map(|(id, node)| DraftSnapshot {
            element_id: *id,
            node: node.clone(),
        })
        .collect();
    for old in drafts {
        let mut new = old.clone();
        new.node.members = expand(&old.node.members);
        before.push(old.into());
        after.push(new.into());
    }
    let selection = expand(&groups.keys().copied().collect::<Vec<_>>());
    commit(world, "Ungroup selection", before, after, &selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authored_entity::AuthoredEntity,
        capability_registry::CapabilityRegistry,
        plugins::{
            history::{PendingCommandQueue, SemanticEnforcement},
            modeling::group::GroupFactory,
        },
    };

    fn snapshot(id: u64, members: &[u64]) -> GroupSnapshot {
        GroupSnapshot {
            element_id: ElementId(id),
            name: format!("Group {id}"),
            member_ids: members.iter().map(|id| ElementId(*id)).collect(),
            frame: Default::default(),
            composite: None,
            linked_model: None,
            cached_bounds: None,
        }
    }

    #[test]
    fn nested_group_and_ungroup_preserve_one_owner_selection_and_atomic_history() {
        let mut world = World::new();
        world.init_resource::<History>();
        world.init_resource::<PendingCommandQueue>();
        world.init_resource::<SemanticEnforcement>();
        world.init_resource::<ElementIdAllocator>();
        world.init_resource::<AuthoredEditPlanRegistry>();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(GroupFactory);
        world.insert_resource(registry);
        let parent = snapshot(100, &[101, 102]);
        parent.apply_to(&mut world);
        for id in [101, 102] {
            snapshot(id, &[]).apply_to(&mut world);
            let entity = find_entity_by_element_id_readonly(&world, ElementId(id)).unwrap();
            world.entity_mut(entity).insert(Selected);
        }
        group_selection(&mut world).unwrap();
        let grouped = selected_ids(&mut world);
        assert_eq!(grouped.len(), 1);
        let group = grouped[0];
        assert_eq!(
            group_snapshot(&world, parent.element_id)
                .unwrap()
                .member_ids,
            vec![group]
        );
        assert_eq!(
            group_snapshot(&world, group).unwrap().member_ids,
            vec![ElementId(101), ElementId(102)]
        );
        assert_eq!(world.resource::<History>().undo_stack_len(), 1);
        world.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands(&mut world);
        assert_eq!(
            group_snapshot(&world, parent.element_id)
                .unwrap()
                .member_ids,
            parent.member_ids
        );
        assert!(find_entity_by_element_id_readonly(&world, group).is_none());
        world.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands(&mut world);
        let entity = find_entity_by_element_id_readonly(&world, group).unwrap();
        world.entity_mut(entity).insert(Selected);
        ungroup_selection(&mut world).unwrap();
        assert_eq!(
            group_snapshot(&world, parent.element_id)
                .unwrap()
                .member_ids,
            parent.member_ids
        );
        assert!(find_entity_by_element_id_readonly(&world, group).is_none());
        assert_eq!(
            selected_ids(&mut world),
            vec![ElementId(101), ElementId(102)]
        );
        assert_eq!(world.resource::<History>().undo_stack_len(), 2);
        world.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands(&mut world);
        assert_eq!(
            group_snapshot(&world, parent.element_id)
                .unwrap()
                .member_ids,
            vec![group]
        );
        assert!(find_entity_by_element_id_readonly(&world, group).is_some());
    }
}
