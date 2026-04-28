//! Hosted architectural layout propagation.
//!
//! This is the first concrete relation-driven geometry slice for the
//! wall/opening/window workflow. It keeps authored placement intent on
//! relation rows:
//!
//! - `layout_on_host` computes `hosted_on.position_along_wall` for a set of
//!   hosted members.
//! - `hosted_on` applies that placement to the hosted occurrence and its
//!   first-class wall opening, when present.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use talos3d_core::plugins::{
    identity::ElementId,
    modeling::{
        assembly::SemanticRelation,
        mesh_generation::NeedsMesh,
        occurrence::{NeedsEval, OccurrenceIdentity},
    },
};

use crate::{
    components::{Opening, ParentWall, Wall},
    mesh_generation::wall_rotation,
};

const DEFAULT_WINDOW_WIDTH_METRES: f32 = 1.0;
const DEFAULT_WINDOW_HEIGHT_METRES: f32 = 1.2;
const DEFAULT_SILL_HEIGHT_METRES: f32 = 0.9;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutOnHostParams {
    /// Hosted relation ids to control, in left-to-right host-axis order.
    #[serde(default, alias = "member_relation_ids")]
    pub members: Vec<ElementId>,
    /// Current v1 mode. Additional modes can be added without changing
    /// `hosted_on`.
    #[serde(default = "default_fixed_start_total_width_mode")]
    pub mode: String,
    /// Anchor used for the first member. `left_edge` means `start_offset_m`
    /// describes the left edge of the first hosted element. `center` means it
    /// describes the first element center.
    #[serde(default = "default_left_edge_anchor")]
    pub anchor: String,
    /// Distance from the host start point to the first member anchor.
    #[serde(default)]
    pub start_offset_m: f32,
    /// Total distribution width. For `left_edge` anchor this is the span from
    /// the first member left edge to the last member right edge.
    #[serde(default)]
    pub total_width_m: f32,
    /// Width used when the layout controls edge-to-edge spacing.
    #[serde(default = "default_window_width")]
    pub member_width_m: f32,
    #[serde(default = "default_sill_height")]
    pub sill_height_m: f32,
    #[serde(default = "default_window_height")]
    pub member_height_m: f32,
}

fn default_fixed_start_total_width_mode() -> String {
    "fixed_start_total_width".to_string()
}

fn default_left_edge_anchor() -> String {
    "left_edge".to_string()
}

fn default_window_width() -> f32 {
    DEFAULT_WINDOW_WIDTH_METRES
}

fn default_window_height() -> f32 {
    DEFAULT_WINDOW_HEIGHT_METRES
}

fn default_sill_height() -> f32 {
    DEFAULT_SILL_HEIGHT_METRES
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutMemberPlacement {
    pub relation_id: ElementId,
    pub center_offset_m: f32,
    pub position_along_wall: f32,
}

pub fn evaluate_layout_on_host(
    params: &LayoutOnHostParams,
    wall_length_m: f32,
) -> Result<Vec<LayoutMemberPlacement>, String> {
    if params.members.is_empty() {
        return Ok(Vec::new());
    }
    if wall_length_m <= f32::EPSILON {
        return Err("layout_on_host target wall has zero length".to_string());
    }
    if params.member_width_m <= 0.0 {
        return Err("layout_on_host member_width_m must be greater than zero".to_string());
    }
    if params.total_width_m <= 0.0 {
        return Err("layout_on_host total_width_m must be greater than zero".to_string());
    }
    if params.mode != "fixed_start_total_width" && params.mode != "equal_spacing" {
        return Err(format!(
            "unsupported layout_on_host mode '{}'; expected fixed_start_total_width",
            params.mode
        ));
    }

    let count = params.members.len();
    let step = if count > 1 {
        let span_between_centers = match params.anchor.as_str() {
            "left_edge" => (params.total_width_m - params.member_width_m).max(0.0),
            "center" => params.total_width_m,
            other => {
                return Err(format!(
                    "unsupported layout_on_host anchor '{other}'; expected left_edge or center"
                ))
            }
        };
        span_between_centers / (count.saturating_sub(1) as f32)
    } else {
        0.0
    };
    let first_center = match params.anchor.as_str() {
        "left_edge" => params.start_offset_m + params.member_width_m * 0.5,
        "center" => params.start_offset_m,
        _ => unreachable!("anchor was validated above"),
    };

    Ok(params
        .members
        .iter()
        .enumerate()
        .map(|(index, relation_id)| {
            let center_offset_m = first_center + step * index as f32;
            LayoutMemberPlacement {
                relation_id: *relation_id,
                center_offset_m,
                position_along_wall: (center_offset_m / wall_length_m).clamp(0.0, 1.0),
            }
        })
        .collect())
}

pub fn evaluate_hosted_layouts_and_placements(world: &mut World) {
    apply_layout_on_host_relations(world);
    apply_hosted_on_relations(world);
}

fn apply_layout_on_host_relations(world: &mut World) {
    let walls = collect_walls(world);
    let relations = collect_relations(world);
    let relation_entities: HashMap<ElementId, Entity> = relations
        .iter()
        .map(|(entity, id, _)| (*id, *entity))
        .collect();

    let mut writes: Vec<(Entity, Value)> = Vec::new();
    for (_, layout_relation_id, relation) in relations
        .iter()
        .filter(|(_, _, relation)| relation.relation_type == "layout_on_host")
    {
        let Some((_, wall)) = walls.get(&relation.target) else {
            warn!(
                "layout_on_host relation {} targets missing/non-wall entity {}",
                layout_relation_id.0, relation.target.0
            );
            continue;
        };
        let params = match serde_json::from_value::<LayoutOnHostParams>(relation.parameters.clone())
        {
            Ok(params) => params,
            Err(error) => {
                warn!(
                    "layout_on_host relation {} has invalid params: {}",
                    layout_relation_id.0, error
                );
                continue;
            }
        };
        let placements = match evaluate_layout_on_host(&params, wall.length()) {
            Ok(placements) => placements,
            Err(error) => {
                warn!(
                    "layout_on_host relation {} could not evaluate: {}",
                    layout_relation_id.0, error
                );
                continue;
            }
        };

        for placement in placements {
            let Some(hosted_entity) = relation_entities.get(&placement.relation_id).copied() else {
                warn!(
                    "layout_on_host relation {} references missing hosted_on relation {}",
                    layout_relation_id.0, placement.relation_id.0
                );
                continue;
            };
            let Some((_, _, hosted_relation)) = relations
                .iter()
                .find(|(_, id, _)| *id == placement.relation_id)
            else {
                continue;
            };
            if hosted_relation.relation_type != "hosted_on" {
                warn!(
                    "layout_on_host relation {} member {} is '{}', not hosted_on",
                    layout_relation_id.0, placement.relation_id.0, hosted_relation.relation_type
                );
                continue;
            }

            let mut next = hosted_relation.parameters.clone();
            ensure_object(&mut next);
            if let Some(object) = next.as_object_mut() {
                object.insert(
                    "position_along_wall".to_string(),
                    json!(placement.position_along_wall),
                );
                object.insert(
                    "center_offset_m".to_string(),
                    json!(placement.center_offset_m),
                );
                object
                    .entry("sill_height".to_string())
                    .or_insert(json!(params.sill_height_m));
                object
                    .entry("window_height_m".to_string())
                    .or_insert(json!(params.member_height_m));
                object
                    .entry("window_width_m".to_string())
                    .or_insert(json!(params.member_width_m));
                object.insert("layout_on_host_id".to_string(), json!(layout_relation_id.0));
            }
            writes.push((hosted_entity, next));
        }
    }

    for (entity, parameters) in writes {
        if let Some(mut relation) = world.get_mut::<SemanticRelation>(entity) {
            relation.parameters = parameters;
        }
    }
}

fn apply_hosted_on_relations(world: &mut World) {
    let walls = collect_walls(world);
    let relations = collect_relations(world);
    let mut transform_writes: Vec<(Entity, Transform, bool, f32)> = Vec::new();
    let mut opening_writes: Vec<(Entity, Entity, ElementId, f32, f32, f32, f32)> = Vec::new();

    for (_, relation_id, relation) in relations
        .iter()
        .filter(|(_, _, relation)| relation.relation_type == "hosted_on")
    {
        let Some((wall_entity, wall)) = walls.get(&relation.target) else {
            warn!(
                "hosted_on relation {} targets missing/non-wall entity {}",
                relation_id.0, relation.target.0
            );
            continue;
        };
        let position = relation
            .parameters
            .get("position_along_wall")
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        let sill_height = relation
            .parameters
            .get("sill_height")
            .or_else(|| relation.parameters.get("sill_height_m"))
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .unwrap_or(DEFAULT_SILL_HEIGHT_METRES);
        let window_height = relation
            .parameters
            .get("window_height_m")
            .or_else(|| relation.parameters.get("opening_height_m"))
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .unwrap_or(DEFAULT_WINDOW_HEIGHT_METRES);
        let window_width = relation
            .parameters
            .get("window_width_m")
            .or_else(|| relation.parameters.get("opening_width_m"))
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .unwrap_or(DEFAULT_WINDOW_WIDTH_METRES);
        let center = point_on_wall(wall, position, sill_height + window_height * 0.5);
        let transform = Transform::from_translation(center).with_rotation(wall_rotation(wall));
        if let Some(source_entity) = find_entity_by_element_id(world, relation.source) {
            let is_occurrence = world.get::<OccurrenceIdentity>(source_entity).is_some();
            transform_writes.push((source_entity, transform, is_occurrence, wall.thickness));
        }

        if let Some(opening_id) = relation
            .parameters
            .get("opening_element_id")
            .and_then(Value::as_u64)
            .map(ElementId)
        {
            if let Some(opening_entity) = find_entity_by_element_id(world, opening_id) {
                opening_writes.push((
                    opening_entity,
                    *wall_entity,
                    opening_id,
                    position,
                    window_width,
                    window_height,
                    sill_height,
                ));
            }
        }
    }

    for (entity, transform, is_occurrence, wall_thickness) in transform_writes {
        let mut entity_mut = world.entity_mut(entity);
        entity_mut.insert(transform);
        if is_occurrence {
            if let Some(mut identity) = entity_mut.get_mut::<OccurrenceIdentity>() {
                identity
                    .overrides
                    .set("wall_thickness".to_string(), json!(wall_thickness));
            }
            entity_mut.insert(NeedsEval);
        }
    }

    for (opening_entity, wall_entity, _opening_id, position, width, height, sill_height) in
        opening_writes
    {
        world.entity_mut(opening_entity).insert((
            Opening {
                width,
                height,
                sill_height,
                kind: crate::components::OpeningKind::Window,
            },
            ParentWall {
                wall_entity,
                position_along_wall: position,
            },
        ));
        world.entity_mut(wall_entity).insert(NeedsMesh);
    }
}

fn collect_walls(world: &mut World) -> HashMap<ElementId, (Entity, Wall)> {
    let mut query = world.query::<(Entity, &ElementId, &Wall)>();
    query
        .iter(world)
        .map(|(entity, element_id, wall)| (*element_id, (entity, wall.clone())))
        .collect()
}

fn collect_relations(world: &mut World) -> Vec<(Entity, ElementId, SemanticRelation)> {
    let mut query = world.query::<(Entity, &ElementId, &SemanticRelation)>();
    query
        .iter(world)
        .map(|(entity, element_id, relation)| (entity, *element_id, relation.clone()))
        .collect()
}

fn find_entity_by_element_id(world: &mut World, element_id: ElementId) -> Option<Entity> {
    let mut query = world.query::<(Entity, &ElementId)>();
    query
        .iter(world)
        .find_map(|(entity, id)| (*id == element_id).then_some(entity))
}

fn point_on_wall(wall: &Wall, position_along_wall: f32, y: f32) -> Vec3 {
    let direction = (wall.end - wall.start).normalize_or_zero();
    let point = wall.start + direction * (wall.length() * position_along_wall.clamp(0.0, 1.0));
    Vec3::new(point.x, y, point.y)
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(Default::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{BimData, OpeningKind};

    fn spawn_relation(
        world: &mut World,
        id: u64,
        source: u64,
        target: u64,
        relation_type: &str,
        parameters: Value,
    ) {
        world.spawn((
            ElementId(id),
            SemanticRelation {
                source: ElementId(source),
                target: ElementId(target),
                relation_type: relation_type.to_string(),
                parameters,
            },
        ));
    }

    #[test]
    fn equal_spacing_from_left_edge_uses_total_width_as_outer_span() {
        let params = LayoutOnHostParams {
            members: vec![ElementId(10), ElementId(11), ElementId(12)],
            start_offset_m: 1.0,
            total_width_m: 5.0,
            member_width_m: 1.0,
            ..LayoutOnHostParams {
                mode: default_fixed_start_total_width_mode(),
                anchor: default_left_edge_anchor(),
                sill_height_m: default_sill_height(),
                member_height_m: default_window_height(),
                members: Vec::new(),
                start_offset_m: 0.0,
                total_width_m: 0.0,
                member_width_m: default_window_width(),
            }
        };

        let placements = evaluate_layout_on_host(&params, 10.0).expect("layout should evaluate");
        let positions: Vec<f32> = placements
            .iter()
            .map(|placement| (placement.position_along_wall * 1000.0).round() / 1000.0)
            .collect();
        assert_eq!(positions, vec![0.15, 0.35, 0.55]);
        assert_eq!(placements[0].center_offset_m, 1.5);
        assert_eq!(placements[2].center_offset_m, 5.5);
    }

    #[test]
    fn layout_relation_writes_hosted_on_params_and_opening() {
        let mut world = World::new();
        let wall = Wall {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(10.0, 0.0),
            height: 3.0,
            thickness: 0.3,
        };
        world.spawn((ElementId(1), wall.clone(), BimData::default()));
        world.spawn((
            ElementId(2),
            Opening {
                width: 1.0,
                height: 1.2,
                sill_height: 0.9,
                kind: OpeningKind::Window,
            },
            ParentWall {
                wall_entity: Entity::PLACEHOLDER,
                position_along_wall: 0.0,
            },
            BimData::default(),
        ));
        world.spawn((ElementId(20), Transform::default()));
        spawn_relation(
            &mut world,
            30,
            20,
            1,
            "hosted_on",
            json!({ "opening_element_id": 2 }),
        );
        spawn_relation(
            &mut world,
            40,
            20,
            1,
            "layout_on_host",
            json!({
                "members": [30],
                "start_offset_m": 1.0,
                "total_width_m": 1.0,
                "member_width_m": 1.0,
                "sill_height_m": 1.0,
                "member_height_m": 1.2
            }),
        );

        evaluate_hosted_layouts_and_placements(&mut world);

        let relation = find_relation(&mut world, ElementId(30));
        let position = relation.parameters["position_along_wall"]
            .as_f64()
            .expect("position_along_wall should be numeric");
        assert!((position - 0.15).abs() < 1e-6);

        let opening_entity = find_entity_by_element_id(&mut world, ElementId(2)).unwrap();
        let opening = world.get::<Opening>(opening_entity).unwrap();
        let parent_wall = world.get::<ParentWall>(opening_entity).unwrap();
        assert_eq!(opening.sill_height, 1.0);
        assert_eq!(parent_wall.position_along_wall, 0.15);
    }

    fn find_relation(world: &mut World, relation_id: ElementId) -> SemanticRelation {
        let mut query = world.query::<(&ElementId, &SemanticRelation)>();
        query
            .iter(world)
            .find_map(|(id, relation)| (*id == relation_id).then_some(relation.clone()))
            .expect("relation should exist")
    }
}
