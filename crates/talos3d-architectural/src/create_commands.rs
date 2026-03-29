use bevy::prelude::*;
use talos3d_core::plugins::{
    commands::enqueue_create_boxed_entity,
    history::HistorySet,
    identity::{ElementId, ElementIdAllocator},
};

use crate::{
    components::{BimData, Opening, OpeningKind, Wall},
    snapshots::{OpeningSnapshot, WallSnapshot},
};

#[derive(Message, Debug, Clone, Copy)]
pub struct CreateWallCommand {
    pub start: Vec2,
    pub end: Vec2,
    pub height: f32,
    pub thickness: f32,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct CreateOpeningCommand {
    pub parent_wall_element_id: ElementId,
    pub width: f32,
    pub height: f32,
    pub sill_height: f32,
    pub kind: OpeningKind,
    pub position_along_wall: f32,
}

pub struct ArchitecturalCreateCommandPlugin;

impl Plugin for ArchitecturalCreateCommandPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CreateWallCommand>()
            .add_message::<CreateOpeningCommand>()
            .add_systems(
                Update,
                (queue_create_wall_commands, queue_create_opening_commands)
                    .in_set(HistorySet::Queue),
            );
    }
}

fn queue_create_wall_commands(world: &mut World) {
    let commands: Vec<CreateWallCommand> = world
        .resource_mut::<Messages<CreateWallCommand>>()
        .drain()
        .collect();
    for command in commands {
        let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
        enqueue_create_boxed_entity(
            world,
            WallSnapshot {
                element_id,
                wall: Wall {
                    start: command.start,
                    end: command.end,
                    height: command.height,
                    thickness: command.thickness,
                },
                bim_data: BimData::default(),
            }
            .into(),
        );
    }
}

fn queue_create_opening_commands(world: &mut World) {
    let commands: Vec<CreateOpeningCommand> = world
        .resource_mut::<Messages<CreateOpeningCommand>>()
        .drain()
        .collect();
    for command in commands {
        let parent_wall = {
            let mut query = world.query::<(&ElementId, &Wall)>();
            query
                .iter(world)
                .find(|(element_id, _)| **element_id == command.parent_wall_element_id)
                .map(|(_, wall)| wall.clone())
        };
        let Some(parent_wall) = parent_wall else {
            continue;
        };

        let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
        enqueue_create_boxed_entity(
            world,
            OpeningSnapshot {
                element_id,
                opening: Opening {
                    width: command.width,
                    height: command.height,
                    sill_height: command.sill_height,
                    kind: command.kind,
                },
                parent_wall,
                parent_wall_element_id: command.parent_wall_element_id,
                position_along_wall: command.position_along_wall,
                bim_data: BimData::default(),
            }
            .into(),
        );
    }
}
