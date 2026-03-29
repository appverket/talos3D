use std::collections::BTreeMap;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};

use crate::plugins::identity::ElementId;

pub const DEFAULT_LAYER_NAME: &str = "Default";

pub struct LayerPlugin;

impl Plugin for LayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LayerRegistry>()
            .init_resource::<LayerState>()
            .add_systems(Update, apply_layer_visibility);
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

    pub fn is_visible(&self, name: &str) -> bool {
        self.layers.get(name).map_or(true, |l| l.visible)
    }

    pub fn is_locked(&self, name: &str) -> bool {
        self.layers.get(name).map_or(false, |l| l.locked)
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

fn apply_layer_visibility(
    registry: Res<LayerRegistry>,
    mut query: Query<(&LayerAssignment, &mut Visibility)>,
) {
    if !registry.is_changed() {
        return;
    }
    for (assignment, mut visibility) in &mut query {
        let target = if registry.is_visible(&assignment.layer) {
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
