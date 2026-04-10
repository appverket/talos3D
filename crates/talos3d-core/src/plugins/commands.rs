use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};

use crate::{
    authored_entity::BoxedEntity,
    capability_registry::CapabilityRegistry,
    plugins::{
        history::{EditorCommand, HistorySet, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        modeling::{
            definition::{Definition, DefinitionRegistry},
            generic_snapshot::PrimitiveSnapshot,
            mesh_generation::NeedsMesh,
            occurrence::ChangedDefinitions,
            primitives::{
                BoxPrimitive, CylinderPrimitive, PlanePrimitive, Polyline, ShapeRotation,
                SpherePrimitive, TriangleMesh,
            },
            snapshots::{PolylineSnapshot, TriangleMeshSnapshot},
        },
    },
};

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CreateBoxCommand>()
            .add_message::<CreateCylinderCommand>()
            .add_message::<CreateSphereCommand>()
            .add_message::<CreatePlaneCommand>()
            .add_message::<CreatePolylineCommand>()
            .add_message::<CreateTriangleMeshCommand>()
            .add_message::<CreateEntityCommand>()
            .add_message::<BeginCommandGroup>()
            .add_message::<EndCommandGroup>()
            .add_message::<DeleteEntitiesCommand>()
            .add_message::<ResolvedDeleteEntitiesCommand>()
            .add_message::<ApplyEntityChangesCommand>()
            .add_systems(
                Update,
                (
                    queue_begin_command_groups,
                    queue_create_entity_commands,
                    queue_create_box_commands,
                    queue_create_cylinder_commands,
                    queue_create_sphere_commands,
                    queue_create_plane_commands,
                    queue_create_polyline_commands,
                    queue_create_triangle_mesh_commands,
                    queue_delete_entities_commands,
                    queue_apply_entity_changes_commands,
                    queue_end_command_groups,
                )
                    .chain()
                    .in_set(HistorySet::Queue),
            );
    }
}

#[derive(Message, Debug, Clone)]
pub struct CreateBoxCommand {
    pub centre: Vec3,
    pub half_extents: Vec3,
}

#[derive(Message, Debug, Clone)]
pub struct CreateCylinderCommand {
    pub centre: Vec3,
    pub radius: f32,
    pub height: f32,
}

#[derive(Message, Debug, Clone)]
pub struct CreateSphereCommand {
    pub centre: Vec3,
    pub radius: f32,
}

#[derive(Message, Debug, Clone)]
pub struct CreatePlaneCommand {
    pub corner_a: Vec2,
    pub corner_b: Vec2,
    pub elevation: f32,
}

#[derive(Message, Debug, Clone)]
pub struct CreatePolylineCommand {
    pub points: Vec<Vec3>,
}

#[derive(Message, Debug, Clone)]
pub struct CreateTriangleMeshCommand {
    pub vertices: Vec<Vec3>,
    pub faces: Vec<[u32; 3]>,
    pub normals: Option<Vec<Vec3>>,
    pub name: Option<String>,
}

#[derive(Message, Debug, Clone)]
pub struct CreateEntityCommand {
    pub snapshot: BoxedEntity,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct BeginCommandGroup {
    pub label: &'static str,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct EndCommandGroup;

#[derive(Message, Debug, Clone)]
pub struct DeleteEntitiesCommand {
    pub element_ids: Vec<ElementId>,
}

#[derive(Message, Debug, Clone)]
pub struct ResolvedDeleteEntitiesCommand {
    pub element_ids: Vec<ElementId>,
}

#[derive(Message, Debug, Clone)]
pub struct ApplyEntityChangesCommand {
    pub label: &'static str,
    pub before: Vec<BoxedEntity>,
    pub after: Vec<BoxedEntity>,
}

struct CreateEntityHistoryCommand {
    snapshot: BoxedEntity,
}

impl EditorCommand for CreateEntityHistoryCommand {
    fn label(&self) -> &'static str {
        match self.snapshot.type_name() {
            "wall" => "Create wall",
            "opening" => "Create opening",
            "box" => "Create box",
            "cylinder" => "Create cylinder",
            "sphere" => "Create sphere",
            "plane" => "Create plane",
            "guide_line" => "Create guide line",
            "dimension_line" => "Create dimension line",
            "profile_extrusion" => "Create profile extrusion",
            "face_profile_feature" => "Create profile feature",
            "profile_sweep" => "Create profile sweep",
            "profile_revolve" => "Create profile revolve",
            "csg" => "Create CSG boolean",
            "polyline" => "Create polyline",
            _ => "Create entity",
        }
    }

    fn apply(&mut self, world: &mut World) {
        self.snapshot.apply_to(world);
    }

    fn undo(&mut self, world: &mut World) {
        self.snapshot.remove_from(world);
    }
}

struct DeleteEntitiesHistoryCommand {
    snapshots: Vec<BoxedEntity>,
}

impl EditorCommand for DeleteEntitiesHistoryCommand {
    fn label(&self) -> &'static str {
        "Delete selection"
    }

    fn apply(&mut self, world: &mut World) {
        for snapshot in &self.snapshots {
            snapshot.remove_from(world);
        }
    }

    fn undo(&mut self, world: &mut World) {
        for snapshot in &self.snapshots {
            snapshot.apply_to(world);
        }
    }
}

struct ModifyEntitiesHistoryCommand {
    label: &'static str,
    before: Vec<BoxedEntity>,
    after: Vec<BoxedEntity>,
}

impl EditorCommand for ModifyEntitiesHistoryCommand {
    fn label(&self) -> &'static str {
        self.label
    }

    fn apply(&mut self, world: &mut World) {
        apply_snapshot_changes(world, &self.after, &self.before);
    }

    fn undo(&mut self, world: &mut World) {
        apply_snapshot_changes(world, &self.before, &self.after);
    }
}

fn apply_snapshot_changes(world: &mut World, target: &[BoxedEntity], previous: &[BoxedEntity]) {
    let previous_by_id: HashMap<_, _> = previous
        .iter()
        .map(|snapshot| (snapshot.element_id(), snapshot))
        .collect();

    for snapshot in target {
        snapshot.apply_with_previous(world, previous_by_id.get(&snapshot.element_id()).copied());
    }
}

fn queue_create_box_commands(world: &mut World) {
    let commands: Vec<CreateBoxCommand> = world
        .resource_mut::<Messages<CreateBoxCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_box(world, command);
    }
}

fn queue_begin_command_groups(world: &mut World) {
    let commands: Vec<BeginCommandGroup> = world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .drain()
        .collect();
    let mut queue = world.resource_mut::<PendingCommandQueue>();
    for command in commands {
        queue.begin_group(command.label);
    }
}

fn queue_create_entity_commands(world: &mut World) {
    let commands: Vec<CreateEntityCommand> = world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_boxed_entity(world, command.snapshot);
    }
}

fn queue_create_cylinder_commands(world: &mut World) {
    let commands: Vec<CreateCylinderCommand> = world
        .resource_mut::<Messages<CreateCylinderCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_cylinder(world, command);
    }
}

fn queue_create_sphere_commands(world: &mut World) {
    let commands: Vec<CreateSphereCommand> = world
        .resource_mut::<Messages<CreateSphereCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_sphere(world, command);
    }
}

fn queue_create_plane_commands(world: &mut World) {
    let commands: Vec<CreatePlaneCommand> = world
        .resource_mut::<Messages<CreatePlaneCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_plane(world, command);
    }
}

fn queue_create_polyline_commands(world: &mut World) {
    let commands: Vec<CreatePolylineCommand> = world
        .resource_mut::<Messages<CreatePolylineCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_polyline(world, command);
    }
}

fn queue_create_triangle_mesh_commands(world: &mut World) {
    let commands: Vec<CreateTriangleMeshCommand> = world
        .resource_mut::<Messages<CreateTriangleMeshCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_create_triangle_mesh(world, command);
    }
}

fn queue_delete_entities_commands(world: &mut World) {
    use crate::plugins::modeling::assembly::{AssemblySnapshot, SemanticAssembly};

    let resolved_from_requests = {
        let commands: Vec<DeleteEntitiesCommand> = world
            .resource_mut::<Messages<DeleteEntitiesCommand>>()
            .drain()
            .collect();
        let registry = world.resource::<CapabilityRegistry>();
        commands
            .into_iter()
            .map(|command| ResolvedDeleteEntitiesCommand {
                element_ids: registry.expand_delete_ids(world, &command.element_ids),
            })
            .collect::<Vec<_>>()
    };
    let mut commands = world
        .resource_mut::<Messages<ResolvedDeleteEntitiesCommand>>()
        .drain()
        .collect::<Vec<_>>();
    commands.extend(resolved_from_requests);

    // Collect all history commands first (immutable borrow), then push them (mutable borrow).
    let history_commands: Vec<Box<dyn EditorCommand>> = commands
        .iter()
        .filter_map(|command| {
            let registry = world.resource::<CapabilityRegistry>();

            let mut snapshots: Vec<BoxedEntity> = command
                .element_ids
                .iter()
                .filter_map(|element_id| {
                    let mut q = world.try_query::<EntityRef>().unwrap();
                    let entity_ref = q.iter(world).find(|entity_ref| {
                        entity_ref.get::<ElementId>().copied() == Some(*element_id)
                    })?;
                    registry.capture_snapshot(&entity_ref, world)
                })
                .collect();

            if snapshots.is_empty() {
                return None;
            }

            snapshots.sort_by_key(snapshot_dependency_order);

            // Capture before/after snapshots of assemblies that will lose members.
            let deleted_ids = &command.element_ids;
            let mut repair_before = Vec::new();
            let mut repair_after = Vec::new();

            let mut q = world.try_query::<EntityRef>().unwrap();
            for entity_ref in q.iter(world) {
                let (Some(eid), Some(assembly)) = (
                    entity_ref.get::<ElementId>(),
                    entity_ref.get::<SemanticAssembly>(),
                ) else {
                    continue;
                };
                if deleted_ids.contains(eid) {
                    continue;
                }
                if assembly
                    .members
                    .iter()
                    .any(|m| deleted_ids.contains(&m.target))
                {
                    if let Some(before) = registry.capture_snapshot(&entity_ref, world) {
                        let mut pruned = assembly.clone();
                        pruned.members.retain(|m| !deleted_ids.contains(&m.target));
                        let after = AssemblySnapshot {
                            element_id: *eid,
                            assembly: pruned,
                        };
                        repair_before.push(before);
                        repair_after.push(BoxedEntity::from(after));
                    }
                }
            }

            if repair_before.is_empty() {
                Some(Box::new(DeleteEntitiesHistoryCommand { snapshots }) as Box<dyn EditorCommand>)
            } else {
                Some(Box::new(DeleteWithAssemblyRepairCommand {
                    delete_snapshots: snapshots,
                    repair_before,
                    repair_after,
                }) as Box<dyn EditorCommand>)
            }
        })
        .collect();

    let mut queue = world.resource_mut::<PendingCommandQueue>();
    for command in history_commands {
        queue.commands.push(command);
    }
}

/// Composite command: delete entities + repair surviving assemblies in one undo step.
struct DeleteWithAssemblyRepairCommand {
    delete_snapshots: Vec<BoxedEntity>,
    repair_before: Vec<BoxedEntity>,
    repair_after: Vec<BoxedEntity>,
}

impl EditorCommand for DeleteWithAssemblyRepairCommand {
    fn label(&self) -> &'static str {
        "Delete selection"
    }

    fn apply(&mut self, world: &mut World) {
        // First apply the assembly repairs (prune stale members).
        apply_snapshot_changes(world, &self.repair_after, &self.repair_before);
        // Then delete the entities.
        for snapshot in &self.delete_snapshots {
            snapshot.remove_from(world);
        }
    }

    fn undo(&mut self, world: &mut World) {
        // First restore deleted entities.
        for snapshot in &self.delete_snapshots {
            snapshot.apply_to(world);
        }
        // Then restore original assembly membership.
        apply_snapshot_changes(world, &self.repair_before, &self.repair_after);
    }
}

fn queue_apply_entity_changes_commands(world: &mut World) {
    let commands: Vec<ApplyEntityChangesCommand> = world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .drain()
        .collect();
    for command in commands {
        enqueue_apply_entity_changes(world, command);
    }
}

fn queue_end_command_groups(world: &mut World) {
    let commands: Vec<EndCommandGroup> = world
        .resource_mut::<Messages<EndCommandGroup>>()
        .drain()
        .collect();
    let mut queue = world.resource_mut::<PendingCommandQueue>();
    for _ in commands {
        queue.end_group();
    }
}

#[cfg(feature = "model-api")]
pub(crate) fn queue_command_events(world: &mut World) {
    queue_begin_command_groups(world);
    queue_create_entity_commands(world);
    queue_create_box_commands(world);
    queue_create_cylinder_commands(world);
    queue_create_sphere_commands(world);
    queue_create_plane_commands(world);
    queue_create_polyline_commands(world);
    queue_create_triangle_mesh_commands(world);
    queue_delete_entities_commands(world);
    queue_apply_entity_changes_commands(world);
    queue_end_command_groups(world);
}

pub(crate) fn enqueue_create_box(world: &mut World, command: CreateBoxCommand) -> ElementId {
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    enqueue_create_boxed_entity(
        world,
        PrimitiveSnapshot {
            element_id,
            primitive: BoxPrimitive {
                centre: command.centre,
                half_extents: command.half_extents,
            },
            rotation: ShapeRotation::default(),
        }
        .into(),
    );
    element_id
}

pub(crate) fn enqueue_create_cylinder(
    world: &mut World,
    command: CreateCylinderCommand,
) -> ElementId {
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    enqueue_create_boxed_entity(
        world,
        PrimitiveSnapshot {
            element_id,
            primitive: CylinderPrimitive {
                centre: command.centre,
                radius: command.radius,
                height: command.height,
            },
            rotation: ShapeRotation::default(),
        }
        .into(),
    );
    element_id
}

pub(crate) fn enqueue_create_sphere(world: &mut World, command: CreateSphereCommand) -> ElementId {
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    enqueue_create_boxed_entity(
        world,
        PrimitiveSnapshot {
            element_id,
            primitive: SpherePrimitive {
                centre: command.centre,
                radius: command.radius,
            },
            rotation: ShapeRotation::default(),
        }
        .into(),
    );
    element_id
}

pub(crate) fn enqueue_create_plane(world: &mut World, command: CreatePlaneCommand) -> ElementId {
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    enqueue_create_boxed_entity(
        world,
        PrimitiveSnapshot {
            element_id,
            primitive: PlanePrimitive {
                corner_a: command.corner_a,
                corner_b: command.corner_b,
                elevation: command.elevation,
            },
            rotation: ShapeRotation::default(),
        }
        .into(),
    );
    element_id
}

pub(crate) fn enqueue_create_polyline(
    world: &mut World,
    command: CreatePolylineCommand,
) -> ElementId {
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    enqueue_create_boxed_entity(
        world,
        PolylineSnapshot {
            element_id,
            primitive: Polyline {
                points: command.points,
            },
            layer: None,
            elevation_metadata: None,
        }
        .into(),
    );
    element_id
}

pub(crate) fn enqueue_create_triangle_mesh(
    world: &mut World,
    command: CreateTriangleMeshCommand,
) -> ElementId {
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    enqueue_create_boxed_entity(
        world,
        TriangleMeshSnapshot {
            element_id,
            primitive: TriangleMesh {
                vertices: command.vertices,
                faces: command.faces,
                normals: command.normals,
                name: command.name,
            },
            layer: None,
        }
        .into(),
    );
    element_id
}

pub fn enqueue_create_boxed_entity(world: &mut World, snapshot: BoxedEntity) {
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(CreateEntityHistoryCommand { snapshot }));
}

pub(crate) fn enqueue_apply_entity_changes(world: &mut World, command: ApplyEntityChangesCommand) {
    if command.before.is_empty() || command.after.is_empty() {
        return;
    }

    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(ModifyEntitiesHistoryCommand {
            label: command.label,
            before: command.before,
            after: command.after,
        }));
}

pub(crate) fn snapshot_dependency_order(snapshot: &BoxedEntity) -> u8 {
    snapshot_dependency_order_by_name(snapshot.type_name())
}

pub fn snapshot_dependency_order_by_name(type_name: &str) -> u8 {
    match type_name {
        "wall" => 0,
        "opening" => 1,
        "face_profile_feature" | "csg" => 3,
        "semantic_assembly" => 4,
        "semantic_relation" => 5,
        _ => 2,
    }
}

pub(crate) fn apply_mesh_primitive<T: Bundle + Clone>(
    world: &mut World,
    element_id: ElementId,
    primitive: T,
    rotation: ShapeRotation,
) {
    if let Some(entity) = find_entity_by_element_id(world, element_id) {
        // Preserve hidden visibility for CSG operands
        let is_csg_operand = world
            .get::<crate::plugins::modeling::csg::CsgOperand>(entity)
            .is_some();
        let is_feature_operand = world
            .get::<crate::plugins::modeling::profile_feature::FeatureOperand>(entity)
            .is_some();
        let visibility = if is_csg_operand || is_feature_operand {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
        world
            .entity_mut(entity)
            .insert((primitive, rotation, NeedsMesh, visibility));
    } else {
        world.spawn((
            element_id,
            primitive,
            rotation,
            NeedsMesh,
            Visibility::Visible,
        ));
    }
}

pub fn despawn_by_element_id(world: &mut World, element_id: ElementId) {
    let Some(entity) = find_entity_by_element_id(world, element_id) else {
        return;
    };

    let mesh_asset_id = world.get::<Mesh3d>(entity).map(|mesh| mesh.id());
    if let Some(mesh_asset_id) = mesh_asset_id {
        world.resource_mut::<Assets<Mesh>>().remove(mesh_asset_id);
    }

    let _ = world.despawn(entity);
}

pub fn find_entity_by_element_id(world: &mut World, element_id: ElementId) -> Option<Entity> {
    let mut query = world.query::<(Entity, &ElementId)>();
    query
        .iter(world)
        .find_map(|(entity, current_id)| (*current_id == element_id).then_some(entity))
}

// ---------------------------------------------------------------------------
// Definition history commands
// ---------------------------------------------------------------------------

struct CreateDefinitionHistoryCommand {
    definition: Definition,
}

impl EditorCommand for CreateDefinitionHistoryCommand {
    fn label(&self) -> &'static str {
        "Create definition"
    }

    fn apply(&mut self, world: &mut World) {
        world
            .resource_mut::<DefinitionRegistry>()
            .insert(self.definition.clone());
    }

    fn undo(&mut self, world: &mut World) {
        world
            .resource_mut::<DefinitionRegistry>()
            .remove(&self.definition.id);
    }
}

struct UpdateDefinitionHistoryCommand {
    before: Definition,
    after: Definition,
}

impl EditorCommand for UpdateDefinitionHistoryCommand {
    fn label(&self) -> &'static str {
        "Update definition"
    }

    fn apply(&mut self, world: &mut World) {
        world
            .resource_mut::<DefinitionRegistry>()
            .insert(self.after.clone());
        world
            .resource_mut::<ChangedDefinitions>()
            .mark_changed(self.after.id.clone());
    }

    fn undo(&mut self, world: &mut World) {
        world
            .resource_mut::<DefinitionRegistry>()
            .insert(self.before.clone());
        world
            .resource_mut::<ChangedDefinitions>()
            .mark_changed(self.before.id.clone());
    }
}

/// Enqueue a create-definition command in the undo history.
pub fn enqueue_create_definition(world: &mut World, definition: Definition) {
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(CreateDefinitionHistoryCommand { definition }));
}

/// Enqueue an update-definition command (one undo step for all N occurrences).
pub fn enqueue_update_definition(world: &mut World, before: Definition, after: Definition) {
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(UpdateDefinitionHistoryCommand { before, after }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_registry::CapabilityRegistry,
        plugins::{
            history::{apply_pending_history_commands, History},
            modeling::{primitives::Polyline, snapshots::PolylineFactory},
        },
    };

    #[test]
    fn delete_entities_command_is_resolved_and_applied() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PolylineFactory);
        world.insert_resource(registry);
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(History::default());
        world.insert_resource(Messages::<DeleteEntitiesCommand>::default());
        world.insert_resource(Messages::<ResolvedDeleteEntitiesCommand>::default());
        world.insert_resource(Assets::<Mesh>::default());

        let element_id = ElementId(42);
        world.spawn((
            element_id,
            Polyline {
                points: vec![Vec3::ZERO, Vec3::X],
            },
        ));

        world
            .resource_mut::<Messages<DeleteEntitiesCommand>>()
            .write(DeleteEntitiesCommand {
                element_ids: vec![element_id],
            });

        queue_delete_entities_commands(&mut world);
        apply_pending_history_commands(&mut world);

        assert!(find_entity_by_element_id(&mut world, element_id).is_none());
    }
}
