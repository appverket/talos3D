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
    capability_registry::{AuthoredEntityFactory, HitCandidate, ModelSummaryAccumulator},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
        math::scale_point_around_center,
    },
};

use crate::components::{
    ElevationCurve, ElevationCurveType, NeedsTerrainMesh, TerrainMeshCache, TerrainSurface,
};

const CURVE_SELECTION_RADIUS_METRES: f32 = 0.2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElevationCurveSnapshot {
    pub element_id: ElementId,
    pub curve: ElevationCurve,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainSurfaceSnapshot {
    pub element_id: ElementId,
    pub surface: TerrainSurface,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum TerrainSnapshotJson {
    ElevationCurve(ElevationCurveSnapshot),
    TerrainSurface(TerrainSurfaceSnapshot),
}

pub struct ElevationCurveFactory;
pub struct TerrainSurfaceFactory;

impl From<ElevationCurveSnapshot> for BoxedEntity {
    fn from(snapshot: ElevationCurveSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl From<TerrainSurfaceSnapshot> for BoxedEntity {
    fn from(snapshot: TerrainSurfaceSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl AuthoredEntity for ElevationCurveSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "elevation_curve"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!(
            "Elevation Curve {:.2}m ({})",
            self.curve.elevation, self.curve.source_layer
        )
    }

    fn center(&self) -> Vec3 {
        points_center(&self.curve.points).unwrap_or(Vec3::ZERO)
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        for point in &mut snapshot.curve.points {
            *point += delta;
        }
        snapshot.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let center = self.center();
        let mut snapshot = self.clone();
        for point in &mut snapshot.curve.points {
            *point = center + rotation * (*point - center);
        }
        snapshot.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        for point in &mut snapshot.curve.points {
            *point = scale_point_around_center(*point, center, factor);
        }
        snapshot.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field(
                "elevation",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.curve.elevation)),
            ),
            property_field_with(
                "source_layer",
                "source layer",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.curve.source_layer.clone())),
                true,
            ),
            property_field_with(
                "curve_type",
                "curve type",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(
                    self.curve.curve_type.as_str().to_string(),
                )),
                true,
            ),
            read_only_property_field(
                "point_count",
                "point count",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.curve.points.len() as f32)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "elevation" => snapshot.curve.elevation = scalar_from_json(value)?,
            "source_layer" => {
                snapshot.curve.source_layer = value
                    .as_str()
                    .ok_or_else(|| "Expected string value".to_string())?
                    .to_string();
            }
            "curve_type" => {
                snapshot.curve.curve_type = parse_curve_type(
                    value
                        .as_str()
                        .ok_or_else(|| "Expected string value".to_string())?,
                )?;
            }
            _ => {
                return Err(invalid_property_error(
                    "elevation_curve",
                    &["elevation", "source_layer", "curve_type"],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        self.curve
            .points
            .iter()
            .enumerate()
            .map(|(index, position)| HandleInfo {
                id: format!("point_{index}"),
                position: *position,
                kind: HandleKind::Vertex,
                label: format!("Point {}", index + 1),
            })
            .collect()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        bounds_from_points(&self.curve.points)
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(TerrainSnapshotJson::ElevationCurve(self.clone()))
            .unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert(self.curve.clone());
            return;
        }

        world.spawn((self.element_id, self.curve.clone()));
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        for segment in self.curve.points.windows(2) {
            gizmos.line(segment[0], segment[1], color);
        }
    }

    fn preview_line_count(&self) -> usize {
        self.curve.points.len().saturating_sub(1)
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl AuthoredEntity for TerrainSurfaceSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "terrain_surface"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        if self.surface.name.is_empty() {
            "Terrain Surface".to_string()
        } else {
            self.surface.name.clone()
        }
    }

    fn center(&self) -> Vec3 {
        if self.surface.boundary.is_empty() {
            self.surface.offset
        } else {
            let (min, max) = boundary_bounds(&self.surface.boundary);
            Vec3::new(
                (min.x + max.x) * 0.5,
                self.surface.datum_elevation + self.surface.offset.y,
                (min.y + max.y) * 0.5,
            )
        }
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.surface.offset += delta;
        for point in &mut snapshot.surface.boundary {
            *point += delta.xz();
        }
        snapshot.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut snapshot = self.clone();
        let center = self.center();
        for point in &mut snapshot.surface.boundary {
            let p3d = Vec3::new(point.x, center.y, point.y);
            let rotated = center + rotation * (p3d - center);
            *point = Vec2::new(rotated.x, rotated.z);
        }
        snapshot.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.surface.offset =
            scale_point_around_center(snapshot.surface.offset, center, factor);
        let center_2d = center.xz();
        for point in &mut snapshot.surface.boundary {
            let scaled = Vec2::new(
                center_2d.x + (point.x - center_2d.x) * factor.x,
                center_2d.y + (point.y - center_2d.y) * factor.z,
            );
            *point = scaled;
        }
        snapshot.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field(
                "datum_elevation",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.surface.datum_elevation)),
            ),
            property_field_with(
                "name",
                "name",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.surface.name.clone())),
                true,
            ),
            property_field(
                "contour_interval",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.surface.contour_interval)),
            ),
            property_field(
                "drape_sample_spacing",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.surface.drape_sample_spacing)),
            ),
            property_field(
                "offset",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.surface.offset)),
            ),
            read_only_property_field(
                "source_curve_count",
                "source curve count",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(
                    self.surface.source_curve_ids.len() as f32
                )),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "datum_elevation" => snapshot.surface.datum_elevation = scalar_from_json(value)?,
            "name" => {
                snapshot.surface.name = value
                    .as_str()
                    .ok_or_else(|| "Expected string value".to_string())?
                    .to_string();
            }
            "contour_interval" => snapshot.surface.contour_interval = scalar_from_json(value)?,
            "drape_sample_spacing" => {
                snapshot.surface.drape_sample_spacing = scalar_from_json(value)?
            }
            "offset" => snapshot.surface.offset = vec3_from_json(value)?,
            _ => {
                return Err(invalid_property_error(
                    "terrain_surface",
                    &[
                        "datum_elevation",
                        "name",
                        "contour_interval",
                        "drape_sample_spacing",
                        "offset",
                    ],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        let mut handles = self
            .surface
            .boundary
            .iter()
            .enumerate()
            .map(|(index, point)| HandleInfo {
                id: format!("boundary_{index}"),
                position: Vec3::new(point.x, self.surface.datum_elevation, point.y),
                kind: HandleKind::Vertex,
                label: format!("Boundary {}", index + 1),
            })
            .collect::<Vec<_>>();
        handles.push(HandleInfo {
            id: "offset".to_string(),
            position: self.surface.offset,
            kind: HandleKind::Center,
            label: "Offset".to_string(),
        });
        handles
    }

    fn bounds(&self) -> Option<EntityBounds> {
        let mut points = self
            .surface
            .boundary
            .iter()
            .map(|point| Vec3::new(point.x, self.surface.datum_elevation, point.y))
            .collect::<Vec<_>>();
        if points.is_empty() {
            points.push(self.surface.offset);
        }
        bounds_from_points(&points)
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(TerrainSnapshotJson::TerrainSurface(self.clone()))
            .unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world
                .entity_mut(entity)
                .insert((self.surface.clone(), NeedsTerrainMesh));
            return;
        }

        world.spawn((self.element_id, self.surface.clone(), NeedsTerrainMesh));
    }

    fn remove_from(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mesh_id = world
                .get_entity(entity)
                .ok()
                .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh_3d| mesh_3d.id()));
            if let Some(mesh_id) = mesh_id {
                world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
            }
        }
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        for segment in self.surface.boundary.windows(2) {
            let start = Vec3::new(segment[0].x, self.surface.datum_elevation, segment[0].y);
            let end = Vec3::new(segment[1].x, self.surface.datum_elevation, segment[1].y);
            gizmos.line(start, end, color);
        }
        if let (Some(first), Some(last)) =
            (self.surface.boundary.first(), self.surface.boundary.last())
        {
            gizmos.line(
                Vec3::new(last.x, self.surface.datum_elevation, last.y),
                Vec3::new(first.x, self.surface.datum_elevation, first.y),
                color,
            );
        }
    }

    fn preview_line_count(&self) -> usize {
        self.surface.boundary.len()
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl AuthoredEntityFactory for ElevationCurveFactory {
    fn type_name(&self) -> &'static str {
        "elevation_curve"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        Some(
            ElevationCurveSnapshot {
                element_id: *entity_ref.get::<ElementId>()?,
                curve: entity_ref.get::<ElevationCurve>()?.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<TerrainSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            TerrainSnapshotJson::ElevationCurve(snapshot) => Ok(snapshot.into()),
            TerrainSnapshotJson::TerrainSurface(_) => {
                Err("Snapshot JSON did not match the expected entity type".to_string())
            }
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let points = point_array(
            object
                .get("points")
                .ok_or_else(|| "Missing required field 'points'".to_string())?,
        )?;
        if points.len() < 2 {
            return Err("Elevation curve requires at least two points".to_string());
        }
        Ok(ElevationCurveSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            curve: ElevationCurve {
                points,
                elevation: object
                    .get("elevation")
                    .map(scalar_from_json)
                    .transpose()?
                    .ok_or_else(|| "Missing required field 'elevation'".to_string())?,
                source_layer: object
                    .get("source_layer")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Missing required field 'source_layer'".to_string())?
                    .to_string(),
                curve_type: object
                    .get("curve_type")
                    .and_then(Value::as_str)
                    .map(parse_curve_type)
                    .transpose()?
                    .unwrap_or_default(),
                survey_source_id: object
                    .get("survey_source_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(curve) = entity_ref.get::<ElevationCurve>() else {
            return;
        };
        for segment in curve.points.windows(2) {
            gizmos.line(segment[0], segment[1], color);
        }
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        entity_ref
            .get::<ElevationCurve>()
            .map_or(0, |curve| curve.points.len().saturating_sub(1))
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let cursor = ray.origin.xz();
        let mut best_hit = None;
        let mut best_distance = CURVE_SELECTION_RADIUS_METRES;

        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let Some(curve) = entity_ref.get::<ElevationCurve>() else {
                continue;
            };
            for segment in curve.points.windows(2) {
                let distance = distance_to_segment(cursor, segment[0].xz(), segment[1].xz());
                if distance < best_distance {
                    best_distance = distance;
                    best_hit = Some(HitCandidate {
                        entity: entity_ref.id(),
                        distance,
                    });
                }
            }
        }

        best_hit
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(element_id), Some(curve)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<ElevationCurve>(),
            ) else {
                continue;
            };
            *summary
                .entity_counts
                .entry("elevation_curve".to_string())
                .or_insert(0) += 1;
            summary.bounding_points.push(
                ElevationCurveSnapshot {
                    element_id: *element_id,
                    curve: curve.clone(),
                }
                .center(),
            );
        }
    }
}

impl AuthoredEntityFactory for TerrainSurfaceFactory {
    fn type_name(&self) -> &'static str {
        "terrain_surface"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        Some(
            TerrainSurfaceSnapshot {
                element_id: *entity_ref.get::<ElementId>()?,
                surface: entity_ref.get::<TerrainSurface>()?.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<TerrainSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            TerrainSnapshotJson::TerrainSurface(snapshot) => Ok(snapshot.into()),
            TerrainSnapshotJson::ElevationCurve(_) => {
                Err("Snapshot JSON did not match the expected entity type".to_string())
            }
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        Ok(TerrainSurfaceSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            surface: TerrainSurface {
                name: object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Terrain Surface")
                    .to_string(),
                source_curve_ids: element_id_array(
                    object
                        .get("source_curve_ids")
                        .ok_or_else(|| "Missing required field 'source_curve_ids'".to_string())?,
                )?,
                datum_elevation: object
                    .get("datum_elevation")
                    .map(scalar_from_json)
                    .transpose()?
                    .unwrap_or_default(),
                boundary: object
                    .get("boundary")
                    .map(vec2_array)
                    .transpose()?
                    .unwrap_or_default(),
                max_triangle_area: object
                    .get("max_triangle_area")
                    .map(scalar_from_json)
                    .transpose()?
                    .unwrap_or(crate::components::DEFAULT_TERRAIN_MAX_TRIANGLE_AREA),
                minimum_angle: object
                    .get("minimum_angle")
                    .map(scalar_from_json)
                    .transpose()?
                    .unwrap_or(crate::components::DEFAULT_TERRAIN_MINIMUM_ANGLE),
                contour_interval: object
                    .get("contour_interval")
                    .map(scalar_from_json)
                    .transpose()?
                    .unwrap_or(crate::components::DEFAULT_TERRAIN_CONTOUR_INTERVAL),
                drape_sample_spacing: object
                    .get("drape_sample_spacing")
                    .map(scalar_from_json)
                    .transpose()?
                    .unwrap_or(crate::components::DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING),
                offset: object
                    .get("offset")
                    .map(vec3_from_json)
                    .transpose()?
                    .unwrap_or(Vec3::ZERO),
            },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(cache) = entity_ref.get::<TerrainMeshCache>() else {
            return;
        };
        for face in &cache.mesh.faces {
            let Some((a, b, c)) = triangle_from_face(&cache.mesh.vertices, *face) else {
                continue;
            };
            gizmos.line(a, b, color);
            gizmos.line(b, c, color);
            gizmos.line(c, a, color);
        }
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        entity_ref
            .get::<TerrainMeshCache>()
            .map_or(0, |cache| cache.mesh.faces.len() * 3)
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(element_id), Some(surface)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<TerrainSurface>(),
            ) else {
                continue;
            };
            *summary
                .entity_counts
                .entry("terrain_surface".to_string())
                .or_insert(0) += 1;
            summary.bounding_points.push(
                TerrainSurfaceSnapshot {
                    element_id: *element_id,
                    surface: surface.clone(),
                }
                .center(),
            );
        }
    }
}

fn point_array(value: &Value) -> Result<Vec<Vec3>, String> {
    value
        .as_array()
        .ok_or_else(|| "Expected an array of points".to_string())?
        .iter()
        .map(vec3_from_json)
        .collect()
}

fn vec2_array(value: &Value) -> Result<Vec<Vec2>, String> {
    value
        .as_array()
        .ok_or_else(|| "Expected an array of Vec2 points".to_string())?
        .iter()
        .map(vec2_from_json)
        .collect()
}

fn element_id_array(value: &Value) -> Result<Vec<ElementId>, String> {
    value
        .as_array()
        .ok_or_else(|| "Expected an array of element ids".to_string())?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .map(ElementId)
                .ok_or_else(|| "Element ids must be unsigned integers".to_string())
        })
        .collect()
}

fn parse_curve_type(value: &str) -> Result<ElevationCurveType, String> {
    match value {
        "major" => Ok(ElevationCurveType::Major),
        "minor" => Ok(ElevationCurveType::Minor),
        "index" => Ok(ElevationCurveType::Index),
        "supplementary" => Ok(ElevationCurveType::Supplementary),
        _ => Err("curve_type must be one of: major, minor, index, supplementary".to_string()),
    }
}

fn points_center(points: &[Vec3]) -> Option<Vec3> {
    bounds_from_points(points).map(|bounds| bounds.center())
}

fn bounds_from_points(points: &[Vec3]) -> Option<EntityBounds> {
    let mut iter = points.iter().copied();
    let first = iter.next()?;
    let mut min = first;
    let mut max = first;
    for point in iter {
        min = min.min(point);
        max = max.max(point);
    }
    Some(EntityBounds { min, max })
}

fn boundary_bounds(boundary: &[Vec2]) -> (Vec2, Vec2) {
    let mut iter = boundary.iter().copied();
    let first = iter.next().unwrap_or(Vec2::ZERO);
    let mut min = first;
    let mut max = first;
    for point in iter {
        min = min.min(point);
        max = max.max(point);
    }
    (min, max)
}

fn distance_to_segment(point: Vec2, start: Vec2, end: Vec2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_squared();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let projection = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    let closest = start + segment * projection;
    point.distance(closest)
}

fn triangle_from_face(vertices: &[Vec3], face: [u32; 3]) -> Option<(Vec3, Vec3, Vec3)> {
    Some((
        *vertices.get(face[0] as usize)?,
        *vertices.get(face[1] as usize)?,
        *vertices.get(face[2] as usize)?,
    ))
}
