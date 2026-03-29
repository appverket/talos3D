use std::collections::{HashMap, HashSet};

use bevy::{ecs::world::EntityRef, prelude::*};

use talos3d_core::plugins::{
    commands::{DeleteEntitiesCommand, ResolvedDeleteEntitiesCommand},
    history::HistorySet,
    identity::ElementId,
    modeling::mesh_generation::NeedsMesh,
};

use crate::components::{Opening, ParentWall, Wall};

pub struct ArchitecturalRulesPlugin;

impl Plugin for ArchitecturalRulesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OpeningParentLookup>()
            .add_systems(
                Update,
                expand_wall_delete_dependencies.before(HistorySet::Queue),
            )
            .add_systems(
                Update,
                (
                    mark_walls_dirty_for_changed_openings,
                    mark_walls_dirty_for_removed_openings,
                    refresh_opening_parent_lookup,
                )
                    .chain()
                    .after(HistorySet::Apply),
            );
    }
}

#[derive(Resource, Default)]
struct OpeningParentLookup {
    opening_to_wall: HashMap<Entity, Entity>,
}

type ChangedOpeningQuery<'w, 's> =
    Query<'w, 's, &'static ParentWall, Or<(Added<Opening>, Changed<Opening>, Changed<ParentWall>)>>;

pub(crate) fn expand_wall_delete_dependencies(world: &mut World) {
    let delete_commands: Vec<DeleteEntitiesCommand> = world
        .resource_mut::<Messages<DeleteEntitiesCommand>>()
        .drain()
        .collect();
    let mut resolved_commands = Vec::new();
    for command in delete_commands {
        let expanded_ids = expanded_delete_ids(world, &command.element_ids);
        if !expanded_ids.is_empty() {
            resolved_commands.push(ResolvedDeleteEntitiesCommand {
                element_ids: expanded_ids,
            });
        }
    }

    let mut resolved_events = world.resource_mut::<Messages<ResolvedDeleteEntitiesCommand>>();
    for command in resolved_commands {
        resolved_events.write(command);
    }
}

pub(crate) fn expanded_delete_ids(world: &World, element_ids: &[ElementId]) -> Vec<ElementId> {
    let mut __q = world.try_query::<EntityRef>().unwrap();
    let walls: Vec<(Entity, ElementId)> = __q
        .iter(world)
        .filter_map(|entity_ref| {
            Some((entity_ref.id(), *entity_ref.get::<ElementId>()?))
                .filter(|_| entity_ref.contains::<Wall>())
        })
        .collect();
    let mut __q2 = world.try_query::<EntityRef>().unwrap();
    let openings: Vec<(ElementId, ParentWall)> = __q2
        .iter(world)
        .filter_map(|entity_ref| {
            Some((
                *entity_ref.get::<ElementId>()?,
                entity_ref.get::<ParentWall>()?.clone(),
            ))
            .filter(|_| entity_ref.contains::<Opening>())
        })
        .collect();

    let mut expanded_ids = Vec::new();
    let mut seen_ids = HashSet::new();
    let wall_entities_by_id: HashMap<ElementId, Entity> =
        walls.into_iter().map(|(entity, id)| (id, entity)).collect();

    for element_id in element_ids {
        if seen_ids.insert(*element_id) {
            expanded_ids.push(*element_id);
        }

        let Some(wall_entity) = wall_entities_by_id.get(element_id).copied() else {
            continue;
        };

        for (opening_id, parent_wall) in &openings {
            if parent_wall.wall_entity == wall_entity && seen_ids.insert(*opening_id) {
                expanded_ids.push(*opening_id);
            }
        }
    }

    expanded_ids
}

fn mark_walls_dirty_for_changed_openings(
    mut commands: Commands,
    changed_openings: ChangedOpeningQuery,
) {
    for parent_wall in &changed_openings {
        commands.entity(parent_wall.wall_entity).insert(NeedsMesh);
    }
}

fn mark_walls_dirty_for_removed_openings(
    mut commands: Commands,
    mut removed_parent_walls: RemovedComponents<ParentWall>,
    opening_parent_lookup: Res<OpeningParentLookup>,
    walls: Query<(), With<Wall>>,
) {
    for opening_entity in removed_parent_walls.read() {
        let Some(parent_wall_entity) = opening_parent_lookup.opening_to_wall.get(&opening_entity)
        else {
            continue;
        };

        if walls.contains(*parent_wall_entity) {
            commands.entity(*parent_wall_entity).insert(NeedsMesh);
        }
    }
}

fn refresh_opening_parent_lookup(
    mut opening_parent_lookup: ResMut<OpeningParentLookup>,
    openings: Query<(Entity, &ParentWall), With<Opening>>,
) {
    opening_parent_lookup.opening_to_wall.clear();
    opening_parent_lookup.opening_to_wall.extend(
        openings
            .iter()
            .map(|(entity, parent_wall)| (entity, parent_wall.wall_entity)),
    );
}
