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

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use talos3d_capability_api::commands::{
    CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
};
use talos3d_core::{
    authored_entity::{AuthoredEntity, BoxedEntity, EntityBounds},
    capability_registry::CapabilityRegistry,
    plugins::{
        commands::{
            despawn_by_element_id, find_entity_by_element_id, find_entity_by_element_id_readonly,
        },
        history::HistorySet,
        identity::{ElementId, ElementIdAllocator},
        modeling::assembly::{AssemblyMemberRef, AssemblySnapshot, SemanticAssembly},
        modeling::generic_snapshot::PrimitiveSnapshot,
        modeling::group::{
            collect_group_members_recursive, group_frame, group_frame_change_snapshots,
            group_membership_add_snapshots, is_group, remove_member_from_groups, GroupMembers,
        },
        modeling::primitives::{BoxPrimitive, ShapeRotation, TriangleMesh},
        modeling::snapshots::TriangleMeshSnapshot,
        selection::Selected,
        transform::{TransformMode, TransformPreviewModifiers, TransformState},
        ui::StatusBarData,
    },
};

use crate::{
    conforming::{
        build_conforming_triangle_mesh, conforming_metrics, ConformingSolid,
        ConformingSolidSnapshot, DEFAULT_MAX_DEPTH, DEFAULT_MIN_THICKNESS,
    },
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

/// Semantic/behavioral aggregate for a structure planted on terrain.
///
/// This is deliberately stronger than group membership: the foundation is a
/// terrain-conforming member, and the superstructure's base datum is re-seated
/// to the foundation top whenever the aggregate moves across grade.
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PlantedStructure {
    /// Semantic structure assembly created for prompt/refinement targeting.
    pub structure_id: ElementId,
    /// Semantic foundation-system assembly nested under the structure.
    #[serde(default)]
    pub foundation_structure_id: Option<ElementId>,
    /// The conforming foundation that derives from the terrain.
    pub foundation_id: ElementId,
    /// Terrain surface sampled by the foundation.
    pub surface_id: ElementId,
    /// Superstructure base offset from the conforming foundation top.
    pub base_offset_from_foundation_top: f32,
}

pub struct PlantingPlugin;

impl Plugin for PlantingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TransformPreviewModifiers>();
        app.world_mut()
            .resource_mut::<TransformPreviewModifiers>()
            .register(adjust_planted_structure_transform_preview);

        app.register_command(
            CommandDescriptor {
                id: "terrain.plant_structure".to_string(),
                label: "Plant Structure".to_string(),
                description:
                    "Establish the planting contract between a semantic structure and terrain: \
                              convert the structure's bottom foundation body into an adaptive \
                              terrain-conforming foundation and seat the superstructure on it."
                        .to_string(),
                category: CommandCategory::Edit,
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "structure_id": {"type": "integer"},
                        "surface_id": {"type": "integer"},
                        "terrain_id": {"type": "integer"},
                        "foundation_id": {"type": "integer"},
                        "min_thickness": {"type": "number"},
                        "max_depth": {"type": "number"},
                        "resolution": {"type": "number"}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.edit".to_string()),
                hint: Some(
                    "Select one semantic structure and one terrain surface, then Plant Structure"
                        .to_string(),
                ),
                requires_selection: true,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_plant_structure,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.plant_on_surface".to_string(),
                label: "Plant On Surface".to_string(),
                description:
                    "Place a terrain-conforming foundation under an object and seat it on \
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
                id: "terrain.release_planted_structure".to_string(),
                label: "Release Planted Structure".to_string(),
                description:
                    "Detach a planted structure from terrain-following behavior while leaving the \
                              current foundation and superstructure in place."
                        .to_string(),
                category: CommandCategory::Edit,
                parameters: Some(json!({
                    "type": "object",
                    "required": ["target_id"],
                    "properties": { "target_id": {"type": "integer"} }
                })),
                default_shortcut: None,
                icon: Some("icon.edit".to_string()),
                hint: Some("Stop a structure from re-seating to terrain when moved".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_release_planted_structure,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.demote_conforming_foundation".to_string(),
                label: "Demote Conforming Foundation".to_string(),
                description:
                    "Freeze an adaptive terrain-conforming foundation body as either a static \
                              snapshot mesh or a conservative max-height box."
                        .to_string(),
                category: CommandCategory::Edit,
                parameters: Some(json!({
                    "type": "object",
                    "required": ["target_id"],
                    "properties": {
                        "target_id": {"type": "integer"},
                        "mode": {
                            "type": "string",
                            "enum": ["snapshot", "max_height_box"],
                            "default": "snapshot"
                        }
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.edit".to_string()),
                hint: Some(
                    "Replace terrain-adaptive foundation behavior with fixed geometry".to_string(),
                ),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_demote_conforming_foundation,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.unplant_on_surface".to_string(),
                label: "Unplant From Surface".to_string(),
                description:
                    "Reverse a plant: remove the hugging foundation, lower the object back, \
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
        )
        .add_systems(Update, reseat_planted_structures.after(HistorySet::Apply));
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

fn optional_id(params: &Value, key: &str) -> Option<ElementId> {
    params.get(key).and_then(Value::as_u64).map(ElementId)
}

fn set_plant_structure_status(world: &mut World, message: impl Into<String>) {
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        let message = message.into();
        status.hint = message.clone();
        status.set_feedback(message, 3.0);
    }
}

fn selected_element_ids(world: &mut World) -> Vec<ElementId> {
    let mut query = world.query::<(&ElementId, Option<&Selected>)>();
    query
        .iter(world)
        .filter_map(|(id, selected)| selected.is_some().then_some(*id))
        .collect()
}

fn has_heightfield(world: &mut World, surface_id: ElementId) -> bool {
    let mut query = world.query::<(&ElementId, &TerrainHeightfield)>();
    query.iter(world).any(|(id, _)| *id == surface_id)
}

fn find_heightfield(world: &mut World, surface_id: ElementId) -> Option<TerrainHeightfield> {
    let mut query = world.query::<(&ElementId, &TerrainHeightfield)>();
    query
        .iter(world)
        .find(|(id, _)| id.0 == surface_id.0)
        .map(|(_, hf)| hf.clone())
}

fn find_heightfield_readonly(world: &World, surface_id: ElementId) -> Option<TerrainHeightfield> {
    let mut query = world.try_query::<(&ElementId, &TerrainHeightfield)>()?;
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

fn capture_by_id(world: &World, element_id: ElementId) -> Option<BoxedEntity> {
    let entity = find_entity_by_element_id_readonly(world, element_id)?;
    capture(world, entity)
}

fn semantic_assembly_by_id(world: &mut World, element_id: ElementId) -> Option<SemanticAssembly> {
    let entity = find_entity_by_element_id(world, element_id)?;
    world.get::<SemanticAssembly>(entity).cloned()
}

fn is_foundation_body(world: &mut World, element_id: ElementId) -> bool {
    let Some(entity) = find_entity_by_element_id(world, element_id) else {
        return false;
    };
    world.get::<ConformingSolid>(entity).is_some()
        || world.get::<BoxPrimitive>(entity).is_some()
        || world.get::<TriangleMesh>(entity).is_some()
}

fn structure_ids_referencing_target(world: &mut World, target_id: ElementId) -> Vec<ElementId> {
    let mut query = world.query::<(&ElementId, &SemanticAssembly)>();
    query
        .iter(world)
        .filter(|(_, assembly)| {
            assembly.assembly_type == "structure"
                && assembly
                    .members
                    .iter()
                    .any(|member| member.target == target_id)
        })
        .map(|(id, _)| *id)
        .collect()
}

fn selected_structure_candidates(world: &mut World, selected_ids: &[ElementId]) -> Vec<ElementId> {
    let mut candidates = Vec::new();
    for selected_id in selected_ids {
        if semantic_assembly_by_id(world, *selected_id)
            .is_some_and(|assembly| assembly.assembly_type == "structure")
        {
            candidates.push(*selected_id);
            continue;
        }
        for structure_id in structure_ids_referencing_target(world, *selected_id) {
            if !candidates.contains(&structure_id) {
                candidates.push(structure_id);
            }
        }
    }
    candidates
}

fn selected_surface_candidates(world: &mut World, selected_ids: &[ElementId]) -> Vec<ElementId> {
    selected_ids
        .iter()
        .copied()
        .filter(|id| has_heightfield(world, *id))
        .collect()
}

fn plant_structure_usage() -> String {
    "Plant Structure: select exactly one semantic structure (or its referenced building group) \
     and one terrain surface, then run Plant Structure; or invoke with structure_id and surface_id."
        .to_string()
}

fn resolve_plant_structure_args(
    world: &mut World,
    params: &Value,
) -> Result<(ElementId, ElementId), String> {
    let selected_ids = selected_element_ids(world);
    let structure_id = optional_id(params, "structure_id").map(Ok).unwrap_or_else(|| {
        let candidates = selected_structure_candidates(world, &selected_ids);
        match candidates.as_slice() {
            [id] => Ok(*id),
            [] => Err(plant_structure_usage()),
            _ => Err(format!(
                "Plant Structure: selection contains multiple structure candidates {:?}; pass structure_id explicitly.",
                candidates.iter().map(|id| id.0).collect::<Vec<_>>()
            )),
        }
    })?;
    let surface_id = optional_id(params, "surface_id")
        .or_else(|| optional_id(params, "terrain_id"))
        .map(Ok)
        .unwrap_or_else(|| {
            let candidates = selected_surface_candidates(world, &selected_ids);
            match candidates.as_slice() {
                [id] => Ok(*id),
                [] => Err(plant_structure_usage()),
                _ => Err(format!(
                    "Plant Structure: selection contains multiple terrain surfaces {:?}; pass surface_id explicitly.",
                    candidates.iter().map(|id| id.0).collect::<Vec<_>>()
                )),
            }
        })?;
    if semantic_assembly_by_id(world, structure_id)
        .is_none_or(|assembly| assembly.assembly_type != "structure")
    {
        return Err(format!(
            "Plant Structure: structure_id {} is not a semantic structure assembly.",
            structure_id.0
        ));
    }
    if !has_heightfield(world, surface_id) {
        return Err(format!(
            "Plant Structure: surface_id {} is not a terrain surface with a heightfield.",
            surface_id.0
        ));
    }
    Ok((structure_id, surface_id))
}

fn translated_target_snapshots(
    world: &World,
    target_id: ElementId,
    delta: Vec3,
) -> Vec<BoxedEntity> {
    if !is_group(world, target_id) {
        return capture_by_id(world, target_id)
            .map(|snapshot| vec![snapshot.translate_by(delta)])
            .unwrap_or_default();
    }

    let member_ids = collect_group_members_recursive(world, target_id);
    let mut translated = member_ids
        .iter()
        .copied()
        .filter(|member_id| !is_group(world, *member_id))
        .filter_map(|member_id| capture_by_id(world, member_id))
        .map(|snapshot| snapshot.translate_by(delta))
        .collect::<Vec<_>>();

    let mut group_ids = vec![target_id];
    group_ids.extend(
        member_ids
            .iter()
            .copied()
            .filter(|member_id| is_group(world, *member_id)),
    );
    for group_id in group_ids {
        let mut new_frame = group_frame(world, group_id).unwrap_or_default();
        new_frame.translation += delta;
        if let Some((_, after)) = group_frame_change_snapshots(world, group_id, new_frame) {
            translated.push(after);
        }
    }

    translated
}

fn translate_target(world: &mut World, target_id: ElementId, delta: Vec3) {
    for snapshot in translated_target_snapshots(world, target_id, delta) {
        snapshot.apply_to(world);
    }
}

fn group_member_ids(world: &World, group_id: ElementId) -> Vec<ElementId> {
    find_entity_by_element_id_readonly(world, group_id)
        .and_then(|entity| world.get::<GroupMembers>(entity))
        .map(|members| members.member_ids.clone())
        .unwrap_or_default()
}

fn member_bounds_excluding(
    world: &World,
    root_ids: &[ElementId],
    excluded: ElementId,
) -> Option<EntityBounds> {
    let registry = world.resource::<CapabilityRegistry>();
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;
    let mut stack = root_ids.to_vec();
    while let Some(id) = stack.pop() {
        if id == excluded {
            continue;
        }
        let Some(entity) = find_entity_by_element_id_readonly(world, id) else {
            continue;
        };
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        if let Some(members) = entity_ref.get::<GroupMembers>() {
            stack.extend(members.member_ids.iter().copied());
            continue;
        }
        if let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) {
            if let Some(bounds) = snapshot.bounds() {
                min = min.min(bounds.min);
                max = max.max(bounds.max);
                any = true;
            } else if let Some(aabb) = entity_ref.get::<bevy::camera::primitives::Aabb>() {
                let center = Vec3::from(aabb.center);
                let half = Vec3::from(aabb.half_extents);
                min = min.min(center - half);
                max = max.max(center + half);
                any = true;
            }
        }
    }
    any.then_some(EntityBounds { min, max })
}

fn translate_members_excluding(
    world: &mut World,
    root_ids: &[ElementId],
    excluded: ElementId,
    delta: Vec3,
) {
    for member_id in root_ids {
        if *member_id == excluded {
            continue;
        }
        translate_target(world, *member_id, delta);
    }
}

fn foundation_top(
    world: &mut World,
    foundation_id: ElementId,
    surface_id: ElementId,
) -> Option<f32> {
    let foundation_entity = find_entity_by_element_id(world, foundation_id)?;
    let solid = world.get::<ConformingSolid>(foundation_entity)?.clone();
    let heightfield = find_heightfield(world, surface_id)?;
    let (max_grade, _) = heightfield.max_over(&solid.world_corners())?;
    Some(max_grade + solid.min_thickness)
}

fn preview_foundation_top(
    world: &World,
    foundation: &BoxedEntity,
    surface_id: ElementId,
) -> Option<f32> {
    if foundation.type_name() != "conforming_solid" {
        return None;
    }
    let foundation =
        serde_json::from_value::<ConformingSolidSnapshot>(foundation.to_json()).ok()?;
    let heightfield = find_heightfield_readonly(world, surface_id)?;
    conforming_metrics(&foundation.solid, &heightfield).map(|metrics| metrics.y_top)
}

fn preview_member_bounds(
    after: &[BoxedEntity],
    member_ids: &HashSet<ElementId>,
) -> Option<EntityBounds> {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;
    for snapshot in after {
        if !member_ids.contains(&snapshot.element_id()) || snapshot.type_name() == "group" {
            continue;
        }
        if let Some(bounds) = snapshot.bounds() {
            min = min.min(bounds.min);
            max = max.max(bounds.max);
            any = true;
        }
    }
    any.then_some(EntityBounds { min, max })
}

fn direct_preview_delta(
    initial_by_id: &HashMap<ElementId, &BoxedEntity>,
    after: &[BoxedEntity],
    member_ids: &HashSet<ElementId>,
) -> Option<Vec3> {
    after.iter().find_map(|snapshot| {
        let id = snapshot.element_id();
        let before = initial_by_id.get(&id)?;
        member_ids
            .contains(&id)
            .then_some(snapshot.center() - before.center())
    })
}

fn ensure_planted_members_in_preview(
    world: &World,
    after: &mut Vec<BoxedEntity>,
    member_ids: &[ElementId],
    direct_delta: Vec3,
) {
    let mut after_ids = after
        .iter()
        .map(BoxedEntity::element_id)
        .collect::<HashSet<_>>();
    for member_id in member_ids {
        if after_ids.contains(member_id) || is_group(world, *member_id) {
            continue;
        }
        if let Some(snapshot) = capture_by_id(world, *member_id) {
            after.push(snapshot.translate_by(direct_delta));
            after_ids.insert(*member_id);
        }
    }
}

fn adjust_planted_structure_transform_preview(
    world: &World,
    state: &TransformState,
    after: &mut Vec<BoxedEntity>,
) {
    if state.mode != TransformMode::Moving || after.is_empty() {
        return;
    }

    let initial_by_id = state
        .initial_snapshots
        .iter()
        .map(|(_, snapshot)| (snapshot.element_id(), snapshot))
        .collect::<HashMap<_, _>>();
    let planted = {
        let mut query = match world.try_query::<(&ElementId, &PlantedStructure)>() {
            Some(query) => query,
            None => return,
        };
        query
            .iter(world)
            .map(|(id, planted)| (*id, *planted))
            .collect::<Vec<_>>()
    };

    for (group_id, planted) in planted {
        let member_ids = collect_group_members_recursive(world, group_id);
        if member_ids.is_empty() {
            continue;
        }
        let member_set = member_ids.iter().copied().collect::<HashSet<_>>();
        let Some(direct_delta) = direct_preview_delta(&initial_by_id, after, &member_set) else {
            continue;
        };

        ensure_planted_members_in_preview(world, after, &member_ids, direct_delta);

        let mut member_set_without_foundation = member_set.clone();
        member_set_without_foundation.remove(&planted.foundation_id);
        let Some(bounds) = preview_member_bounds(after, &member_set_without_foundation) else {
            continue;
        };
        let Some(foundation) = after
            .iter()
            .find(|snapshot| snapshot.element_id() == planted.foundation_id)
        else {
            continue;
        };
        let Some(top) = preview_foundation_top(world, foundation, planted.surface_id) else {
            continue;
        };
        let delta_y = top + planted.base_offset_from_foundation_top - bounds.min.y;
        if delta_y.abs() <= 0.001 {
            continue;
        }
        let vertical_delta = Vec3::new(0.0, delta_y, 0.0);
        for snapshot in after.iter_mut() {
            let id = snapshot.element_id();
            if id != planted.foundation_id && member_set.contains(&id) {
                *snapshot = snapshot.translate_by(vertical_delta);
            }
        }
    }
}

fn translate_group_frame(world: &mut World, group_id: ElementId, delta: Vec3) {
    let mut new_frame = group_frame(world, group_id).unwrap_or_default();
    new_frame.translation += delta;
    if let Some((_, after)) = group_frame_change_snapshots(world, group_id, new_frame) {
        after.apply_to(world);
    }
}

fn foundation_footprint_from_body(
    world: &mut World,
    foundation_body_id: ElementId,
) -> Result<(Vec2, Vec2, f32), String> {
    if let Some(entity) = find_entity_by_element_id(world, foundation_body_id) {
        if let Some(solid) = world.get::<ConformingSolid>(entity) {
            return Ok((solid.position, solid.half_extents, solid.yaw));
        }
    }
    let bounds = capture_by_id(world, foundation_body_id)
        .and_then(|snapshot| snapshot.0.bounds())
        .ok_or_else(|| {
            format!(
                "Plant Structure: foundation object {} has no measurable footprint.",
                foundation_body_id.0
            )
        })?;
    Ok((
        Vec2::new(
            (bounds.min.x + bounds.max.x) * 0.5,
            (bounds.min.z + bounds.max.z) * 0.5,
        ),
        Vec2::new(
            ((bounds.max.x - bounds.min.x) * 0.5).max(0.05),
            ((bounds.max.z - bounds.min.z) * 0.5).max(0.05),
        ),
        0.0,
    ))
}

fn replace_foundation_with_conforming_body(
    world: &mut World,
    foundation_body_id: ElementId,
    surface_id: ElementId,
    min_thickness: f32,
    max_depth: f32,
    resolution: f32,
) -> Result<(), String> {
    let (position, half_extents, yaw) = foundation_footprint_from_body(world, foundation_body_id)?;
    remove_mesh_asset_for_id(world, foundation_body_id);
    despawn_by_element_id(world, foundation_body_id);
    ConformingSolidSnapshot {
        element_id: foundation_body_id,
        derived_top: 0.0,
        solid: ConformingSolid {
            name: "Terrain-conforming foundation".to_string(),
            position,
            half_extents,
            yaw,
            min_thickness,
            max_depth,
            surface_id,
            resolution,
            floor_datum: None,
        },
    }
    .apply_to(world);
    Ok(())
}

fn create_foundation_structure_assembly(
    world: &mut World,
    foundation_id: ElementId,
    surface_id: ElementId,
) -> ElementId {
    let foundation_structure_id = world.resource::<ElementIdAllocator>().next_id();
    AssemblySnapshot {
        element_id: foundation_structure_id,
        assembly: SemanticAssembly {
            assembly_type: "foundation_system".to_string(),
            label: "Terrain-conforming foundation system".to_string(),
            members: vec![AssemblyMemberRef {
                target: foundation_id,
                role: "terrain_conforming_body".to_string(),
            }],
            parameters: json!({
                "surface_id": surface_id.0,
                "representation": "terrain_conforming",
                "adaptive": true,
            }),
            metadata: json!({
                "kind": "foundation_system",
                "placement_kind": "terrain_conforming_foundation",
                "body_id": foundation_id.0,
            }),
        },
        refinement_state: None,
        obligations: None,
        claim_grounding: None,
        authoring_provenance: None,
    }
    .apply_to(world);
    foundation_structure_id
}

#[derive(Debug, Clone, Copy)]
struct StructurePlantTarget {
    group_id: ElementId,
    foundation_structure_id: ElementId,
    foundation_body_id: ElementId,
}

fn foundation_body_from_system(
    world: &mut World,
    foundation_structure_id: ElementId,
) -> Option<ElementId> {
    let assembly = semantic_assembly_by_id(world, foundation_structure_id)?;
    if assembly.assembly_type != "foundation_system" {
        return None;
    }
    assembly
        .members
        .iter()
        .find(|member| is_foundation_body(world, member.target))
        .map(|member| member.target)
}

fn direct_foundation_body_from_structure(
    world: &mut World,
    structure: &SemanticAssembly,
    explicit_foundation_id: Option<ElementId>,
) -> Option<ElementId> {
    if explicit_foundation_id.is_some_and(|id| is_foundation_body(world, id)) {
        return explicit_foundation_id;
    }
    structure
        .members
        .iter()
        .filter(|member| member.role.contains("foundation"))
        .find(|member| is_foundation_body(world, member.target))
        .map(|member| member.target)
}

fn foundation_system_from_structure(
    world: &mut World,
    structure: &SemanticAssembly,
) -> Option<ElementId> {
    structure.members.iter().find_map(|member| {
        let assembly = semantic_assembly_by_id(world, member.target)?;
        (assembly.assembly_type == "foundation_system").then_some(member.target)
    })
}

fn movable_group_from_structure(
    world: &mut World,
    structure: &SemanticAssembly,
) -> Option<ElementId> {
    structure
        .members
        .iter()
        .find(|member| member.role == "superstructure_group" && is_group(world, member.target))
        .or_else(|| {
            structure
                .members
                .iter()
                .find(|member| is_group(world, member.target))
        })
        .map(|member| member.target)
}

fn ensure_structure_has_foundation_system_member(
    world: &mut World,
    structure_id: ElementId,
    foundation_structure_id: ElementId,
    foundation_body_id: ElementId,
    surface_id: ElementId,
) {
    let Some(entity) = find_entity_by_element_id(world, structure_id) else {
        return;
    };
    let Some(mut assembly) = world.get_mut::<SemanticAssembly>(entity) else {
        return;
    };
    if !assembly
        .members
        .iter()
        .any(|member| member.target == foundation_structure_id)
    {
        assembly.members.push(AssemblyMemberRef {
            target: foundation_structure_id,
            role: "foundation".to_string(),
        });
    }
    if !assembly
        .members
        .iter()
        .any(|member| member.target == foundation_body_id)
    {
        assembly.members.push(AssemblyMemberRef {
            target: foundation_body_id,
            role: "terrain_conforming_foundation".to_string(),
        });
    }
    assembly.parameters = json!({
        "placement": "planted_on_surface",
        "surface_id": surface_id.0,
    });
    assembly.metadata = json!({
        "kind": "structure",
        "placement_kind": "planted_structure",
        "foundation_structure_id": foundation_structure_id.0,
    });
}

fn update_foundation_system_as_adaptive(
    world: &mut World,
    foundation_structure_id: ElementId,
    foundation_body_id: ElementId,
    surface_id: ElementId,
) {
    let Some(entity) = find_entity_by_element_id(world, foundation_structure_id) else {
        return;
    };
    let Some(mut assembly) = world.get_mut::<SemanticAssembly>(entity) else {
        return;
    };
    if let Some(member) = assembly
        .members
        .iter_mut()
        .find(|member| member.target == foundation_body_id)
    {
        member.role = "terrain_conforming_body".to_string();
    } else {
        assembly.members.push(AssemblyMemberRef {
            target: foundation_body_id,
            role: "terrain_conforming_body".to_string(),
        });
    }
    assembly.parameters = json!({
        "surface_id": surface_id.0,
        "representation": "terrain_conforming",
        "adaptive": true,
    });
    assembly.metadata = json!({
        "kind": "foundation_system",
        "placement_kind": "terrain_conforming_foundation",
        "body_id": foundation_body_id.0,
    });
}

fn resolve_structure_plant_target(
    world: &mut World,
    structure_id: ElementId,
    surface_id: ElementId,
    explicit_foundation_id: Option<ElementId>,
) -> Result<StructurePlantTarget, String> {
    let structure = semantic_assembly_by_id(world, structure_id)
        .ok_or_else(|| format!("Structure {} was not found.", structure_id.0))?;
    if structure.assembly_type != "structure" {
        return Err(format!(
            "Plant Structure: {} is a '{}' assembly, not a structure.",
            structure_id.0, structure.assembly_type
        ));
    }
    let group_id = movable_group_from_structure(world, &structure).ok_or_else(|| {
        format!(
            "Plant Structure: structure {} needs a grouped superstructure member to move as one unit.",
            structure_id.0
        )
    })?;
    let foundation_structure_id = foundation_system_from_structure(world, &structure);
    let foundation_body_id = foundation_structure_id
        .and_then(|id| {
            explicit_foundation_id
                .filter(|foundation_id| is_foundation_body(world, *foundation_id))
                .or_else(|| foundation_body_from_system(world, id))
        })
        .or_else(|| direct_foundation_body_from_structure(world, &structure, explicit_foundation_id))
        .ok_or_else(|| {
            format!(
                "Plant Structure: structure {} needs a bottom foundation object; add one or pass foundation_id.",
                structure_id.0
            )
        })?;
    let foundation_structure_id = foundation_structure_id.unwrap_or_else(|| {
        create_foundation_structure_assembly(world, foundation_body_id, surface_id)
    });
    Ok(StructurePlantTarget {
        group_id,
        foundation_structure_id,
        foundation_body_id,
    })
}

fn create_structure_assembly(
    world: &mut World,
    planted_group_id: ElementId,
    foundation_structure_id: ElementId,
    foundation_id: ElementId,
    surface_id: ElementId,
    label: String,
) -> ElementId {
    let structure_id = world.resource::<ElementIdAllocator>().next_id();
    AssemblySnapshot {
        element_id: structure_id,
        assembly: SemanticAssembly {
            assembly_type: "structure".to_string(),
            label,
            members: vec![
                AssemblyMemberRef {
                    target: planted_group_id,
                    role: "superstructure_group".to_string(),
                },
                AssemblyMemberRef {
                    target: foundation_structure_id,
                    role: "foundation".to_string(),
                },
                AssemblyMemberRef {
                    target: foundation_id,
                    role: "terrain_conforming_foundation".to_string(),
                },
            ],
            parameters: json!({
                "placement": "planted_on_surface",
                "surface_id": surface_id.0,
            }),
            metadata: json!({
                "kind": "structure",
                "placement_kind": "planted_structure",
                "foundation_structure_id": foundation_structure_id.0,
            }),
        },
        refinement_state: None,
        obligations: None,
        claim_grounding: None,
        authoring_provenance: None,
    }
    .apply_to(world);
    structure_id
}

fn find_foundation_structure_for_body(world: &mut World, body_id: ElementId) -> Option<ElementId> {
    let mut query = world.query::<(&ElementId, &SemanticAssembly)>();
    query
        .iter(world)
        .find(|(_, assembly)| {
            assembly.assembly_type == "foundation_system"
                && assembly
                    .members
                    .iter()
                    .any(|member| member.target == body_id)
        })
        .map(|(id, _)| *id)
}

fn resolve_conforming_foundation_target(
    world: &mut World,
    target_id: ElementId,
) -> Result<(Option<ElementId>, ElementId), String> {
    let target_entity = find_entity_by_element_id(world, target_id)
        .ok_or_else(|| "target not found".to_string())?;
    if world.get::<ConformingSolid>(target_entity).is_some() {
        return Ok((
            find_foundation_structure_for_body(world, target_id),
            target_id,
        ));
    }

    let assembly = world
        .get::<SemanticAssembly>(target_entity)
        .ok_or_else(|| {
            "target is neither a conforming foundation body nor a foundation_system assembly"
                .to_string()
        })?
        .clone();
    if assembly.assembly_type != "foundation_system" {
        return Err("target semantic assembly is not a foundation_system".to_string());
    }
    for member in &assembly.members {
        let Some(member_entity) = find_entity_by_element_id(world, member.target) else {
            continue;
        };
        if world.get::<ConformingSolid>(member_entity).is_some() {
            return Ok((Some(target_id), member.target));
        }
    }
    Err("foundation_system has no adaptive conforming foundation body to demote".to_string())
}

fn remove_mesh_asset_for_id(world: &mut World, element_id: ElementId) {
    let Some(entity) = find_entity_by_element_id(world, element_id) else {
        return;
    };
    let mesh_id = world
        .get_entity(entity)
        .ok()
        .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh| mesh.id()));
    if let Some(mesh_id) = mesh_id {
        world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
    }
}

fn update_foundation_structure_after_demote(
    world: &mut World,
    foundation_structure_id: ElementId,
    body_id: ElementId,
    surface_id: ElementId,
    representation: &str,
    metrics: crate::conforming::ConformingMetrics,
) {
    let Some(entity) = find_entity_by_element_id(world, foundation_structure_id) else {
        return;
    };
    let Some(mut assembly) = world.get_mut::<SemanticAssembly>(entity) else {
        return;
    };
    for member in &mut assembly.members {
        if member.target == body_id {
            member.role = match representation {
                "max_height_box" => "max_height_box_body".to_string(),
                _ => "frozen_conforming_snapshot".to_string(),
            };
        }
    }
    assembly.parameters = json!({
        "surface_id": surface_id.0,
        "representation": representation,
        "adaptive": false,
        "y_top": metrics.y_top,
        "min_thickness": metrics.min_thickness,
        "max_thickness": metrics.max_thickness,
    });
    assembly.metadata = json!({
        "kind": "foundation_system",
        "placement_kind": "demoted_conforming_foundation",
        "body_id": body_id.0,
        "demoted_representation": representation,
    });
}

fn mark_structure_demoted(world: &mut World, structure_id: ElementId, representation: &str) {
    let Some(entity) = find_entity_by_element_id(world, structure_id) else {
        return;
    };
    let Some(mut assembly) = world.get_mut::<SemanticAssembly>(entity) else {
        return;
    };
    let label = assembly.label.clone();
    let members = assembly.members.clone();
    let parameters = assembly.parameters.clone();
    assembly.metadata = json!({
        "kind": "structure",
        "placement_kind": "foundation_demoted",
        "previous_placement_kind": "planted_structure",
        "demoted_foundation_representation": representation,
    });
    assembly.label = label;
    assembly.members = members;
    assembly.parameters = parameters;
}

fn release_planted_links_for_foundation(
    world: &mut World,
    foundation_id: ElementId,
    representation: &str,
) -> Vec<ElementId> {
    let planted = {
        let mut query = world.query::<(Entity, &ElementId, &PlantedStructure)>();
        query
            .iter(world)
            .filter(|(_, _, planted)| planted.foundation_id == foundation_id)
            .map(|(entity, id, planted)| (entity, *id, planted.structure_id))
            .collect::<Vec<_>>()
    };
    for (_, _, structure_id) in &planted {
        mark_structure_demoted(world, *structure_id, representation);
    }
    for (entity, _, _) in &planted {
        world.entity_mut(*entity).remove::<PlantedStructure>();
        world.entity_mut(*entity).remove::<PlantedOnSurface>();
    }
    planted.into_iter().map(|(_, id, _)| id).collect()
}

fn reseat_planted_structures(world: &mut World) {
    let planted = {
        let mut query = world.query::<(Entity, &ElementId, &PlantedStructure)>();
        query
            .iter(world)
            .map(|(entity, id, planted)| (entity, *id, *planted))
            .collect::<Vec<_>>()
    };
    for (_, group_id, planted) in planted {
        let members = group_member_ids(world, group_id);
        if members.is_empty() {
            continue;
        }
        let Some(bounds) = member_bounds_excluding(world, &members, planted.foundation_id) else {
            continue;
        };
        let Some(top) = foundation_top(world, planted.foundation_id, planted.surface_id) else {
            continue;
        };
        let target_base = top + planted.base_offset_from_foundation_top;
        let delta_y = target_base - bounds.min.y;
        if delta_y.abs() <= 0.001 {
            continue;
        }
        translate_members_excluding(
            world,
            &members,
            planted.foundation_id,
            Vec3::new(0.0, delta_y, 0.0),
        );
    }
}

fn execute_plant_structure(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    let (structure_id, surface_id) = match resolve_plant_structure_args(world, params) {
        Ok(args) => args,
        Err(error) => {
            set_plant_structure_status(world, error.clone());
            return Err(error);
        }
    };
    let min_thickness = optional_f32(params, "min_thickness", DEFAULT_MIN_THICKNESS).max(0.0);
    let max_depth =
        optional_f32(params, "max_depth", DEFAULT_MAX_DEPTH).max(min_thickness.max(0.01));
    let resolution = optional_f32(params, "resolution", 0.5).max(0.05);
    let explicit_foundation_id = optional_id(params, "foundation_id");

    let target =
        resolve_structure_plant_target(world, structure_id, surface_id, explicit_foundation_id)?;
    let group_entity = find_entity_by_element_id(world, target.group_id).ok_or_else(|| {
        format!(
            "Plant Structure: group {} was not found.",
            target.group_id.0
        )
    })?;
    if world.get::<PlantedOnSurface>(group_entity).is_some()
        || world.get::<PlantedStructure>(group_entity).is_some()
    {
        return Err(format!(
            "Plant Structure: structure {} is already planted; release or unplant first.",
            structure_id.0
        ));
    }

    if !group_member_ids(world, target.group_id).contains(&target.foundation_body_id) {
        if let Some((_, after)) =
            group_membership_add_snapshots(world, target.group_id, target.foundation_body_id)
        {
            after.apply_to(world);
        }
    }

    replace_foundation_with_conforming_body(
        world,
        target.foundation_body_id,
        surface_id,
        min_thickness,
        max_depth,
        resolution,
    )?;
    update_foundation_system_as_adaptive(
        world,
        target.foundation_structure_id,
        target.foundation_body_id,
        surface_id,
    );
    ensure_structure_has_foundation_system_member(
        world,
        structure_id,
        target.foundation_structure_id,
        target.foundation_body_id,
        surface_id,
    );

    let members = group_member_ids(world, target.group_id);
    let bounds =
        member_bounds_excluding(world, &members, target.foundation_body_id).ok_or_else(|| {
            format!(
            "Plant Structure: structure {} has no measurable superstructure above its foundation.",
            structure_id.0
        )
        })?;
    let y_top = foundation_top(world, target.foundation_body_id, surface_id).ok_or_else(|| {
        "Plant Structure: foundation footprint does not overlap terrain.".to_string()
    })?;
    let y_delta = y_top - bounds.min.y;
    if y_delta.abs() > 0.001 {
        translate_members_excluding(
            world,
            &members,
            target.foundation_body_id,
            Vec3::new(0.0, y_delta, 0.0),
        );
        translate_group_frame(world, target.group_id, Vec3::new(0.0, y_delta, 0.0));
    }

    let group_entity = find_entity_by_element_id(world, target.group_id).ok_or_else(|| {
        format!(
            "Plant Structure: group {} was not found.",
            target.group_id.0
        )
    })?;
    world.entity_mut(group_entity).insert(PlantedOnSurface {
        foundation_id: target.foundation_body_id,
        y_delta,
        hidden_foundation: None,
    });
    world.entity_mut(group_entity).insert(PlantedStructure {
        structure_id,
        foundation_structure_id: Some(target.foundation_structure_id),
        foundation_id: target.foundation_body_id,
        surface_id,
        base_offset_from_foundation_top: 0.0,
    });
    set_plant_structure_status(
        world,
        format!(
            "Planted structure {} on terrain {} using foundation {}",
            structure_id.0, surface_id.0, target.foundation_body_id.0
        ),
    );

    Ok(CommandResult {
        modified: vec![
            structure_id.0,
            target.group_id.0,
            target.foundation_structure_id.0,
            target.foundation_body_id.0,
        ],
        output: Some(json!({
            "structure_id": structure_id.0,
            "planted_group_id": target.group_id.0,
            "foundation_structure_id": target.foundation_structure_id.0,
            "foundation_id": target.foundation_body_id.0,
            "surface_id": surface_id.0,
            "y_top": y_top,
            "raised_by": y_delta,
            "contract": "planted_structure",
        })),
        ..CommandResult::empty()
    })
}

fn execute_plant_on_surface(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    let target_id = required_id(params, "target_id")?;
    let surface_id = required_id(params, "surface_id")?;
    let min_thickness = optional_f32(params, "min_thickness", DEFAULT_MIN_THICKNESS).max(0.0);
    let max_depth =
        optional_f32(params, "max_depth", DEFAULT_MAX_DEPTH).max(min_thickness.max(0.01));
    let resolution = optional_f32(params, "resolution", 0.5).max(0.05);
    let hide_id = params
        .get("hide_element_id")
        .and_then(Value::as_u64)
        .map(ElementId);

    let target_entity = find_entity_by_element_id(world, target_id)
        .ok_or_else(|| "target not found".to_string())?;
    if world.get::<PlantedOnSurface>(target_entity).is_some() {
        return Err("target is already planted; unplant first".to_string());
    }

    let target_before =
        capture(world, target_entity).ok_or_else(|| "cannot capture target".to_string())?;
    let bounds = target_before
        .0
        .bounds()
        .ok_or_else(|| "target has no bounds".to_string())?;
    let center = Vec2::new(
        (bounds.min.x + bounds.max.x) * 0.5,
        (bounds.min.z + bounds.max.z) * 0.5,
    );
    let half_extents = Vec2::new(
        ((bounds.max.x - bounds.min.x) * 0.5).max(0.05),
        ((bounds.max.z - bounds.min.z) * 0.5).max(0.05),
    );
    let base_y = bounds.min.y;

    let heightfield = find_heightfield(world, surface_id)
        .ok_or_else(|| "surface height field not ready".to_string())?;
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
            floor_datum: None,
        },
    }
    .apply_to(world);
    let foundation_structure_id =
        create_foundation_structure_assembly(world, foundation_id, surface_id);

    // Seat the object on the foundation's flat top. Group targets need their
    // authored members moved as a unit; a group snapshot itself is membership
    // metadata and intentionally has no geometric translate.
    translate_target(world, target_id, Vec3::new(0.0, y_delta, 0.0));
    if is_group(world, target_id) {
        if let Some((_, after)) = group_membership_add_snapshots(world, target_id, foundation_id) {
            after.apply_to(world);
        }
    }
    let structure_id = create_structure_assembly(
        world,
        target_id,
        foundation_structure_id,
        foundation_id,
        surface_id,
        format!("Planted {}", target_before.0.label()),
    );

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
    world.entity_mut(target_entity).insert(PlantedStructure {
        structure_id,
        foundation_structure_id: Some(foundation_structure_id),
        foundation_id,
        surface_id,
        base_offset_from_foundation_top: 0.0,
    });

    Ok(CommandResult {
        output: Some(json!({
            "structure_id": structure_id.0,
            "foundation_structure_id": foundation_structure_id.0,
            "planted_group_id": target_id.0,
            "foundation_id": foundation_id.0,
            "y_top": y_top,
            "raised_by": y_delta,
        })),
        ..CommandResult::empty()
    })
}

fn execute_release_planted_structure(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let target_id = required_id(params, "target_id")?;
    let target_entity = find_entity_by_element_id(world, target_id)
        .ok_or_else(|| "target not found".to_string())?;
    world.entity_mut(target_entity).remove::<PlantedStructure>();
    world.entity_mut(target_entity).remove::<PlantedOnSurface>();
    Ok(CommandResult::empty())
}

fn execute_demote_conforming_foundation(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let target_id = required_id(params, "target_id")?;
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("snapshot");
    if !matches!(mode, "snapshot" | "max_height_box") {
        return Err("mode must be 'snapshot' or 'max_height_box'".to_string());
    }

    let (foundation_structure_id, foundation_body_id) =
        resolve_conforming_foundation_target(world, target_id)?;
    let foundation_entity = find_entity_by_element_id(world, foundation_body_id)
        .ok_or_else(|| "foundation body not found".to_string())?;
    let solid = world
        .get::<ConformingSolid>(foundation_entity)
        .cloned()
        .ok_or_else(|| "foundation body is not an adaptive conforming solid".to_string())?;
    let heightfield = find_heightfield(world, solid.surface_id)
        .ok_or_else(|| "surface height field not ready".to_string())?;

    let metrics = match mode {
        "snapshot" => {
            let (mesh, metrics) = build_conforming_triangle_mesh(
                &solid,
                &heightfield,
                Some(format!("{} frozen snapshot", solid.name)),
            )
            .ok_or_else(|| "could not build conforming foundation snapshot".to_string())?;
            remove_mesh_asset_for_id(world, foundation_body_id);
            despawn_by_element_id(world, foundation_body_id);
            TriangleMeshSnapshot {
                element_id: foundation_body_id,
                primitive: mesh,
                layer: None,
                material_assignment: None,
                semantic_shadow: None,
            }
            .apply_to(world);
            metrics
        }
        "max_height_box" => {
            let metrics = conforming_metrics(&solid, &heightfield)
                .ok_or_else(|| "could not measure conforming foundation".to_string())?;
            let height = metrics.max_thickness.max(0.01);
            remove_mesh_asset_for_id(world, foundation_body_id);
            despawn_by_element_id(world, foundation_body_id);
            PrimitiveSnapshot {
                element_id: foundation_body_id,
                primitive: BoxPrimitive {
                    centre: Vec3::new(
                        solid.position.x,
                        metrics.y_top - height * 0.5,
                        solid.position.y,
                    ),
                    half_extents: Vec3::new(
                        solid.half_extents.x.max(0.01),
                        height * 0.5,
                        solid.half_extents.y.max(0.01),
                    ),
                },
                rotation: ShapeRotation(Quat::from_rotation_y(solid.yaw)),
                material_assignment: None,
                opening_context: None,
            }
            .apply_to(world);
            metrics
        }
        _ => unreachable!(),
    };

    if let Some(foundation_structure_id) = foundation_structure_id {
        update_foundation_structure_after_demote(
            world,
            foundation_structure_id,
            foundation_body_id,
            solid.surface_id,
            mode,
            metrics,
        );
    }
    let released_planted_groups =
        release_planted_links_for_foundation(world, foundation_body_id, mode);

    Ok(CommandResult {
        output: Some(json!({
            "foundation_structure_id": foundation_structure_id.map(|id| id.0),
            "foundation_body_id": foundation_body_id.0,
            "demoted_representation": mode,
            "y_top": metrics.y_top,
            "max_thickness": metrics.max_thickness,
            "released_planted_groups": released_planted_groups
                .iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
        })),
        ..CommandResult::empty()
    })
}

fn execute_unplant_on_surface(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    let target_id = required_id(params, "target_id")?;
    let target_entity = find_entity_by_element_id(world, target_id)
        .ok_or_else(|| "target not found".to_string())?;
    let link = world
        .get::<PlantedOnSurface>(target_entity)
        .copied()
        .ok_or_else(|| "target is not planted".to_string())?;

    // Remove the foundation (and free its mesh).
    remove_member_from_groups(world, link.foundation_id);
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
    translate_target(world, target_id, Vec3::new(0.0, -link.y_delta, 0.0));

    // Un-hide the original foundation.
    if let Some(hidden) = link.hidden_foundation {
        if let Some(entity) = find_entity_by_element_id(world, hidden) {
            world.entity_mut(entity).insert(Visibility::Inherited);
        }
    }

    world.entity_mut(target_entity).remove::<PlantedOnSurface>();
    world.entity_mut(target_entity).remove::<PlantedStructure>();

    Ok(CommandResult::empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conforming::ConformingSolidFactory;
    use talos3d_core::{
        authored_entity::AuthoredEntity,
        capability_registry::CapabilityRegistry,
        plugins::modeling::{
            assembly::SemanticAssembly,
            generic_factory::PrimitiveFactory,
            generic_snapshot::PrimitiveSnapshot,
            group::{GroupFactory, GroupFrame, GroupMembers, GroupSnapshot},
            primitives::{BoxPrimitive, ShapeRotation},
        },
    };

    fn flat_heightfield(height: f32) -> TerrainHeightfield {
        let points = [
            Vec3::new(-5.0, height, -5.0),
            Vec3::new(5.0, height, -5.0),
            Vec3::new(5.0, height, 5.0),
            Vec3::new(-5.0, height, 5.0),
        ];
        TerrainHeightfield::build(&points, &[], 1.0, 0.0).expect("heightfield")
    }

    fn ramp_heightfield() -> TerrainHeightfield {
        let points = [
            Vec3::new(-5.0, -5.0, -5.0),
            Vec3::new(5.0, 5.0, -5.0),
            Vec3::new(5.0, 5.0, 5.0),
            Vec3::new(-5.0, -5.0, 5.0),
        ];
        TerrainHeightfield::build(&points, &[], 1.0, 0.0).expect("heightfield")
    }

    fn test_world() -> World {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(GroupFactory);
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        registry.register_factory(ConformingSolidFactory);
        world.insert_resource(registry);

        let mut allocator = ElementIdAllocator::default();
        allocator.set_next(1000);
        world.insert_resource(allocator);
        world
    }

    #[test]
    fn plant_group_translates_leaf_members_and_group_frame() {
        let mut world = test_world();
        world.spawn((ElementId(10), flat_heightfield(2.0)));

        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(1),
            name: "Grouped building".to_string(),
            member_ids: vec![ElementId(2)],
            frame: GroupFrame::default(),
            composite: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let result = execute_plant_on_surface(
            &mut world,
            &json!({
                "target_id": 1,
                "surface_id": 10,
                "min_thickness": 0.2,
            }),
        )
        .expect("plant grouped target");
        let raised_by = result
            .output
            .as_ref()
            .and_then(|output| output.get("raised_by"))
            .and_then(Value::as_f64)
            .expect("raised_by") as f32;
        let foundation_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_id");
        let foundation_structure_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_structure_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_structure_id");
        let structure_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("structure_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("structure_id");
        assert!((raised_by - 2.2).abs() < 0.05);

        let box_entity = find_entity_by_element_id(&mut world, ElementId(2)).expect("box entity");
        let primitive = world
            .get::<BoxPrimitive>(box_entity)
            .expect("moved box primitive");
        assert!(
            (primitive.centre.y - 2.7).abs() < 0.05,
            "member centre should be raised with the group, got {}",
            primitive.centre.y
        );

        let group_entity =
            find_entity_by_element_id(&mut world, ElementId(1)).expect("group entity");
        let members = world
            .get::<GroupMembers>(group_entity)
            .expect("group members");
        assert!(
            members.member_ids.contains(&foundation_id),
            "planted foundation should be part of the grouped planted object"
        );
        assert!(
            (members.frame.translation.y - raised_by).abs() < 0.05,
            "group frame should track the raised assembly datum"
        );
        assert!(world.get::<PlantedOnSurface>(group_entity).is_some());
        let planted = world
            .get::<PlantedStructure>(group_entity)
            .expect("planted structure behavior");
        assert_eq!(planted.structure_id, structure_id);
        assert_eq!(
            planted.foundation_structure_id,
            Some(foundation_structure_id)
        );
        assert_eq!(planted.foundation_id, foundation_id);

        let foundation_structure_entity =
            find_entity_by_element_id(&mut world, foundation_structure_id)
                .expect("foundation structure entity");
        let foundation_structure = world
            .get::<SemanticAssembly>(foundation_structure_entity)
            .expect("semantic foundation assembly");
        assert_eq!(foundation_structure.assembly_type, "foundation_system");
        assert!(foundation_structure.members.iter().any(|member| {
            member.target == foundation_id && member.role == "terrain_conforming_body"
        }));

        let structure_entity =
            find_entity_by_element_id(&mut world, structure_id).expect("structure entity");
        let structure = world
            .get::<SemanticAssembly>(structure_entity)
            .expect("semantic structure assembly");
        assert_eq!(structure.assembly_type, "structure");
        assert!(structure
            .members
            .iter()
            .any(|member| member.target == foundation_structure_id && member.role == "foundation"));
        assert!(structure
            .members
            .iter()
            .any(|member| member.target == foundation_id
                && member.role == "terrain_conforming_foundation"));
    }

    #[test]
    fn plant_structure_uses_selected_group_and_terrain_and_converts_foundation() {
        let mut world = test_world();
        world.spawn((ElementId(10), flat_heightfield(2.0), Selected));

        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 1.2, 0.0),
                half_extents: Vec3::new(1.0, 0.8, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        PrimitiveSnapshot {
            element_id: ElementId(3),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.2, 0.0),
                half_extents: Vec3::new(1.2, 0.2, 1.2),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(1),
            name: "Structure group".to_string(),
            member_ids: vec![ElementId(2), ElementId(3)],
            frame: GroupFrame::default(),
            composite: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);
        let group_entity =
            find_entity_by_element_id(&mut world, ElementId(1)).expect("group entity");
        world.entity_mut(group_entity).insert(Selected);

        AssemblySnapshot {
            element_id: ElementId(20),
            assembly: SemanticAssembly {
                assembly_type: "structure".to_string(),
                label: "Selectable structure".to_string(),
                members: vec![
                    AssemblyMemberRef {
                        target: ElementId(1),
                        role: "superstructure_group".to_string(),
                    },
                    AssemblyMemberRef {
                        target: ElementId(3),
                        role: "foundation".to_string(),
                    },
                ],
                parameters: json!({}),
                metadata: json!({ "kind": "structure" }),
            },
            refinement_state: None,
            obligations: None,
            claim_grounding: None,
            authoring_provenance: None,
        }
        .apply_to(&mut world);

        let result = execute_plant_structure(
            &mut world,
            &json!({
                "min_thickness": 0.2,
            }),
        )
        .expect("plant selected structure");
        let foundation_structure_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_structure_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_structure_id");
        assert_eq!(
            result
                .output
                .as_ref()
                .and_then(|output| output.get("structure_id"))
                .and_then(Value::as_u64),
            Some(20)
        );

        let foundation_entity =
            find_entity_by_element_id(&mut world, ElementId(3)).expect("foundation entity");
        assert!(world.get::<BoxPrimitive>(foundation_entity).is_none());
        let foundation = world
            .get::<ConformingSolid>(foundation_entity)
            .expect("foundation converted to conforming solid");
        assert_eq!(foundation.surface_id, ElementId(10));
        assert!((foundation.half_extents.x - 1.2).abs() < 0.01);

        let building_entity =
            find_entity_by_element_id(&mut world, ElementId(2)).expect("building entity");
        let building = world
            .get::<BoxPrimitive>(building_entity)
            .expect("building box");
        assert!(
            (building.centre.y - 3.0).abs() < 0.05,
            "superstructure should be seated on terrain foundation top"
        );

        let group_entity =
            find_entity_by_element_id(&mut world, ElementId(1)).expect("group entity");
        let planted = world
            .get::<PlantedStructure>(group_entity)
            .expect("planted structure");
        assert_eq!(planted.structure_id, ElementId(20));
        assert_eq!(
            planted.foundation_structure_id,
            Some(foundation_structure_id)
        );
        assert_eq!(planted.foundation_id, ElementId(3));

        let foundation_structure_entity =
            find_entity_by_element_id(&mut world, foundation_structure_id)
                .expect("foundation structure");
        let foundation_structure = world
            .get::<SemanticAssembly>(foundation_structure_entity)
            .expect("foundation semantic assembly");
        assert_eq!(foundation_structure.assembly_type, "foundation_system");
        assert_eq!(
            foundation_structure.parameters["representation"],
            json!("terrain_conforming")
        );

        let structure_entity =
            find_entity_by_element_id(&mut world, ElementId(20)).expect("structure entity");
        let structure = world
            .get::<SemanticAssembly>(structure_entity)
            .expect("semantic structure");
        assert!(structure
            .members
            .iter()
            .any(|member| member.target == foundation_structure_id && member.role == "foundation"));
    }

    #[test]
    fn plant_structure_reports_selection_guidance_when_arguments_are_missing() {
        let mut world = test_world();
        let err = execute_plant_structure(&mut world, &json!({})).expect_err("missing args");
        assert!(err.contains("select exactly one semantic structure"));
        assert!(err.contains("structure_id"));
        assert!(err.contains("surface_id"));
    }

    #[test]
    fn demote_conforming_foundation_to_snapshot_preserves_semantic_foundation() {
        let mut world = test_world();
        world.spawn((ElementId(10), ramp_heightfield()));

        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(1),
            name: "Grouped building".to_string(),
            member_ids: vec![ElementId(2)],
            frame: GroupFrame::default(),
            composite: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let result = execute_plant_on_surface(
            &mut world,
            &json!({
                "target_id": 1,
                "surface_id": 10,
                "min_thickness": 0.2,
            }),
        )
        .expect("plant grouped target");
        let foundation_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_id");
        let foundation_structure_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_structure_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_structure_id");

        let demote = execute_demote_conforming_foundation(
            &mut world,
            &json!({
                "target_id": foundation_structure_id.0,
                "mode": "snapshot",
            }),
        )
        .expect("demote foundation");
        assert_eq!(
            demote
                .output
                .as_ref()
                .and_then(|output| output.get("foundation_body_id"))
                .and_then(Value::as_u64),
            Some(foundation_id.0)
        );

        let foundation_entity =
            find_entity_by_element_id(&mut world, foundation_id).expect("foundation entity");
        assert!(world.get::<ConformingSolid>(foundation_entity).is_none());
        let mesh = world
            .get::<talos3d_core::plugins::modeling::primitives::TriangleMesh>(foundation_entity)
            .expect("frozen foundation mesh");
        assert!(!mesh.vertices.is_empty());
        assert!(!mesh.faces.is_empty());

        let foundation_structure_entity =
            find_entity_by_element_id(&mut world, foundation_structure_id)
                .expect("foundation structure entity");
        let foundation_structure = world
            .get::<SemanticAssembly>(foundation_structure_entity)
            .expect("semantic foundation assembly");
        assert_eq!(
            foundation_structure.parameters["representation"],
            json!("snapshot")
        );
        assert!(foundation_structure.members.iter().any(|member| {
            member.target == foundation_id && member.role == "frozen_conforming_snapshot"
        }));

        let group_entity =
            find_entity_by_element_id(&mut world, ElementId(1)).expect("group entity");
        assert!(world.get::<PlantedStructure>(group_entity).is_none());
        assert!(world.get::<PlantedOnSurface>(group_entity).is_none());
    }

    #[test]
    fn demote_conforming_foundation_to_max_height_box_uses_max_thickness() {
        let mut world = test_world();
        world.spawn((ElementId(10), ramp_heightfield()));

        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(1),
            name: "Grouped building".to_string(),
            member_ids: vec![ElementId(2)],
            frame: GroupFrame::default(),
            composite: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let result = execute_plant_on_surface(
            &mut world,
            &json!({
                "target_id": 1,
                "surface_id": 10,
                "min_thickness": 0.2,
                "max_depth": 4.0,
            }),
        )
        .expect("plant grouped target");
        let foundation_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_id");
        let expected_metrics = {
            let foundation_entity =
                find_entity_by_element_id(&mut world, foundation_id).expect("foundation entity");
            let solid = world
                .get::<ConformingSolid>(foundation_entity)
                .expect("conforming solid")
                .clone();
            let heightfield = find_heightfield(&mut world, ElementId(10)).expect("heightfield");
            conforming_metrics(&solid, &heightfield).expect("metrics")
        };

        execute_demote_conforming_foundation(
            &mut world,
            &json!({
                "target_id": foundation_id.0,
                "mode": "max_height_box",
            }),
        )
        .expect("demote foundation");

        let foundation_entity =
            find_entity_by_element_id(&mut world, foundation_id).expect("foundation entity");
        assert!(world.get::<ConformingSolid>(foundation_entity).is_none());
        let primitive = world
            .get::<BoxPrimitive>(foundation_entity)
            .expect("max-height box");
        assert!((primitive.half_extents.y * 2.0 - expected_metrics.max_thickness).abs() < 0.01);
        assert!(
            (primitive.centre.y + primitive.half_extents.y - expected_metrics.y_top).abs() < 0.01
        );
    }

    #[test]
    fn planted_structure_reseats_superstructure_after_horizontal_move() {
        let mut world = test_world();
        world.spawn((ElementId(10), ramp_heightfield()));

        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(1),
            name: "Grouped building".to_string(),
            member_ids: vec![ElementId(2)],
            frame: GroupFrame::default(),
            composite: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let result = execute_plant_on_surface(
            &mut world,
            &json!({
                "target_id": 1,
                "surface_id": 10,
                "min_thickness": 0.2,
            }),
        )
        .expect("plant grouped target");
        let foundation_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_id");
        let initial_top =
            foundation_top(&mut world, foundation_id, ElementId(10)).expect("initial top");

        translate_target(&mut world, ElementId(1), Vec3::new(2.0, 0.0, 0.0));
        let moved_top =
            foundation_top(&mut world, foundation_id, ElementId(10)).expect("moved top");
        assert!(
            moved_top > initial_top + 0.5,
            "test surface should put the moved foundation at a higher top: initial {initial_top}, moved {moved_top}"
        );
        reseat_planted_structures(&mut world);

        let box_entity = find_entity_by_element_id(&mut world, ElementId(2)).expect("box entity");
        let primitive = world
            .get::<BoxPrimitive>(box_entity)
            .expect("moved box primitive");
        assert!(
            (primitive.centre.x - 2.0).abs() < 0.05,
            "superstructure should move horizontally with the planted group"
        );
        assert!(
            (primitive.centre.y - (moved_top + 0.5)).abs() < 0.05,
            "superstructure should re-seat to the new foundation top; got {}",
            primitive.centre.y
        );

        let foundation_entity =
            find_entity_by_element_id(&mut world, foundation_id).expect("foundation entity");
        let foundation = world
            .get::<ConformingSolid>(foundation_entity)
            .expect("foundation solid");
        assert!(
            (foundation.position.x - 2.0).abs() < 0.05,
            "foundation should move horizontally with the planted group"
        );
    }

    #[test]
    fn planted_structure_transform_preview_moves_roof_and_reseats_group_from_child_drag() {
        let mut world = test_world();
        world.spawn((ElementId(10), ramp_heightfield()));

        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        PrimitiveSnapshot {
            element_id: ElementId(4),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 1.25, 0.0),
                half_extents: Vec3::new(1.1, 0.25, 1.1),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(1),
            name: "Planted cottage".to_string(),
            member_ids: vec![ElementId(2), ElementId(4)],
            frame: GroupFrame::default(),
            composite: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let result = execute_plant_on_surface(
            &mut world,
            &json!({
                "target_id": 1,
                "surface_id": 10,
                "min_thickness": 0.2,
            }),
        )
        .expect("plant grouped target");
        let foundation_id = result
            .output
            .as_ref()
            .and_then(|output| output.get("foundation_id"))
            .and_then(Value::as_u64)
            .map(ElementId)
            .expect("foundation_id");

        let roof_entity = find_entity_by_element_id(&mut world, ElementId(4)).expect("roof");
        let roof_before = capture_by_id(&world, ElementId(4)).expect("roof snapshot");
        let mut after = vec![roof_before.translate_by(Vec3::new(2.0, 0.0, 0.0))];
        let state = TransformState {
            mode: TransformMode::Moving,
            initial_snapshots: vec![(roof_entity, roof_before)],
            ..Default::default()
        };

        adjust_planted_structure_transform_preview(&world, &state, &mut after);

        assert!(
            after
                .iter()
                .any(|snapshot| snapshot.element_id() == foundation_id),
            "preview should include the terrain-following foundation"
        );
        let body = after
            .iter()
            .find(|snapshot| snapshot.element_id() == ElementId(2))
            .expect("preview should include the body");
        let roof = after
            .iter()
            .find(|snapshot| snapshot.element_id() == ElementId(4))
            .expect("preview should include the roof");
        assert!(
            (body.center().x - 2.0).abs() < 0.05,
            "body should move with a child drag"
        );
        assert!(
            (roof.center().x - 2.0).abs() < 0.05,
            "roof should move with the planted structure"
        );

        let foundation = after
            .iter()
            .find(|snapshot| snapshot.element_id() == foundation_id)
            .expect("foundation snapshot");
        let moved_top =
            preview_foundation_top(&world, foundation, ElementId(10)).expect("moved top");
        let member_set = [ElementId(2), ElementId(4)]
            .into_iter()
            .collect::<HashSet<_>>();
        let bounds = preview_member_bounds(&after, &member_set).expect("member bounds");
        assert!(
            (bounds.min.y - moved_top).abs() < 0.05,
            "superstructure should preview on the moved conforming foundation top"
        );
    }
}
