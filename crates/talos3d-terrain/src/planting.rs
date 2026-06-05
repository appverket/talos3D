//! Reversible "plant on surface" operation (ADR-059, PP-PLANT-D).
//!
//! Domain-neutral platform capability: place a [`ConformingSolid`] hugging
//! foundation under a target object's footprint, raise the object so its base
//! sits on the foundation's flat top (the surface-riding datum), and optionally
//! hide an existing foundation so the operation is fully reversible. `unplant`
//! restores the object, removes the foundation, and un-hides the original.
//!
//! The architecture domain layers foundation naming / recipe discovery on top of
//! this (PP-PLANT-E); the mechanism itself carries no architecture semantics.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use talos3d_capability_api::commands::{
    CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
};
use talos3d_core::{
    authored_entity::AuthoredEntity,
    capability_registry::CapabilityRegistry,
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
    },
};

use crate::{
    conforming::{ConformingSolid, ConformingSolidSnapshot, DEFAULT_MAX_DEPTH, DEFAULT_MIN_THICKNESS},
    heightfield::TerrainHeightfield,
};

/// Link recording how an object was planted, so the operation can be reversed.
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PlantedOnSurface {
    /// The conforming foundation created under the object.
    pub foundation_id: ElementId,
    /// How far the object was raised (subtract to restore).
    pub y_delta: f32,
    /// An original foundation that was hidden (un-hide to restore).
    pub hidden_foundation: Option<ElementId>,
}

pub struct PlantingPlugin;

impl Plugin for PlantingPlugin {
    fn build(&self, app: &mut App) {
        app.register_command(
            CommandDescriptor {
                id: "terrain.plant_on_surface".to_string(),
                label: "Plant On Surface".to_string(),
                description: "Place a terrain-conforming foundation under an object and seat it on \
                              the surface at minimum clearance; optionally hide its original \
                              foundation (reversible)."
                    .to_string(),
                category: CommandCategory::Create,
                parameters: Some(json!({
                    "type": "object",
                    "required": ["target_id", "surface_id"],
                    "properties": {
                        "target_id": {"type": "integer"},
                        "surface_id": {"type": "integer"},
                        "min_thickness": {"type": "number"},
                        "max_depth": {"type": "number"},
                        "resolution": {"type": "number"},
                        "hide_element_id": {"type": "integer"}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.create".to_string()),
                hint: Some("Seat an object on the terrain with a hugging foundation".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_plant_on_surface,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.unplant_on_surface".to_string(),
                label: "Unplant From Surface".to_string(),
                description: "Reverse a plant: remove the hugging foundation, lower the object back, \
                              and un-hide its original foundation."
                    .to_string(),
                category: CommandCategory::Edit,
                parameters: Some(json!({
                    "type": "object",
                    "required": ["target_id"],
                    "properties": { "target_id": {"type": "integer"} }
                })),
                default_shortcut: None,
                icon: Some("icon.edit".to_string()),
                hint: Some("Reverse a plant operation".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_unplant_on_surface,
        );
    }
}

fn required_id(params: &Value, key: &str) -> Result<ElementId, String> {
    params
        .get(key)
        .and_then(Value::as_u64)
        .map(ElementId)
        .ok_or_else(|| format!("missing integer '{key}'"))
}

fn optional_f32(params: &Value, key: &str, default: f32) -> f32 {
    params
        .get(key)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .unwrap_or(default)
}

fn find_heightfield(world: &mut World, surface_id: ElementId) -> Option<TerrainHeightfield> {
    let mut query = world.query::<(&ElementId, &TerrainHeightfield)>();
    query
        .iter(world)
        .find(|(id, _)| id.0 == surface_id.0)
        .map(|(_, hf)| hf.clone())
}

fn capture(world: &World, entity: Entity) -> Option<talos3d_core::authored_entity::BoxedEntity> {
    let entity_ref = world.get_entity(entity).ok()?;
    world
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&entity_ref, world)
}

fn execute_plant_on_surface(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    let target_id = required_id(params, "target_id")?;
    let surface_id = required_id(params, "surface_id")?;
    let min_thickness = optional_f32(params, "min_thickness", DEFAULT_MIN_THICKNESS).max(0.0);
    let max_depth = optional_f32(params, "max_depth", DEFAULT_MAX_DEPTH).max(min_thickness.max(0.01));
    let resolution = optional_f32(params, "resolution", 0.5).max(0.05);
    let hide_id = params.get("hide_element_id").and_then(Value::as_u64).map(ElementId);

    let target_entity =
        find_entity_by_element_id(world, target_id).ok_or_else(|| "target not found".to_string())?;
    if world.get::<PlantedOnSurface>(target_entity).is_some() {
        return Err("target is already planted; unplant first".to_string());
    }

    let target_before = capture(world, target_entity).ok_or_else(|| "cannot capture target".to_string())?;
    let bounds = target_before
        .0
        .bounds()
        .ok_or_else(|| "target has no bounds".to_string())?;
    let center = Vec2::new((bounds.min.x + bounds.max.x) * 0.5, (bounds.min.z + bounds.max.z) * 0.5);
    let half_extents = Vec2::new(
        ((bounds.max.x - bounds.min.x) * 0.5).max(0.05),
        ((bounds.max.z - bounds.min.z) * 0.5).max(0.05),
    );
    let base_y = bounds.min.y;

    let heightfield =
        find_heightfield(world, surface_id).ok_or_else(|| "surface height field not ready".to_string())?;
    let corners = [
        center + Vec2::new(-half_extents.x, -half_extents.y),
        center + Vec2::new(half_extents.x, -half_extents.y),
        center + Vec2::new(half_extents.x, half_extents.y),
        center + Vec2::new(-half_extents.x, half_extents.y),
    ];
    let (max_grade, _) = heightfield
        .max_over(&corners)
        .ok_or_else(|| "footprint does not overlap the surface".to_string())?;
    let y_top = max_grade + min_thickness;
    let y_delta = y_top - base_y;

    // Create the hugging foundation under the footprint.
    let foundation_id = world.resource::<ElementIdAllocator>().next_id();
    ConformingSolidSnapshot {
        element_id: foundation_id,
        derived_top: 0.0,
        solid: ConformingSolid {
            name: "Hugging Foundation".to_string(),
            position: center,
            half_extents,
            yaw: 0.0,
            min_thickness,
            max_depth,
            surface_id,
            resolution,
        },
    }
    .apply_to(world);

    // Seat the object on the foundation's flat top.
    target_before.0.translate_by(Vec3::new(0.0, y_delta, 0.0)).0.apply_to(world);

    // Hide the original foundation (reversible).
    if let Some(hide_id) = hide_id {
        if let Some(entity) = find_entity_by_element_id(world, hide_id) {
            world.entity_mut(entity).insert(Visibility::Hidden);
        }
    }

    world.entity_mut(target_entity).insert(PlantedOnSurface {
        foundation_id,
        y_delta,
        hidden_foundation: hide_id,
    });

    Ok(CommandResult {
        output: Some(json!({
            "foundation_id": foundation_id.0,
            "y_top": y_top,
            "raised_by": y_delta,
        })),
        ..CommandResult::empty()
    })
}

fn execute_unplant_on_surface(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    let target_id = required_id(params, "target_id")?;
    let target_entity =
        find_entity_by_element_id(world, target_id).ok_or_else(|| "target not found".to_string())?;
    let link = world
        .get::<PlantedOnSurface>(target_entity)
        .copied()
        .ok_or_else(|| "target is not planted".to_string())?;

    // Remove the foundation (and free its mesh).
    if let Some(foundation) = find_entity_by_element_id(world, link.foundation_id) {
        let mesh_id = world
            .get_entity(foundation)
            .ok()
            .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh| mesh.id()));
        if let Some(mesh_id) = mesh_id {
            world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
        }
    }
    despawn_by_element_id(world, link.foundation_id);

    // Lower the object back to its original datum.
    if let Some(target_before) = capture(world, target_entity) {
        target_before
            .0
            .translate_by(Vec3::new(0.0, -link.y_delta, 0.0))
            .0
            .apply_to(world);
    }

    // Un-hide the original foundation.
    if let Some(hidden) = link.hidden_foundation {
        if let Some(entity) = find_entity_by_element_id(world, hidden) {
            world.entity_mut(entity).insert(Visibility::Inherited);
        }
    }

    world.entity_mut(target_entity).remove::<PlantedOnSurface>();

    Ok(CommandResult::empty())
}
