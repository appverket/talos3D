use std::any::Any;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use talos3d_core::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        scalar_from_json, vec2_from_json, vec3_from_json, AuthoredEntity, BoxedEntity,
        EntityBounds, HandleInfo, HandleKind, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, HitCandidate, ModelSummaryAccumulator, SnapPoint,
    },
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
        math::{draw_loop, rectangle_corners, scale_point_around_center},
        modeling::mesh_generation::NeedsMesh,
        snap::SnapKind,
        tools::Preview,
    },
};

use crate::{
    components::{BimData, Opening, OpeningKind, ParentWall, Wall},
    mesh_generation::{wall_rotation, wall_transform},
};

const OPENING_PICK_PADDING_METRES: f32 = 0.1;
const OPENING_FACE_OFFSET_METRES: f32 = 0.02;
const OUTLINE_PADDING_METRES: f32 = 0.03;
const OPENING_PREVIEW_COLOR: Color = Color::srgba(0.35, 0.9, 1.0, 0.25);
const OPENING_PREVIEW_DEPTH_BIAS_METRES: f32 = 0.01;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallSnapshot {
    pub element_id: ElementId,
    pub wall: Wall,
    pub bim_data: BimData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpeningSnapshot {
    pub element_id: ElementId,
    pub opening: Opening,
    pub parent_wall: Wall,
    pub parent_wall_element_id: ElementId,
    pub position_along_wall: f32,
    pub bim_data: BimData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum ArchitecturalSnapshotJson {
    Wall(WallSnapshot),
    Opening(OpeningSnapshot),
}

impl From<WallSnapshot> for BoxedEntity {
    fn from(snapshot: WallSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl From<OpeningSnapshot> for BoxedEntity {
    fn from(snapshot: OpeningSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl AuthoredEntity for WallSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "wall"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!(
            "Wall from ({:.2}, {:.2}) to ({:.2}, {:.2})",
            self.wall.start.x, self.wall.start.y, self.wall.end.x, self.wall.end.y
        )
    }

    fn center(&self) -> Vec3 {
        let mid = (self.wall.start + self.wall.end) * 0.5;
        Vec3::new(mid.x, self.wall.height * 0.5, mid.y)
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        WallSnapshot {
            element_id: self.element_id,
            wall: Wall {
                start: self.wall.start + delta.xz(),
                end: self.wall.end + delta.xz(),
                height: self.wall.height,
                thickness: self.wall.thickness,
            },
            bim_data: self.bim_data.clone(),
        }
        .into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let pivot = Vec3::new(
            (self.wall.start.x + self.wall.end.x) * 0.5,
            0.0,
            (self.wall.start.y + self.wall.end.y) * 0.5,
        );
        let start_3d = Vec3::new(self.wall.start.x, 0.0, self.wall.start.y);
        let end_3d = Vec3::new(self.wall.end.x, 0.0, self.wall.end.y);
        let rotated_start = pivot + rotation * (start_3d - pivot);
        let rotated_end = pivot + rotation * (end_3d - pivot);
        WallSnapshot {
            element_id: self.element_id,
            wall: Wall {
                start: Vec2::new(rotated_start.x, rotated_start.z),
                end: Vec2::new(rotated_end.x, rotated_end.z),
                height: self.wall.height,
                thickness: self.wall.thickness,
            },
            bim_data: self.bim_data.clone(),
        }
        .into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let scaled_start = scale_point_around_center(
            Vec3::new(self.wall.start.x, 0.0, self.wall.start.y),
            center,
            factor,
        );
        let scaled_end = scale_point_around_center(
            Vec3::new(self.wall.end.x, 0.0, self.wall.end.y),
            center,
            factor,
        );
        WallSnapshot {
            element_id: self.element_id,
            wall: Wall {
                start: scaled_start.xz(),
                end: scaled_end.xz(),
                height: self.wall.height,
                thickness: self.wall.thickness,
            },
            bim_data: self.bim_data.clone(),
        }
        .into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "start",
                "start",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(wall_point_vec3(self.wall.start))),
                true,
            ),
            property_field_with(
                "end",
                "end",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(wall_point_vec3(self.wall.end))),
                true,
            ),
            property_field(
                "height",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.wall.height)),
            ),
            property_field(
                "thickness",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.wall.thickness)),
            ),
            read_only_property_field(
                "length",
                "length",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(
                    self.wall.start.distance(self.wall.end),
                )),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "start" => snapshot.wall.start = wall_point_from_json(value)?,
            "end" => snapshot.wall.end = wall_point_from_json(value)?,
            "height" => snapshot.wall.height = scalar_from_json(value)?,
            "thickness" => snapshot.wall.thickness = scalar_from_json(value)?,
            _ => {
                return Err(invalid_property_error(
                    "wall",
                    &["start", "end", "height", "thickness"],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        let start = Vec3::new(self.wall.start.x, 0.0, self.wall.start.y);
        let end = Vec3::new(self.wall.end.x, 0.0, self.wall.end.y);
        vec![
            HandleInfo {
                id: "start".to_string(),
                position: start,
                kind: HandleKind::Vertex,
                label: "Start endpoint".to_string(),
            },
            HandleInfo {
                id: "end".to_string(),
                position: end,
                kind: HandleKind::Vertex,
                label: "End endpoint".to_string(),
            },
            HandleInfo {
                id: "midpoint".to_string(),
                position: (start + end) * 0.5,
                kind: HandleKind::Center,
                label: "Midpoint".to_string(),
            },
        ]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        let direction = self.wall.direction().unwrap_or(Vec2::X);
        let normal = Vec2::new(-direction.y, direction.x) * (self.wall.thickness * 0.5);
        let bottom = [
            Vec3::new(
                self.wall.start.x + normal.x,
                0.0,
                self.wall.start.y + normal.y,
            ),
            Vec3::new(self.wall.end.x + normal.x, 0.0, self.wall.end.y + normal.y),
            Vec3::new(self.wall.end.x - normal.x, 0.0, self.wall.end.y - normal.y),
            Vec3::new(
                self.wall.start.x - normal.x,
                0.0,
                self.wall.start.y - normal.y,
            ),
        ];
        let top = bottom.map(|point| point + Vec3::Y * self.wall.height);
        let points = [
            bottom[0], bottom[1], bottom[2], bottom[3], top[0], top[1], top[2], top[3],
        ];
        Some(bounds_from_points(&points))
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let mut snapshot = self.clone();
        match handle_id {
            "start" => snapshot.wall.start = cursor.xz(),
            "end" => snapshot.wall.end = cursor.xz(),
            _ => return None,
        }
        Some(snapshot.into())
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(ArchitecturalSnapshotJson::Wall(self.clone())).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world
                .entity_mut(entity)
                .insert((self.wall.clone(), self.bim_data.clone(), NeedsMesh));
        } else {
            world.spawn((
                self.element_id,
                self.wall.clone(),
                self.bim_data.clone(),
                NeedsMesh,
            ));
        }
    }

    fn apply_with_previous(&self, world: &mut World, previous: Option<&dyn AuthoredEntity>) {
        let Some(previous) = previous.and_then(|snapshot| snapshot.as_any().downcast_ref::<Self>())
        else {
            self.apply_to(world);
            return;
        };

        if previous.wall.length() != self.wall.length()
            || previous.wall.height != self.wall.height
            || previous.wall.thickness != self.wall.thickness
        {
            self.apply_to(world);
            return;
        }

        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.wall.clone(),
                self.bim_data.clone(),
                wall_transform(&self.wall),
            ));
        } else {
            self.apply_to(world);
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(wall_transform(&self.wall))
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_wall_outline(gizmos, &self.wall, color);
    }

    fn preview_line_count(&self) -> usize {
        wall_outline_line_count()
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl AuthoredEntity for OpeningSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "opening"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!(
            "Opening ({:?}) in wall #{}",
            self.opening.kind, self.parent_wall_element_id.0
        )
    }

    fn center(&self) -> Vec3 {
        opening_center(&self.parent_wall, &self.opening, self.position_along_wall)
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let wall_length = self.parent_wall.length();
        let wall_direction = self.parent_wall.direction().unwrap_or(Vec2::X);
        let distance_along_wall =
            self.position_along_wall * wall_length + delta.xz().dot(wall_direction);
        let position_along_wall = if wall_length > f32::EPSILON {
            (distance_along_wall / wall_length).clamp(0.0, 1.0)
        } else {
            self.position_along_wall
        };

        OpeningSnapshot {
            element_id: self.element_id,
            opening: Opening {
                sill_height: self.opening.sill_height + delta.y,
                ..self.opening.clone()
            },
            parent_wall: self.parent_wall.clone(),
            parent_wall_element_id: self.parent_wall_element_id,
            position_along_wall,
            bim_data: self.bim_data.clone(),
        }
        .into()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.clone().into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let wall_length = self.parent_wall.length();
        let wall_direction = self.parent_wall.direction().unwrap_or(Vec2::X);
        let opening_center =
            opening_center(&self.parent_wall, &self.opening, self.position_along_wall);
        let scaled_center = scale_point_around_center(opening_center, center, factor);
        let distance_along_wall = (scaled_center.xz() - self.parent_wall.start).dot(wall_direction);
        let position_along_wall = if wall_length > f32::EPSILON {
            (distance_along_wall / wall_length).clamp(0.0, 1.0)
        } else {
            self.position_along_wall
        };
        let width_scale = wall_axis_scale_factor(wall_direction, factor);
        let height_scale = factor.y.abs();
        let scaled_height = self.opening.height * height_scale;
        let sill_height = scaled_center.y - scaled_height * 0.5;

        OpeningSnapshot {
            element_id: self.element_id,
            opening: Opening {
                width: self.opening.width * width_scale,
                height: scaled_height,
                sill_height,
                kind: self.opening.kind,
            },
            parent_wall: self.parent_wall.clone(),
            parent_wall_element_id: self.parent_wall_element_id,
            position_along_wall,
            bim_data: self.bim_data.clone(),
        }
        .into()
    }

    fn transform_parent(&self) -> Option<ElementId> {
        Some(self.parent_wall_element_id)
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field(
                "width",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.opening.width)),
            ),
            property_field(
                "height",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.opening.height)),
            ),
            property_field(
                "sill_height",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.opening.sill_height)),
            ),
            property_field(
                "position_along_wall",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.position_along_wall)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "width" => snapshot.opening.width = scalar_from_json(value)?,
            "height" => snapshot.opening.height = scalar_from_json(value)?,
            "sill_height" => snapshot.opening.sill_height = scalar_from_json(value)?,
            "kind" => {
                snapshot.opening.kind = parse_opening_kind(
                    value
                        .as_str()
                        .ok_or_else(|| "Opening kind must be a string".to_string())?,
                )?;
            }
            "position_along_wall" => snapshot.position_along_wall = scalar_from_json(value)?,
            _ => {
                return Err(invalid_property_error(
                    "opening",
                    &[
                        "width",
                        "height",
                        "sill_height",
                        "kind",
                        "position_along_wall",
                    ],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![HandleInfo {
            id: "position".to_string(),
            position: self.center(),
            kind: HandleKind::Parameter,
            label: "Position handle".to_string(),
        }]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        let geometry =
            opening_geometry(&self.opening, &self.parent_wall, self.position_along_wall)?;
        let points = [
            geometry.front[0],
            geometry.front[1],
            geometry.front[2],
            geometry.front[3],
            geometry.back[0],
            geometry.back[1],
            geometry.back[2],
            geometry.back[3],
        ];
        Some(bounds_from_points(&points))
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        if handle_id != "position" {
            return None;
        }

        let wall_length = self.parent_wall.length();
        if wall_length <= f32::EPSILON {
            return Some(self.clone().into());
        }

        let wall_direction = self.parent_wall.direction()?;
        let distance_along_wall = (cursor.xz() - self.parent_wall.start).dot(wall_direction);

        Some(
            OpeningSnapshot {
                position_along_wall: (distance_along_wall / wall_length).clamp(0.0, 1.0),
                ..self.clone()
            }
            .into(),
        )
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(ArchitecturalSnapshotJson::Opening(self.clone()))
            .unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        let Some(parent_wall_entity) =
            find_entity_by_element_id(world, self.parent_wall_element_id)
        else {
            return;
        };

        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.opening.clone(),
                ParentWall {
                    wall_entity: parent_wall_entity,
                    position_along_wall: self.position_along_wall,
                },
                self.bim_data.clone(),
            ));
        } else {
            world.spawn((
                self.element_id,
                self.opening.clone(),
                ParentWall {
                    wall_entity: parent_wall_entity,
                    position_along_wall: self.position_along_wall,
                },
                self.bim_data.clone(),
            ));
        }

        world.entity_mut(parent_wall_entity).insert(NeedsMesh);
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        opening_preview_transform(&self.opening, &self.parent_wall, self.position_along_wall)
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_opening_outline(
            gizmos,
            &self.opening,
            &self.parent_wall,
            self.position_along_wall,
            color,
        );
    }

    fn preview_line_count(&self) -> usize {
        opening_outline_line_count()
    }

    fn sync_preview_entity(&self, world: &mut World, existing: Option<Entity>) -> Option<Entity> {
        let mesh = opening_preview_mesh(&self.opening, &self.parent_wall)?;
        let transform =
            opening_preview_transform(&self.opening, &self.parent_wall, self.position_along_wall)?;

        if let Some(existing) = existing {
            if let Some(mesh_id) = world
                .get::<Mesh3d>(existing)
                .map(|mesh_handle| mesh_handle.id())
            {
                if let Some(existing_mesh) = world.resource_mut::<Assets<Mesh>>().get_mut(mesh_id) {
                    *existing_mesh = mesh;
                }
                if let Ok(mut entity_mut) = world.get_entity_mut(existing) {
                    entity_mut.insert(transform);
                }
                return Some(existing);
            }

            self.cleanup_preview_entity(world, existing);
        }

        let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
        let material_handle =
            world
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    base_color: OPENING_PREVIEW_COLOR,
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    ..default()
                });

        Some(
            world
                .spawn((
                    Preview,
                    Mesh3d(mesh_handle),
                    MeshMaterial3d(material_handle),
                    transform,
                ))
                .id(),
        )
    }

    fn cleanup_preview_entity(&self, world: &mut World, preview_entity: Entity) {
        let mesh_id = world.get::<Mesh3d>(preview_entity).map(|mesh| mesh.id());
        let material_id = world
            .get::<MeshMaterial3d<StandardMaterial>>(preview_entity)
            .map(|material| material.id());

        if let Some(mesh_id) = mesh_id {
            world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
        }
        if let Some(material_id) = material_id {
            world
                .resource_mut::<Assets<StandardMaterial>>()
                .remove(material_id);
        }

        let _ = world.despawn(preview_entity);
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

pub struct WallFactory;
pub struct OpeningFactory;

impl AuthoredEntityFactory for WallFactory {
    fn type_name(&self) -> &'static str {
        "wall"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let wall = entity_ref.get::<Wall>()?;
        let bim_data = entity_ref.get::<BimData>()?;
        Some(
            WallSnapshot {
                element_id,
                wall: wall.clone(),
                bim_data: bim_data.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<ArchitecturalSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            ArchitecturalSnapshotJson::Wall(snapshot) => Ok(snapshot.into()),
            ArchitecturalSnapshotJson::Opening(_) => {
                Err("Snapshot JSON did not match the expected entity type".to_string())
            }
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let snapshot = WallSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            wall: Wall {
                start: object
                    .get("start")
                    .map(vec2_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'start'".to_string())?,
                end: object
                    .get("end")
                    .map(vec2_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'end'".to_string())?,
                height: object
                    .get("height")
                    .map(scalar_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'height'".to_string())?,
                thickness: object
                    .get("thickness")
                    .map(scalar_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'thickness'".to_string())?,
            },
            bim_data: BimData::default(),
        };
        validate_wall_geometry(&snapshot.wall)?;
        Ok(snapshot.into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(wall) = entity_ref.get::<Wall>() else {
            return;
        };
        draw_wall_outline(gizmos, wall, color);
    }

    fn selection_line_count(&self, _world: &World, _entity: Entity) -> usize {
        wall_outline_line_count()
    }

    fn collect_snap_points(&self, world: &World, out: &mut Vec<SnapPoint>) {
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let Some(wall) = entity_ref.get::<Wall>() else {
                continue;
            };
            let start = Vec3::new(wall.start.x, 0.0, wall.start.y);
            let end = Vec3::new(wall.end.x, 0.0, wall.end.y);
            out.push(SnapPoint {
                position: start,
                kind: SnapKind::Endpoint,
            });
            out.push(SnapPoint {
                position: end,
                kind: SnapKind::Endpoint,
            });
            out.push(SnapPoint {
                position: (start + end) * 0.5,
                kind: SnapKind::Midpoint,
            });
        }
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut total_wall_length: f32 = 0.0;
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(element_id), Some(wall), Some(bim_data)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<Wall>(),
                entity_ref.get::<BimData>(),
            ) else {
                continue;
            };
            *summary.entity_counts.entry("wall".to_string()).or_insert(0) += 1;
            total_wall_length += wall.length();
            summary.bounding_points.push(
                WallSnapshot {
                    element_id: *element_id,
                    wall: wall.clone(),
                    bim_data: bim_data.clone(),
                }
                .center(),
            );
        }
        if total_wall_length > 0.0 {
            summary.metrics.insert(
                "total_wall_length".to_string(),
                serde_json::json!(total_wall_length),
            );
        }
    }

    fn collect_delete_dependencies(
        &self,
        world: &World,
        requested_ids: &[ElementId],
        out: &mut Vec<ElementId>,
    ) {
        for requested_id in requested_ids {
            let mut __q = world.try_query::<EntityRef>().unwrap();
            let Some(wall_entity) = __q
                .iter(world)
                .find_map(|entity_ref| {
                    let element_id = entity_ref.get::<ElementId>()?;
                    entity_ref
                        .contains::<Wall>()
                        .then_some((*element_id == *requested_id, entity_ref.id()))
                })
                .and_then(|(matches, entity)| matches.then_some(entity))
            else {
                continue;
            };

            let mut __q = world.try_query::<EntityRef>().unwrap();
            for entity_ref in __q.iter(world) {
                let (Some(element_id), Some(parent_wall)) = (
                    entity_ref.get::<ElementId>(),
                    entity_ref.get::<ParentWall>(),
                ) else {
                    continue;
                };
                if entity_ref.contains::<Opening>() && parent_wall.wall_entity == wall_entity {
                    out.push(*element_id);
                }
            }
        }
    }
}

impl AuthoredEntityFactory for OpeningFactory {
    fn type_name(&self) -> &'static str {
        "opening"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let opening = entity_ref.get::<Opening>()?;
        let parent_wall = entity_ref.get::<ParentWall>()?;
        let bim_data = entity_ref.get::<BimData>()?;
        let parent_wall_ref = world.get_entity(parent_wall.wall_entity).ok()?;
        let parent_wall_element_id = *parent_wall_ref.get::<ElementId>()?;
        let wall = parent_wall_ref.get::<Wall>()?;
        Some(
            OpeningSnapshot {
                element_id,
                opening: opening.clone(),
                parent_wall: wall.clone(),
                parent_wall_element_id,
                position_along_wall: parent_wall.position_along_wall,
                bim_data: bim_data.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<ArchitecturalSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            ArchitecturalSnapshotJson::Opening(snapshot) => Ok(snapshot.into()),
            ArchitecturalSnapshotJson::Wall(_) => {
                Err("Snapshot JSON did not match the expected entity type".to_string())
            }
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let parent_wall_element_id = ElementId(
            object
                .get("parent_wall_element_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    "Missing or invalid integer field 'parent_wall_element_id'".to_string()
                })?,
        );
        let mut __q = world.try_query::<EntityRef>().unwrap();
        let parent_wall = __q
            .iter(world)
            .find_map(|entity_ref| {
                let element_id = entity_ref.get::<ElementId>()?;
                let wall = entity_ref.get::<Wall>()?;
                (*element_id == parent_wall_element_id).then(|| wall.clone())
            })
            .ok_or_else(|| format!("Entity not found: {}", parent_wall_element_id.0))?;

        let snapshot = OpeningSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            opening: Opening {
                width: object
                    .get("width")
                    .map(scalar_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'width'".to_string())?,
                height: object
                    .get("height")
                    .map(scalar_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'height'".to_string())?,
                sill_height: object
                    .get("sill_height")
                    .map(scalar_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'sill_height'".to_string())?,
                kind: parse_opening_kind(
                    object
                        .get("kind")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "Missing or invalid string field 'kind'".to_string())?,
                )?,
            },
            parent_wall,
            parent_wall_element_id,
            position_along_wall: object
                .get("position_along_wall")
                .map(scalar_from_json)
                .transpose()?
                .ok_or_else(|| "Missing required field 'position_along_wall'".to_string())?,
            bim_data: BimData::default(),
        };
        validate_opening_geometry(
            &snapshot.opening,
            &snapshot.parent_wall,
            snapshot.position_along_wall,
        )?;
        Ok(snapshot.into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let (Some(opening), Some(parent_wall)) =
            (entity_ref.get::<Opening>(), entity_ref.get::<ParentWall>())
        else {
            return;
        };
        let Ok(wall_entity_ref) = world.get_entity(parent_wall.wall_entity) else {
            return;
        };
        let Some(wall) = wall_entity_ref.get::<Wall>() else {
            return;
        };
        draw_opening_outline(
            gizmos,
            opening,
            wall,
            parent_wall.position_along_wall,
            color,
        );
    }

    fn selection_line_count(&self, _world: &World, _entity: Entity) -> usize {
        opening_outline_line_count()
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let mut best_hit = None;

        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(opening), Some(parent_wall)) =
                (entity_ref.get::<Opening>(), entity_ref.get::<ParentWall>())
            else {
                continue;
            };
            let Ok(wall_entity_ref) = world.get_entity(parent_wall.wall_entity) else {
                continue;
            };
            let Some(wall) = wall_entity_ref.get::<Wall>() else {
                continue;
            };
            let Some(opening_geometry) =
                opening_geometry(opening, wall, parent_wall.position_along_wall)
            else {
                continue;
            };

            for face_center in [opening_geometry.front_center, opening_geometry.back_center] {
                let Some(hit_point) =
                    intersect_ray_with_plane(ray, face_center, opening_geometry.normal)
                else {
                    continue;
                };
                let horizontal_distance = (hit_point - face_center)
                    .dot(opening_geometry.direction_3d)
                    .abs();
                let vertical_distance = (hit_point.y - face_center.y).abs();
                if horizontal_distance <= opening.width * 0.5 + OPENING_PICK_PADDING_METRES
                    && vertical_distance <= opening.height * 0.5 + OPENING_PICK_PADDING_METRES
                {
                    let distance = ray.origin.distance(hit_point);
                    match best_hit {
                        Some(HitCandidate {
                            distance: best_distance,
                            ..
                        }) if distance >= best_distance => {}
                        _ => {
                            best_hit = Some(HitCandidate {
                                entity: entity_ref.id(),
                                distance,
                            })
                        }
                    }
                }
            }
        }

        best_hit
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut total_opening_count: usize = 0;
        let mut wall_openings: std::collections::HashMap<u64, Vec<u64>> =
            std::collections::HashMap::new();
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let Some(snapshot) = self.capture_snapshot(&entity_ref, world) else {
                continue;
            };
            *summary
                .entity_counts
                .entry("opening".to_string())
                .or_insert(0) += 1;
            total_opening_count += 1;
            wall_openings
                .entry(
                    snapshot
                        .transform_parent()
                        .unwrap_or(snapshot.element_id())
                        .0,
                )
                .or_default()
                .push(snapshot.element_id().0);
            summary.bounding_points.push(snapshot.center());
        }
        if total_opening_count > 0 {
            summary.metrics.insert(
                "total_opening_count".to_string(),
                serde_json::json!(total_opening_count),
            );
            for ids in wall_openings.values_mut() {
                ids.sort_unstable();
            }
            summary.metrics.insert(
                "wall_openings".to_string(),
                serde_json::json!(wall_openings),
            );
        }
    }
}

fn parse_opening_kind(value: &str) -> Result<OpeningKind, String> {
    match value.to_ascii_lowercase().as_str() {
        "window" => Ok(OpeningKind::Window),
        "door" => Ok(OpeningKind::Door),
        _ => Err("Opening kind must be one of: window, door".to_string()),
    }
}

fn wall_point_vec3(point: Vec2) -> Vec3 {
    Vec3::new(point.x, 0.0, point.y)
}

fn wall_point_from_json(value: &Value) -> Result<Vec2, String> {
    vec3_from_json(value)
        .map(|point| point.xz())
        .or_else(|_| vec2_from_json(value))
}

fn validate_wall_geometry(wall: &Wall) -> Result<(), String> {
    if wall.height < 0.1 {
        return Err("Wall height must be at least 0.1m".to_string());
    }
    if wall.thickness < 0.1 {
        return Err("Wall thickness must be at least 0.1m".to_string());
    }
    if wall.start.distance(wall.end) < 0.1 {
        return Err("Wall must be at least 0.1m long".to_string());
    }
    Ok(())
}

fn validate_opening_geometry(
    opening: &Opening,
    parent_wall: &Wall,
    position_along_wall: f32,
) -> Result<(), String> {
    if opening.width < 0.1 {
        return Err("Opening width must be at least 0.1m".to_string());
    }
    if opening.height < 0.1 {
        return Err("Opening height must be at least 0.1m".to_string());
    }
    if opening.sill_height < 0.0 {
        return Err("Opening sill height must be non-negative".to_string());
    }
    if opening.kind == OpeningKind::Door && opening.sill_height.abs() > f32::EPSILON {
        return Err("Door openings must have sill_height 0.0".to_string());
    }
    if opening.sill_height + opening.height > parent_wall.height {
        return Err("Opening must fit within the parent wall height".to_string());
    }
    if !(0.0..=1.0).contains(&position_along_wall) {
        return Err("Opening position_along_wall must be between 0.0 and 1.0".to_string());
    }
    if opening.width > parent_wall.length() {
        return Err("Opening width must not exceed the parent wall length".to_string());
    }
    Ok(())
}

fn wall_axis_scale_factor(direction: Vec2, factor: Vec3) -> f32 {
    Vec2::new(direction.x * factor.x, direction.y * factor.z).length()
}

fn opening_center(wall: &Wall, opening: &Opening, position_along_wall: f32) -> Vec3 {
    let direction = (wall.end - wall.start).normalize_or_zero();
    let center = wall.start
        + direction * (position_along_wall.clamp(0.0, 1.0) * wall.start.distance(wall.end));
    Vec3::new(
        center.x,
        opening.sill_height + opening.height * 0.5,
        center.y,
    )
}

fn opening_preview_mesh(opening: &Opening, wall: &Wall) -> Option<Mesh> {
    if opening.width <= 0.0 || opening.height <= 0.0 || wall.thickness <= 0.0 {
        return None;
    }

    Some(Mesh::from(Cuboid::new(
        opening.width,
        opening.height,
        wall.thickness + OPENING_PREVIEW_DEPTH_BIAS_METRES,
    )))
}

fn opening_preview_transform(
    opening: &Opening,
    wall: &Wall,
    position_along_wall: f32,
) -> Option<Transform> {
    Some(
        Transform::from_translation(opening_center(wall, opening, position_along_wall))
            .with_rotation(wall_rotation(wall)),
    )
}

fn wall_outline_line_count() -> usize {
    12
}

fn opening_outline_line_count() -> usize {
    12
}

fn draw_wall_outline(gizmos: &mut Gizmos, wall: &Wall, color: Color) {
    let direction = wall.direction().unwrap_or(Vec2::X);
    let normal =
        Vec2::new(-direction.y, direction.x) * (wall.thickness * 0.5 + OUTLINE_PADDING_METRES);
    let start_left = wall.start + normal;
    let start_right = wall.start - normal;
    let end_left = wall.end + normal;
    let end_right = wall.end - normal;
    let bottom = [
        Vec3::new(start_left.x, 0.0, start_left.y),
        Vec3::new(end_left.x, 0.0, end_left.y),
        Vec3::new(end_right.x, 0.0, end_right.y),
        Vec3::new(start_right.x, 0.0, start_right.y),
    ];
    let top = bottom.map(|point| point + Vec3::Y * wall.height);

    draw_loop(gizmos, bottom, color);
    draw_loop(gizmos, top, color);
    for index in 0..bottom.len() {
        gizmos.line(bottom[index], top[index], color);
    }
}

fn draw_opening_outline(
    gizmos: &mut Gizmos,
    opening: &Opening,
    wall: &Wall,
    position_along_wall: f32,
    color: Color,
) {
    let Some(opening_geometry) = opening_geometry(opening, wall, position_along_wall) else {
        return;
    };
    draw_loop(gizmos, opening_geometry.front, color);
    draw_loop(gizmos, opening_geometry.back, color);
    for index in 0..opening_geometry.front.len() {
        gizmos.line(
            opening_geometry.front[index],
            opening_geometry.back[index],
            color,
        );
    }
}

struct OpeningGeometry {
    front: [Vec3; 4],
    back: [Vec3; 4],
    direction_3d: Vec3,
    normal: Vec3,
    front_center: Vec3,
    back_center: Vec3,
}

fn opening_geometry(
    opening: &Opening,
    wall: &Wall,
    position_along_wall: f32,
) -> Option<OpeningGeometry> {
    let direction = wall.direction()?;
    let direction_3d = Vec3::new(direction.x, 0.0, direction.y);
    let normal = Vec3::new(-direction.y, 0.0, direction.x);
    let center_distance = position_along_wall.clamp(0.0, 1.0) * wall.length();
    let center = wall.start + direction * center_distance;
    let opening_center = Vec3::new(
        center.x,
        opening.sill_height + opening.height * 0.5,
        center.y,
    );
    let horizontal_offset = direction_3d * (opening.width * 0.5 + OUTLINE_PADDING_METRES);
    let vertical_offset = Vec3::Y * (opening.height * 0.5 + OUTLINE_PADDING_METRES);
    let face_offset = normal * (wall.thickness * 0.5 + OPENING_FACE_OFFSET_METRES);

    Some(OpeningGeometry {
        front: rectangle_corners(
            opening_center + face_offset,
            horizontal_offset,
            vertical_offset,
        ),
        back: rectangle_corners(
            opening_center - face_offset,
            horizontal_offset,
            vertical_offset,
        ),
        direction_3d,
        normal,
        front_center: opening_center + face_offset,
        back_center: opening_center - face_offset,
    })
}

fn bounds_from_points(points: &[Vec3]) -> EntityBounds {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for point in points {
        min = min.min(*point);
        max = max.max(*point);
    }
    EntityBounds { min, max }
}

fn intersect_ray_with_plane(ray: Ray3d, plane_point: Vec3, plane_normal: Vec3) -> Option<Vec3> {
    let denominator = ray.direction.dot(plane_normal);
    if denominator.abs() <= f32::EPSILON {
        return None;
    }

    let distance = (plane_point - ray.origin).dot(plane_normal) / denominator;
    (distance >= 0.0).then_some(ray.get_point(distance))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_property_fields_include_read_only_length_and_vec3_endpoints() {
        let snapshot = WallSnapshot {
            element_id: ElementId(9),
            wall: Wall {
                start: Vec2::new(1.0, 2.0),
                end: Vec2::new(5.0, 2.0),
                height: 3.0,
                thickness: 0.2,
            },
            bim_data: BimData::default(),
        };

        let fields = snapshot.property_fields();
        assert_eq!(fields[0].name, "start");
        assert_eq!(fields[0].kind, PropertyValueKind::Vec3);
        assert_eq!(
            fields[0].value,
            Some(PropertyValue::Vec3(Vec3::new(1.0, 0.0, 2.0)))
        );
        assert!(fields[0].editable);

        let length = fields
            .iter()
            .find(|field| field.name == "length")
            .expect("wall should expose computed length");
        assert_eq!(length.value, Some(PropertyValue::Scalar(4.0)));
        assert!(!length.editable);
    }

    #[test]
    fn wall_apply_with_previous_skips_remesh_for_translate_only_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(10),
                Wall {
                    start: Vec2::new(0.0, 0.0),
                    end: Vec2::new(4.0, 0.0),
                    height: 3.0,
                    thickness: 0.2,
                },
                BimData::default(),
                Transform::IDENTITY,
            ))
            .id();

        let previous = WallSnapshot {
            element_id: ElementId(10),
            wall: Wall {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(4.0, 0.0),
                height: 3.0,
                thickness: 0.2,
            },
            bim_data: BimData::default(),
        };
        let updated = WallSnapshot {
            element_id: ElementId(10),
            wall: Wall {
                start: Vec2::new(2.0, 1.0),
                end: Vec2::new(6.0, 1.0),
                height: 3.0,
                thickness: 0.2,
            },
            bim_data: BimData::default(),
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(!entity_ref.contains::<NeedsMesh>());
        let transform = entity_ref
            .get::<Transform>()
            .expect("translated wall should keep a transform");
        assert_eq!(
            transform.translation,
            wall_transform(&updated.wall).translation
        );
    }

    #[test]
    fn wall_apply_with_previous_marks_remesh_for_length_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(11),
                Wall {
                    start: Vec2::new(0.0, 0.0),
                    end: Vec2::new(4.0, 0.0),
                    height: 3.0,
                    thickness: 0.2,
                },
                BimData::default(),
            ))
            .id();

        let previous = WallSnapshot {
            element_id: ElementId(11),
            wall: Wall {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(4.0, 0.0),
                height: 3.0,
                thickness: 0.2,
            },
            bim_data: BimData::default(),
        };
        let updated = WallSnapshot {
            element_id: ElementId(11),
            wall: Wall {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(6.0, 0.0),
                height: 3.0,
                thickness: 0.2,
            },
            bim_data: BimData::default(),
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(entity_ref.contains::<NeedsMesh>());
    }

    #[test]
    fn opening_apply_to_marks_parent_wall_for_remesh() {
        let mut world = World::new();
        let parent_wall_entity = world
            .spawn((
                ElementId(20),
                Wall {
                    start: Vec2::new(0.0, 0.0),
                    end: Vec2::new(4.0, 0.0),
                    height: 3.0,
                    thickness: 0.2,
                },
                BimData::default(),
            ))
            .id();

        let snapshot = OpeningSnapshot {
            element_id: ElementId(21),
            opening: Opening {
                width: 1.2,
                height: 1.5,
                sill_height: 0.9,
                kind: OpeningKind::Window,
            },
            parent_wall: Wall {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(4.0, 0.0),
                height: 3.0,
                thickness: 0.2,
            },
            parent_wall_element_id: ElementId(20),
            position_along_wall: 0.5,
            bim_data: BimData::default(),
        };

        snapshot.apply_to(&mut world);

        let wall_entity_ref = world.entity(parent_wall_entity);
        assert!(
            wall_entity_ref.contains::<NeedsMesh>(),
            "opening edits must invalidate the parent wall mesh exactly once on commit",
        );
    }

    #[test]
    fn door_opening_validation_requires_zero_sill_height() {
        let parent_wall = Wall {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let door = Opening {
            width: 0.9,
            height: 2.1,
            sill_height: 0.2,
            kind: OpeningKind::Door,
        };

        let error = validate_opening_geometry(&door, &parent_wall, 0.5).unwrap_err();

        assert_eq!(error, "Door openings must have sill_height 0.0");
    }

    #[test]
    fn door_opening_validation_accepts_zero_sill_height() {
        let parent_wall = Wall {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let door = Opening {
            width: 0.9,
            height: 2.1,
            sill_height: 0.0,
            kind: OpeningKind::Door,
        };

        validate_opening_geometry(&door, &parent_wall, 0.5).unwrap();
    }
}
