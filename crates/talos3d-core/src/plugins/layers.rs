use std::collections::{BTreeMap, HashMap, HashSet};

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};

use crate::plugins::{identity::ElementId, modeling::group::GroupMembers};

pub const DEFAULT_LAYER_NAME: &str = "Default";

/// Marker for entities that carry an [`ElementId`] but are scene *infrastructure*
/// (lights, and any future non-drawable helpers) rather than CAD geometry. The
/// generic layer system must never sweep these onto a document layer nor drive
/// their [`Visibility`] from a layer's visible flag — otherwise hiding a layer
/// (e.g. "Default") would set a light to `Visibility::Hidden`, Bevy would drop
/// it from rendering, and the whole scene would collapse to ambient-only light.
/// Infrastructure plugins add this marker to their entities at spawn.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LayerVisibilityExempt;

pub struct LayerPlugin;

impl Plugin for LayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LayerRegistry>()
            .init_resource::<LayerState>()
            // Default fallback runs in Update, strictly after domain plugins
            // (e.g. terrain) have claimed their entities in PreUpdate. Then
            // visibility is applied.
            .add_systems(
                Update,
                (assign_default_layer, apply_layer_visibility).chain(),
            );
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerDef {
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub color: Option<[f32; 4]>,
    pub order: u32,
}

impl LayerDef {
    pub fn new(name: impl Into<String>, order: u32) -> Self {
        Self {
            name: name.into(),
            visible: true,
            locked: false,
            color: None,
            order,
        }
    }
}

#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct LayerRegistry {
    pub layers: BTreeMap<String, LayerDef>,
    next_order: u32,
}

impl Default for LayerRegistry {
    fn default() -> Self {
        let mut layers = BTreeMap::new();
        layers.insert(
            DEFAULT_LAYER_NAME.to_string(),
            LayerDef::new(DEFAULT_LAYER_NAME, 0),
        );
        Self {
            layers,
            next_order: 1,
        }
    }
}

impl LayerRegistry {
    pub fn ensure_layer(&mut self, name: &str) {
        if !self.layers.contains_key(name) {
            self.layers
                .insert(name.to_string(), LayerDef::new(name, self.next_order));
            self.next_order += 1;
        }
    }

    pub fn create_layer(&mut self, name: String) -> &LayerDef {
        self.layers.entry(name.clone()).or_insert_with(|| {
            let def = LayerDef::new(&name, self.next_order);
            self.next_order += 1;
            def
        })
    }

    pub fn generate_unique_name(&self) -> String {
        for i in 1.. {
            let candidate = format!("Layer {i}");
            if !self.layers.contains_key(&candidate) {
                return candidate;
            }
        }
        unreachable!()
    }

    pub fn rename_layer(&mut self, old_name: &str, new_name: String) -> Result<(), String> {
        if old_name == DEFAULT_LAYER_NAME {
            return Err("Cannot rename the Default layer".to_string());
        }
        if self.layers.contains_key(&new_name) {
            return Err(format!("Layer '{new_name}' already exists"));
        }
        if let Some(mut def) = self.layers.remove(old_name) {
            def.name = new_name.clone();
            self.layers.insert(new_name, def);
            Ok(())
        } else {
            Err(format!("Layer '{old_name}' not found"))
        }
    }

    pub fn delete_layer(&mut self, name: &str) -> Result<(), String> {
        if name == DEFAULT_LAYER_NAME {
            return Err("Cannot delete the Default layer".to_string());
        }
        if self.layers.remove(name).is_some() {
            Ok(())
        } else {
            Err(format!("Layer '{name}' not found"))
        }
    }

    pub fn sorted_layers(&self) -> Vec<&LayerDef> {
        let mut layers: Vec<&LayerDef> = self.layers.values().collect();
        layers.sort_by_key(|l| l.order);
        layers
    }

    pub fn subset_for_names<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> LayerRegistry {
        let mut subset = LayerRegistry::default();
        for name in names {
            if name == DEFAULT_LAYER_NAME {
                continue;
            }
            if let Some(def) = self.layers.get(name) {
                subset.layers.insert(name.to_string(), def.clone());
            } else {
                subset.ensure_layer(name);
            }
        }
        subset.next_order = subset
            .layers
            .values()
            .map(|layer| layer.order)
            .max()
            .unwrap_or(0)
            + 1;
        subset
    }

    pub fn is_visible(&self, name: &str) -> bool {
        self.layers.get(name).is_none_or(|l| l.visible)
    }

    pub fn is_locked(&self, name: &str) -> bool {
        self.layers.get(name).is_some_and(|l| l.locked)
    }
}

#[derive(Resource, Debug, Clone)]
pub struct LayerState {
    pub active_layer: String,
}

impl Default for LayerState {
    fn default() -> Self {
        Self {
            active_layer: DEFAULT_LAYER_NAME.to_string(),
        }
    }
}

impl LayerState {
    pub fn set_active(&mut self, name: String, registry: &mut LayerRegistry) {
        registry.ensure_layer(&name);
        if let Some(def) = registry.layers.get_mut(&name) {
            def.visible = true;
            def.locked = false;
        }
        self.active_layer = name;
    }
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerAssignment {
    pub layer: String,
}

impl LayerAssignment {
    pub fn new(name: impl Into<String>) -> Self {
        Self { layer: name.into() }
    }

    pub fn default_layer() -> Self {
        Self {
            layer: DEFAULT_LAYER_NAME.to_string(),
        }
    }
}

/// Ensure every authored entity lives on a layer. Any entity carrying an
/// [`ElementId`] but no [`LayerAssignment`] is placed on the Default layer, so
/// the layer panel can list and toggle it instead of it being invisible to the
/// layer system (e.g. derived terrain-surface meshes, freshly created
/// primitives). Domain plugins may claim entities first by running their own
/// assignment system `before(assign_default_layer)`.
pub fn assign_default_layer(
    mut commands: Commands,
    query: Query<
        Entity,
        (
            With<ElementId>,
            Without<LayerAssignment>,
            Without<LayerVisibilityExempt>,
        ),
    >,
) {
    for entity in &query {
        commands
            .entity(entity)
            .try_insert(LayerAssignment::default_layer());
    }
}

fn apply_layer_visibility(
    registry: Res<LayerRegistry>,
    groups: Query<(&ElementId, &GroupMembers, Option<&LayerAssignment>)>,
    changed: Query<(), Or<(Changed<LayerAssignment>, Changed<GroupMembers>)>>,
    mut removed_groups: RemovedComponents<GroupMembers>,
    mut query: Query<
        (Option<&ElementId>, &LayerAssignment, &mut Visibility),
        Without<LayerVisibilityExempt>,
    >,
) {
    // Imports and reassignment can change membership while the registry stays
    // unchanged. Group membership is semantic, not a Bevy ChildOf hierarchy.
    let removed = removed_groups.read().next().is_some();
    if !registry.is_changed()
        && changed.is_empty()
        && !removed
        && !query
            .iter_mut()
            .any(|(_, _, visibility)| visibility.is_added())
    {
        return;
    }
    let group_index: HashMap<_, _> = groups
        .iter()
        .map(|(id, members, _)| (*id, members))
        .collect();
    let mut pending = Vec::new();
    for (id, _, assignment) in &groups {
        let layer = assignment.map_or(DEFAULT_LAYER_NAME, |assignment| assignment.layer.as_str());
        if !registry.is_visible(layer) {
            pending.push(*id);
        }
    }
    let mut hidden = HashSet::new();
    while let Some(id) = pending.pop() {
        if !hidden.insert(id) {
            continue;
        }
        if let Some(group) = group_index.get(&id) {
            pending.extend_from_slice(&group.member_ids);
        }
    }
    for (id, assignment, mut visibility) in &mut query {
        let target = if registry.is_visible(&assignment.layer)
            && !id.is_some_and(|id| hidden.contains(id))
        {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != target {
            *visibility = target;
        }
    }
}

pub fn entity_layer_name(world: &World, entity: Entity) -> &str {
    world
        .get_entity(entity)
        .ok()
        .and_then(|e| e.get::<LayerAssignment>())
        .map(|a| a.layer.as_str())
        .unwrap_or(DEFAULT_LAYER_NAME)
}

pub fn entity_on_locked_layer(world: &World, entity: Entity) -> bool {
    let registry = world.resource::<LayerRegistry>();
    let layer_name = entity_layer_name(world, entity);
    registry.is_locked(layer_name)
}

pub fn entity_on_visible_layer(world: &World, entity: Entity) -> bool {
    let registry = world.resource::<LayerRegistry>();
    let layer_name = entity_layer_name(world, entity);
    registry.is_visible(layer_name)
}

pub fn count_entities_per_layer(world: &World) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let mut q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in q.iter(world) {
        if entity_ref.get::<ElementId>().is_none() {
            continue;
        }
        let layer = entity_ref
            .get::<LayerAssignment>()
            .map(|a| a.layer.as_str())
            .unwrap_or(DEFAULT_LAYER_NAME);
        *counts.entry(layer.to_string()).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::SystemState;

    #[test]
    fn imported_and_reassigned_geometry_obeys_unchanged_layer_registry() {
        let mut app = App::new();
        app.add_plugins(LayerPlugin);
        app.world_mut()
            .resource_mut::<LayerRegistry>()
            .ensure_layer("Hidden");
        app.world_mut()
            .resource_mut::<LayerRegistry>()
            .layers
            .get_mut("Hidden")
            .unwrap()
            .visible = false;
        app.update();
        app.update();
        let imported = app
            .world_mut()
            .spawn((
                ElementId(1),
                LayerAssignment::new("Hidden"),
                Visibility::Visible,
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(imported),
            Some(&Visibility::Hidden)
        );
        app.world_mut()
            .entity_mut(imported)
            .insert(LayerAssignment::default_layer());
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(imported),
            Some(&Visibility::Inherited)
        );
        app.world_mut()
            .entity_mut(imported)
            .insert(LayerAssignment::new("Hidden"));
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(imported),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn hiding_group_layer_hides_nested_members_and_preserves_child_layer_state() {
        let mut app = App::new();
        app.add_plugins(LayerPlugin);
        for name in ["House", "Roof", "Hidden"] {
            app.world_mut()
                .resource_mut::<LayerRegistry>()
                .ensure_layer(name);
        }
        app.world_mut()
            .resource_mut::<LayerRegistry>()
            .layers
            .get_mut("Hidden")
            .unwrap()
            .visible = false;
        app.world_mut().spawn((
            ElementId(1),
            LayerAssignment::new("House"),
            GroupMembers {
                name: "House".into(),
                member_ids: vec![ElementId(2)],
                frame: Default::default(),
                linked_model: None,
            },
        ));
        app.world_mut().spawn((
            ElementId(2),
            LayerAssignment::new("Roof"),
            GroupMembers {
                name: "Roof".into(),
                member_ids: vec![ElementId(3), ElementId(4)],
                frame: Default::default(),
                linked_model: None,
            },
        ));
        let visible = app
            .world_mut()
            .spawn((
                ElementId(3),
                LayerAssignment::new("Roof"),
                Visibility::Visible,
            ))
            .id();
        let hidden = app
            .world_mut()
            .spawn((
                ElementId(4),
                LayerAssignment::new("Hidden"),
                Visibility::Visible,
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(visible),
            Some(&Visibility::Inherited)
        );
        app.world_mut()
            .resource_mut::<LayerRegistry>()
            .layers
            .get_mut("House")
            .unwrap()
            .visible = false;
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(visible),
            Some(&Visibility::Hidden)
        );
        app.world_mut()
            .resource_mut::<LayerRegistry>()
            .layers
            .get_mut("House")
            .unwrap()
            .visible = true;
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(visible),
            Some(&Visibility::Inherited)
        );
        assert_eq!(
            app.world().get::<Visibility>(hidden),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn exempt_entities_are_never_layer_managed() {
        // Regression: scene lights (marked `LayerVisibilityExempt`) must not be
        // swept onto the Default layer nor have their `Visibility` driven by it.
        // Hiding the Default layer once collapsed the whole scene to ambient-only
        // lighting because the auto-assigned lights went `Visibility::Hidden`.
        let mut app = App::new();
        app.init_resource::<LayerRegistry>()
            .init_resource::<LayerState>()
            .add_systems(
                Update,
                (assign_default_layer, apply_layer_visibility).chain(),
            );

        let light = app
            .world_mut()
            .spawn((ElementId(1), LayerVisibilityExempt, Visibility::Inherited))
            .id();
        let geometry = app
            .world_mut()
            .spawn((ElementId(2), Visibility::Inherited))
            .id();

        app.update();

        // Geometry is claimed by the Default layer; the exempt light is not.
        assert!(app.world().get::<LayerAssignment>(geometry).is_some());
        assert!(app.world().get::<LayerAssignment>(light).is_none());

        // Hide the Default layer and re-run.
        app.world_mut()
            .resource_mut::<LayerRegistry>()
            .layers
            .get_mut(DEFAULT_LAYER_NAME)
            .unwrap()
            .visible = false;
        app.update();

        // Geometry follows the layer (hidden); the light stays lit regardless.
        assert_eq!(
            *app.world().get::<Visibility>(geometry).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            *app.world().get::<Visibility>(light).unwrap(),
            Visibility::Inherited
        );
    }

    #[test]
    fn default_layer_assignment_ignores_entities_despawned_before_commands_apply() {
        let mut world = World::new();
        let entity = world.spawn((ElementId(42), Visibility::Inherited)).id();
        let mut state = SystemState::<(
            Commands,
            Query<
                Entity,
                (
                    With<ElementId>,
                    Without<LayerAssignment>,
                    Without<LayerVisibilityExempt>,
                ),
            >,
        )>::new(&mut world);

        {
            let (commands, query) = state.get_mut(&mut world).expect("system state");
            assign_default_layer(commands, query);
        }
        world.despawn(entity);
        state.apply(&mut world);

        assert!(world.get_entity(entity).is_err());
    }
}
