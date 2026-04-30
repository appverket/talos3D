use std::any::Any;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field, scalar_from_json, vec3_from_json, AuthoredEntity,
        BoxedEntity, EntityBounds, HandleInfo, HandleKind, PropertyFieldDef, PropertyValue,
        PropertyValueKind, PushPullAffordance, PushPullBlockReason,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityRegistry, FaceHitCandidate, FaceId, HitCandidate,
        ModelSummaryAccumulator,
    },
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
        layers::LayerAssignment,
        materials::{
            material_assignment_display_id, material_assignment_from_value,
            material_assignment_option_from_value, MaterialAssignment,
        },
        math::scale_point_around_center,
        modeling::{
            editable_mesh::{EditableMesh, OperationLog, OperationOrigin},
            mesh_generation::NeedsMesh,
            primitives::{
                BoxPrimitive, CylinderPrimitive, ElevationMetadata, PlanePrimitive, Polyline,
                ShapeRotation, SpherePrimitive, TriangleMesh,
            },
        },
    },
};

const POLYLINE_SELECTION_RADIUS_METRES: f32 = 0.2;
const POLYLINE_SELECTION_SCREEN_FRACTION: f32 = 0.008;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolylineSnapshot {
    pub element_id: ElementId,
    pub primitive: Polyline,
    pub layer: Option<String>,
    pub elevation_metadata: Option<ElevationMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_assignment: Option<MaterialAssignment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriangleMeshSnapshot {
    pub element_id: ElementId,
    pub primitive: TriangleMesh,
    pub layer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_assignment: Option<MaterialAssignment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditableMeshSnapshot {
    pub element_id: ElementId,
    pub mesh: super::editable_mesh::EditableMesh,
    /// Operation log preserving parametric provenance. None for meshes created directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_log: Option<super::editable_mesh::OperationLog>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_assignment: Option<MaterialAssignment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum ModelingSnapshotJson {
    Polyline(PolylineSnapshot),
    TriangleMesh(TriangleMeshSnapshot),
    EditableMesh(EditableMeshSnapshot),
}

pub struct PolylineFactory;
pub struct EditableMeshFactory;
pub struct TriangleMeshFactory;

impl From<PolylineSnapshot> for BoxedEntity {
    fn from(snapshot: PolylineSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl From<TriangleMeshSnapshot> for BoxedEntity {
    fn from(snapshot: TriangleMeshSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl From<EditableMeshSnapshot> for BoxedEntity {
    fn from(snapshot: EditableMeshSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// Box, Cylinder, and Plane snapshots are now handled by the generic
// PrimitiveSnapshot<T> in generic_snapshot.rs.  Their factories live in
// generic_factory.rs as PrimitiveFactory<T>.
//
// Below: PolylineSnapshot, TriangleMeshSnapshot, and EditableMeshSnapshot
// remain manually implemented because they have non-standard component sets
// (e.g. layer, elevation metadata, operation log).

impl AuthoredEntity for PolylineSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "polyline"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("Polyline ({} points)", self.primitive.points.len())
    }

    fn center(&self) -> Vec3 {
        self.primitive
            .points
            .iter()
            .copied()
            .fold(Vec3::ZERO, |acc, point| acc + point)
            / self.primitive.points.len().max(1) as f32
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        PolylineSnapshot {
            element_id: self.element_id,
            primitive: Polyline {
                points: self
                    .primitive
                    .points
                    .iter()
                    .map(|point| *point + delta)
                    .collect(),
            },
            layer: self.layer.clone(),
            elevation_metadata: self.elevation_metadata.clone(),
            material_assignment: self.material_assignment.clone(),
        }
        .into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let center = self.center();
        PolylineSnapshot {
            element_id: self.element_id,
            primitive: Polyline {
                points: self
                    .primitive
                    .points
                    .iter()
                    .map(|point| center + rotation * (*point - center))
                    .collect(),
            },
            layer: self.layer.clone(),
            elevation_metadata: self.elevation_metadata.clone(),
            material_assignment: self.material_assignment.clone(),
        }
        .into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        PolylineSnapshot {
            element_id: self.element_id,
            primitive: Polyline {
                points: self
                    .primitive
                    .points
                    .iter()
                    .map(|point| scale_point_around_center(*point, center, factor))
                    .collect(),
            },
            layer: self.layer.clone(),
            elevation_metadata: self.elevation_metadata.clone(),
            material_assignment: self.material_assignment.clone(),
        }
        .into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let mut fields = Vec::new();
        if let Some(layer) = &self.layer {
            fields.push(property_field(
                "layer",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(layer.clone())),
            ));
        }
        if let Some(elevation) = &self.elevation_metadata {
            fields.push(property_field(
                "elevation",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(elevation.elevation)),
            ));
            fields.push(property_field(
                "survey_source_id",
                PropertyValueKind::Text,
                elevation.survey_source_id.clone().map(PropertyValue::Text),
            ));
        }
        fields.push(property_field(
            "material",
            PropertyValueKind::Text,
            material_assignment_display_id(self.material_assignment.as_ref())
                .map(PropertyValue::Text),
        ));
        fields
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        if matches!(property_name, "material" | "material_assignment") {
            return self.set_material_assignment(material_assignment_option_from_value(value)?);
        }
        let mut snapshot = self.clone();
        match property_name {
            "points" => snapshot.primitive.points = point_array(value)?,
            "layer" => {
                snapshot.layer = match value {
                    Value::Null => None,
                    Value::String(value) => Some(value.clone()),
                    _ => return Err("Polyline layer must be a string or null".to_string()),
                };
            }
            "elevation" => {
                let Some(metadata) = snapshot.elevation_metadata.as_mut() else {
                    return Err("Polyline does not have elevation metadata".to_string());
                };
                metadata.elevation = scalar_from_json(value)?;
            }
            "survey_source_id" => {
                let Some(metadata) = snapshot.elevation_metadata.as_mut() else {
                    return Err("Polyline does not have elevation metadata".to_string());
                };
                metadata.survey_source_id = match value {
                    Value::Null => None,
                    Value::String(value) => Some(value.clone()),
                    _ => {
                        return Err("Polyline survey source ID must be a string or null".to_string())
                    }
                };
            }
            _ => {
                return Err(invalid_property_error(
                    "polyline",
                    &["points", "layer", "elevation", "survey_source_id"],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn material_assignment(&self) -> Option<MaterialAssignment> {
        self.material_assignment.clone()
    }

    fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        snapshot.material_assignment = assignment;
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        self.primitive
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| HandleInfo {
                id: format!("vertex_{index}"),
                position: *point,
                kind: HandleKind::Vertex,
                label: format!("Vertex {}", index + 1),
            })
            .collect()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        (!self.primitive.points.is_empty()).then(|| bounds_from_points(&self.primitive.points))
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let index = handle_id.strip_prefix("vertex_")?.parse::<usize>().ok()?;
        let mut snapshot = self.clone();
        let point = snapshot.primitive.points.get_mut(index)?;
        *point = Vec3::new(cursor.x, point.y, cursor.z);
        Some(snapshot.into())
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(ModelingSnapshotJson::Polyline(self.clone())).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        let aabb = if self.primitive.points.is_empty() {
            None
        } else {
            Some(aabb_from_points(&self.primitive.points))
        };
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mut entity = world.entity_mut(entity);
            entity.insert((self.primitive.clone(), Visibility::Visible));
            if let Some(aabb) = aabb {
                entity.insert(aabb);
            }
            if let Some(layer) = &self.layer {
                entity.insert(LayerAssignment::new(layer));
            } else {
                entity.remove::<LayerAssignment>();
            }
            if let Some(metadata) = &self.elevation_metadata {
                entity.insert(metadata.clone());
            } else {
                entity.remove::<ElevationMetadata>();
            }
            if let Some(material_assignment) = &self.material_assignment {
                entity.insert(material_assignment.clone());
            } else {
                entity.remove::<MaterialAssignment>();
            }
        } else {
            let mut entity = world.spawn((
                self.element_id,
                self.primitive.clone(),
                Visibility::Visible,
                Transform::IDENTITY,
            ));
            if let Some(aabb) = aabb {
                entity.insert(aabb);
            }
            if let Some(layer) = &self.layer {
                entity.insert(LayerAssignment::new(layer));
            }
            if let Some(metadata) = &self.elevation_metadata {
                entity.insert(metadata.clone());
            }
            if let Some(material_assignment) = &self.material_assignment {
                entity.insert(material_assignment.clone());
            }
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(Transform::IDENTITY)
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_polyline_outline(gizmos, &self.primitive, color);
    }

    fn preview_line_count(&self) -> usize {
        polyline_outline_line_count(&self.primitive)
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl AuthoredEntity for TriangleMeshSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "triangle_mesh"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.primitive
            .name
            .clone()
            .unwrap_or_else(|| format!("Triangle Mesh ({} faces)", self.primitive.faces.len()))
    }

    fn center(&self) -> Vec3 {
        self.primitive
            .vertices
            .iter()
            .copied()
            .fold(Vec3::ZERO, |acc, vertex| acc + vertex)
            / self.primitive.vertices.len().max(1) as f32
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        TriangleMeshSnapshot {
            element_id: self.element_id,
            primitive: TriangleMesh {
                vertices: self
                    .primitive
                    .vertices
                    .iter()
                    .map(|vertex| *vertex + delta)
                    .collect(),
                faces: self.primitive.faces.clone(),
                normals: self.primitive.normals.clone(),
                name: self.primitive.name.clone(),
            },
            layer: self.layer.clone(),
            material_assignment: self.material_assignment.clone(),
        }
        .into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let center = self.center();
        TriangleMeshSnapshot {
            element_id: self.element_id,
            primitive: TriangleMesh {
                vertices: self
                    .primitive
                    .vertices
                    .iter()
                    .map(|vertex| center + rotation * (*vertex - center))
                    .collect(),
                faces: self.primitive.faces.clone(),
                normals: self.primitive.normals.clone().map(|normals| {
                    normals
                        .into_iter()
                        .map(|normal| rotation * normal)
                        .collect()
                }),
                name: self.primitive.name.clone(),
            },
            layer: self.layer.clone(),
            material_assignment: self.material_assignment.clone(),
        }
        .into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        TriangleMeshSnapshot {
            element_id: self.element_id,
            primitive: TriangleMesh {
                vertices: self
                    .primitive
                    .vertices
                    .iter()
                    .map(|vertex| scale_point_around_center(*vertex, center, factor))
                    .collect(),
                faces: self.primitive.faces.clone(),
                normals: None,
                name: self.primitive.name.clone(),
            },
            layer: self.layer.clone(),
            material_assignment: self.material_assignment.clone(),
        }
        .into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let mut fields = vec![property_field(
            "name",
            PropertyValueKind::Text,
            self.primitive.name.clone().map(PropertyValue::Text),
        )];
        if let Some(layer) = &self.layer {
            fields.push(property_field(
                "layer",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(layer.clone())),
            ));
        }
        fields.push(property_field(
            "material",
            PropertyValueKind::Text,
            material_assignment_display_id(self.material_assignment.as_ref())
                .map(PropertyValue::Text),
        ));
        fields
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        if matches!(property_name, "material" | "material_assignment") {
            return self.set_material_assignment(material_assignment_option_from_value(value)?);
        }
        let mut snapshot = self.clone();
        match property_name {
            "name" => {
                snapshot.primitive.name = match value {
                    Value::Null => None,
                    Value::String(value) => Some(value.clone()),
                    _ => return Err("Triangle mesh name must be a string or null".to_string()),
                };
            }
            "layer" => {
                snapshot.layer = match value {
                    Value::Null => None,
                    Value::String(value) => Some(value.clone()),
                    _ => return Err("Triangle mesh layer must be a string or null".to_string()),
                };
            }
            _ => return Err(invalid_property_error("triangle_mesh", &["name", "layer"])),
        }
        Ok(snapshot.into())
    }

    fn material_assignment(&self) -> Option<MaterialAssignment> {
        self.material_assignment.clone()
    }

    fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        snapshot.material_assignment = assignment;
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        self.primitive
            .vertices
            .iter()
            .enumerate()
            .map(|(index, vertex)| HandleInfo {
                id: format!("vertex_{index}"),
                position: *vertex,
                kind: HandleKind::Vertex,
                label: format!("Vertex {}", index + 1),
            })
            .collect()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        triangle_mesh_bounds(&self.primitive).map(|(min, max)| EntityBounds { min, max })
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let index = handle_id.strip_prefix("vertex_")?.parse::<usize>().ok()?;
        let mut snapshot = self.clone();
        let vertex = snapshot.primitive.vertices.get_mut(index)?;
        *vertex = Vec3::new(cursor.x, vertex.y, cursor.z);
        snapshot.primitive.normals = None;
        Some(snapshot.into())
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(ModelingSnapshotJson::TriangleMesh(self.clone()))
            .unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mut entity = world.entity_mut(entity);
            entity.insert((self.primitive.clone(), NeedsMesh, Visibility::Visible));
            if let Some(layer) = &self.layer {
                entity.insert(LayerAssignment::new(layer));
            } else {
                entity.remove::<LayerAssignment>();
            }
            if let Some(material_assignment) = &self.material_assignment {
                entity.insert(material_assignment.clone());
            } else {
                entity.remove::<MaterialAssignment>();
            }
        } else {
            let mut entity = world.spawn((
                self.element_id,
                self.primitive.clone(),
                NeedsMesh,
                Visibility::Visible,
            ));
            if let Some(layer) = &self.layer {
                entity.insert(LayerAssignment::new(layer));
            }
            if let Some(material_assignment) = &self.material_assignment {
                entity.insert(material_assignment.clone());
            }
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(Transform::IDENTITY)
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_triangle_mesh_outline(gizmos, &self.primitive, color);
    }

    fn preview_line_count(&self) -> usize {
        triangle_mesh_outline_line_count(&self.primitive)
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl AuthoredEntityFactory for PolylineFactory {
    fn type_name(&self) -> &'static str {
        "polyline"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let primitive = entity_ref.get::<Polyline>()?;
        Some(
            PolylineSnapshot {
                element_id,
                primitive: primitive.clone(),
                layer: entity_ref.get::<LayerAssignment>().map(|a| a.layer.clone()),
                elevation_metadata: entity_ref.get::<ElevationMetadata>().cloned(),
                material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<ModelingSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            ModelingSnapshotJson::Polyline(snapshot) => Ok(snapshot.into()),
            _ => Err("Snapshot JSON did not match the expected entity type".to_string()),
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let snapshot = PolylineSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            primitive: Polyline {
                points: point_array(
                    object
                        .get("points")
                        .ok_or_else(|| "Missing required field 'points'".to_string())?,
                )?,
            },
            layer: object
                .get("layer")
                .map(layer_from_json)
                .transpose()?
                .flatten(),
            elevation_metadata: object
                .get("elevation_metadata")
                .map(elevation_metadata_from_json)
                .transpose()?
                .flatten(),
            material_assignment: object
                .get("material_assignment")
                .and_then(material_assignment_from_value),
        };
        if snapshot.primitive.points.len() < 2 {
            return Err("Polyline must have at least 2 points".to_string());
        }
        Ok(snapshot.into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(primitive) = entity_ref.get::<Polyline>() else {
            return;
        };
        draw_polyline_outline(gizmos, primitive, color);
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        let Some(primitive) = entity_ref.get::<Polyline>() else {
            return 0;
        };
        polyline_outline_line_count(primitive)
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let mut best_hit = None;
        let mut best_distance = f32::MAX;

        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let Some(polyline) = entity_ref.get::<Polyline>() else {
                continue;
            };
            for segment in polyline.points.windows(2) {
                let (dist_to_segment, ray_t) =
                    ray_segment_closest_distance(ray, segment[0], segment[1]);
                // Scale the selection threshold by distance from camera so
                // clicking remains easy when zoomed out.
                let threshold = POLYLINE_SELECTION_RADIUS_METRES
                    .max(ray_t * POLYLINE_SELECTION_SCREEN_FRACTION);
                if dist_to_segment < threshold && ray_t < best_distance {
                    best_distance = ray_t;
                    best_hit = Some(HitCandidate {
                        entity: entity_ref.id(),
                        distance: ray_t,
                    });
                }
            }
        }

        best_hit
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(element_id), Some(primitive)) =
                (entity_ref.get::<ElementId>(), entity_ref.get::<Polyline>())
            else {
                continue;
            };
            *summary
                .entity_counts
                .entry("polyline".to_string())
                .or_insert(0) += 1;
            summary.bounding_points.push(
                PolylineSnapshot {
                    element_id: *element_id,
                    primitive: primitive.clone(),
                    layer: entity_ref.get::<LayerAssignment>().map(|a| a.layer.clone()),
                    elevation_metadata: entity_ref.get::<ElevationMetadata>().cloned(),
                    material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
                }
                .center(),
            );
        }
    }
}

impl AuthoredEntityFactory for TriangleMeshFactory {
    fn type_name(&self) -> &'static str {
        "triangle_mesh"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let primitive = entity_ref.get::<TriangleMesh>()?;
        Some(
            TriangleMeshSnapshot {
                element_id,
                primitive: primitive.clone(),
                layer: entity_ref.get::<LayerAssignment>().map(|a| a.layer.clone()),
                material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<ModelingSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            ModelingSnapshotJson::TriangleMesh(snapshot) => Ok(snapshot.into()),
            _ => Err("Snapshot JSON did not match the expected entity type".to_string()),
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let snapshot = TriangleMeshSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            primitive: TriangleMesh {
                vertices: point_array(
                    object
                        .get("vertices")
                        .ok_or_else(|| "Missing required field 'vertices'".to_string())?,
                )?,
                faces: face_array(
                    object
                        .get("faces")
                        .ok_or_else(|| "Missing required field 'faces'".to_string())?,
                )?,
                normals: object
                    .get("normals")
                    .filter(|normals| !normals.is_null())
                    .map(point_array)
                    .transpose()?,
                name: object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            },
            layer: object
                .get("layer")
                .map(layer_from_json)
                .transpose()?
                .flatten(),
            material_assignment: object
                .get("material_assignment")
                .and_then(material_assignment_from_value),
        };
        validate_triangle_mesh(&snapshot.primitive)?;
        Ok(snapshot.into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(primitive) = entity_ref.get::<TriangleMesh>() else {
            return;
        };
        draw_triangle_mesh_outline(gizmos, primitive, color);
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        let Some(primitive) = entity_ref.get::<TriangleMesh>() else {
            return 0;
        };
        triangle_mesh_outline_line_count(primitive)
    }

    fn hit_test_face(&self, world: &World, entity: Entity, ray: Ray3d) -> Option<FaceHitCandidate> {
        let entity_ref = world.get_entity(entity).ok()?;
        let element_id = *entity_ref.get::<ElementId>()?;
        let mesh = entity_ref.get::<TriangleMesh>()?;

        let mut best: Option<(f32, usize)> = None;
        for (face_idx, face) in mesh.faces.iter().enumerate() {
            let v0 = mesh.vertices[face[0] as usize];
            let v1 = mesh.vertices[face[1] as usize];
            let v2 = mesh.vertices[face[2] as usize];
            if let Some(t) = ray_triangle_intersection(ray, v0, v1, v2) {
                if best.is_none() || t < best.unwrap().0 {
                    best = Some((t, face_idx));
                }
            }
        }

        let (distance, face_idx) = best?;
        let face = &mesh.faces[face_idx];
        let v0 = mesh.vertices[face[0] as usize];
        let v1 = mesh.vertices[face[1] as usize];
        let v2 = mesh.vertices[face[2] as usize];
        let normal = (v1 - v0).cross(v2 - v0).normalize();
        let face_normal = if normal.dot(*ray.direction) < 0.0 {
            normal
        } else {
            -normal
        };
        let centroid = (v0 + v1 + v2) / 3.0;

        Some(FaceHitCandidate {
            entity,
            element_id,
            distance,
            face_id: FaceId(face_idx as u32),
            generated_face_ref: None,
            normal: face_normal,
            centroid,
        })
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(element_id), Some(primitive)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<TriangleMesh>(),
            ) else {
                continue;
            };
            *summary
                .entity_counts
                .entry("triangle_mesh".to_string())
                .or_insert(0) += 1;
            summary.bounding_points.push(
                TriangleMeshSnapshot {
                    element_id: *element_id,
                    primitive: primitive.clone(),
                    layer: entity_ref.get::<LayerAssignment>().map(|a| a.layer.clone()),
                    material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
                }
                .center(),
            );
        }
    }
}

fn layer_from_json(value: &Value) -> Result<Option<String>, String> {
    Ok(match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        _ => return Err("Layer must be a string or null".to_string()),
    })
}

fn elevation_metadata_from_json(value: &Value) -> Result<Option<ElevationMetadata>, String> {
    let Some(object) = value.as_object() else {
        return match value {
            Value::Null => Ok(None),
            _ => Err("Elevation metadata must be an object or null".to_string()),
        };
    };
    Ok(Some(ElevationMetadata {
        source_layer: object
            .get("source_layer")
            .and_then(Value::as_str)
            .ok_or_else(|| "Elevation metadata is missing 'source_layer'".to_string())?
            .to_string(),
        elevation: object
            .get("elevation")
            .map(scalar_from_json)
            .transpose()?
            .ok_or_else(|| "Elevation metadata is missing 'elevation'".to_string())?,
        survey_source_id: match object.get("survey_source_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(
                    "Elevation metadata survey_source_id must be a string or null".to_string(),
                )
            }
        },
    }))
}

fn point_array(value: &Value) -> Result<Vec<Vec3>, String> {
    value
        .as_array()
        .ok_or_else(|| "Expected an array of points".to_string())?
        .iter()
        .map(vec3_from_json)
        .collect()
}

fn face_array(value: &Value) -> Result<Vec<[u32; 3]>, String> {
    value
        .as_array()
        .ok_or_else(|| "Expected an array of faces".to_string())?
        .iter()
        .map(|face| {
            let array = face
                .as_array()
                .ok_or_else(|| "Face must be an array".to_string())?;
            if array.len() != 3 {
                return Err("Face must contain exactly 3 vertex indices".to_string());
            }
            Ok([
                array[0]
                    .as_u64()
                    .ok_or_else(|| "Face indices must be unsigned integers".to_string())?
                    as u32,
                array[1]
                    .as_u64()
                    .ok_or_else(|| "Face indices must be unsigned integers".to_string())?
                    as u32,
                array[2]
                    .as_u64()
                    .ok_or_else(|| "Face indices must be unsigned integers".to_string())?
                    as u32,
            ])
        })
        .collect()
}

fn validate_triangle_mesh(primitive: &TriangleMesh) -> Result<(), String> {
    if primitive.vertices.is_empty() {
        return Err("Triangle mesh must contain at least one vertex".to_string());
    }
    if primitive.faces.is_empty() {
        return Err("Triangle mesh must contain at least one face".to_string());
    }
    if let Some(normals) = &primitive.normals {
        if normals.len() != primitive.vertices.len() {
            return Err("Triangle mesh normals must match the vertex count".to_string());
        }
    }
    for face in &primitive.faces {
        for vertex_index in face {
            if *vertex_index as usize >= primitive.vertices.len() {
                return Err(
                    "Triangle mesh face references an out-of-range vertex index".to_string()
                );
            }
        }
    }
    Ok(())
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

fn aabb_from_points(points: &[Vec3]) -> bevy::camera::primitives::Aabb {
    let bounds = bounds_from_points(points);
    let center = (bounds.min + bounds.max) * 0.5;
    let half_extents = ((bounds.max - bounds.min) * 0.5).max(Vec3::splat(0.01));
    bevy::camera::primitives::Aabb {
        center: center.into(),
        half_extents: half_extents.into(),
    }
}

fn polyline_outline_line_count(primitive: &Polyline) -> usize {
    primitive.points.len().saturating_sub(1)
}

fn triangle_mesh_outline_line_count(primitive: &TriangleMesh) -> usize {
    triangle_mesh_bounds(primitive)
        .map(|_| 12) // 12 edges of a bounding-box wireframe
        .unwrap_or(0)
}

fn draw_polyline_outline(gizmos: &mut Gizmos, primitive: &Polyline, color: Color) {
    for segment in primitive.points.windows(2) {
        gizmos.line(segment[0], segment[1], color);
    }
}

fn draw_triangle_mesh_outline(gizmos: &mut Gizmos, primitive: &TriangleMesh, color: Color) {
    let Some((min, max)) = triangle_mesh_bounds(primitive) else {
        return;
    };
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(max.x, max.y, min.z),
    ];
    let bottom = &corners[0..4];
    let top = &corners[4..8];

    for index in 0..4 {
        let next_index = (index + 1) % 4;
        gizmos.line(bottom[index], bottom[next_index], color);
        gizmos.line(top[index], top[next_index], color);
        gizmos.line(bottom[index], top[index], color);
    }
}

fn triangle_mesh_bounds(primitive: &TriangleMesh) -> Option<(Vec3, Vec3)> {
    let first = primitive.vertices.first().copied()?;
    Some(
        primitive
            .vertices
            .iter()
            .copied()
            .fold((first, first), |(min, max), vertex| {
                (min.min(vertex), max.max(vertex))
            }),
    )
}

/// Returns (closest_distance, ray_parameter) between a 3D ray and a line segment.
fn ray_segment_closest_distance(ray: Ray3d, seg_a: Vec3, seg_b: Vec3) -> (f32, f32) {
    let d = *ray.direction; // ray direction (unit)
    let e = seg_b - seg_a; // segment direction
    let r = ray.origin - seg_a;

    let a = d.dot(d); // == 1.0 for unit direction, but keep general
    let b = d.dot(e);
    let c = e.dot(e);
    let f = d.dot(r);
    let g = e.dot(r);

    let denom = a * c - b * b;

    let seg_t = if denom.abs() <= f32::EPSILON {
        // Nearly parallel: use segment start
        0.0
    } else {
        let t_seg = (a * g - b * f) / denom;
        t_seg.clamp(0.0, 1.0)
    };

    // Derive ray_t for the clamped seg_t
    let closest_seg = seg_a + e * seg_t;
    let ray_t_final = d.dot(closest_seg - ray.origin).max(0.0);
    let closest_ray = ray.origin + d * ray_t_final;

    let distance = closest_ray.distance(closest_seg);
    (distance, ray_t_final)
}

// --- Face detection helpers ---

/// Moller-Trumbore ray-triangle intersection. Returns distance along ray or None.
pub fn ray_triangle_intersection(ray: Ray3d, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    let edge1 = v1 - v0;
    let edge2 = v2 - v0;
    let h = ray.direction.cross(edge2);
    let a = edge1.dot(h);
    if a.abs() < 1e-7 {
        return None;
    }
    let f = 1.0 / a;
    let s = ray.origin - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(edge1);
    let v = f * ray.direction.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * edge2.dot(q);
    if t > 1e-5 {
        Some(t)
    } else {
        None
    }
}

/// Box face quads in world space, given a primitive and rotation.
/// Returns 6 quads (4 corners each) and their outward normals.
/// Face order: -X(0), +X(1), -Y(2), +Y(3), -Z(4), +Z(5).
pub fn box_face_quads(primitive: &BoxPrimitive, rotation: ShapeRotation) -> Vec<([Vec3; 4], Vec3)> {
    let local = box_face_quads_local(primitive.half_extents);
    local
        .iter()
        .map(|(corners, normal)| {
            let world_corners = [
                primitive.centre + rotation.0 * corners[0],
                primitive.centre + rotation.0 * corners[1],
                primitive.centre + rotation.0 * corners[2],
                primitive.centre + rotation.0 * corners[3],
            ];
            let world_normal = rotation.0 * *normal;
            (world_corners, world_normal)
        })
        .collect()
}

fn box_face_quads_local(half: Vec3) -> [([Vec3; 4], Vec3); 6] {
    [
        // -X face (face 0)
        (
            [
                Vec3::new(-half.x, -half.y, -half.z),
                Vec3::new(-half.x, -half.y, half.z),
                Vec3::new(-half.x, half.y, half.z),
                Vec3::new(-half.x, half.y, -half.z),
            ],
            Vec3::NEG_X,
        ),
        // +X face (face 1)
        (
            [
                Vec3::new(half.x, -half.y, half.z),
                Vec3::new(half.x, -half.y, -half.z),
                Vec3::new(half.x, half.y, -half.z),
                Vec3::new(half.x, half.y, half.z),
            ],
            Vec3::X,
        ),
        // -Y face (face 2)
        (
            [
                Vec3::new(-half.x, -half.y, half.z),
                Vec3::new(-half.x, -half.y, -half.z),
                Vec3::new(half.x, -half.y, -half.z),
                Vec3::new(half.x, -half.y, half.z),
            ],
            Vec3::NEG_Y,
        ),
        // +Y face (face 3)
        (
            [
                Vec3::new(-half.x, half.y, -half.z),
                Vec3::new(-half.x, half.y, half.z),
                Vec3::new(half.x, half.y, half.z),
                Vec3::new(half.x, half.y, -half.z),
            ],
            Vec3::Y,
        ),
        // -Z face (face 4)
        (
            [
                Vec3::new(half.x, -half.y, -half.z),
                Vec3::new(-half.x, -half.y, -half.z),
                Vec3::new(-half.x, half.y, -half.z),
                Vec3::new(half.x, half.y, -half.z),
            ],
            Vec3::NEG_Z,
        ),
        // +Z face (face 5)
        (
            [
                Vec3::new(-half.x, -half.y, half.z),
                Vec3::new(half.x, -half.y, half.z),
                Vec3::new(half.x, half.y, half.z),
                Vec3::new(-half.x, half.y, half.z),
            ],
            Vec3::Z,
        ),
    ]
}

// --- EditableMesh AuthoredEntity + Factory ---

impl AuthoredEntity for EditableMeshSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "mesh"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        let (min, max) = self.mesh.bounds();
        let center = (min + max) * 0.5;
        format!(
            "Mesh at ({:.2}, {:.2}, {:.2})",
            center.x, center.y, center.z
        )
    }

    fn center(&self) -> Vec3 {
        let (min, max) = self.mesh.bounds();
        (min + max) * 0.5
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut new = self.clone();
        for v in &mut new.mesh.vertices {
            *v += delta;
        }
        new.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let center = self.center();
        let mut new = self.clone();
        for v in &mut new.mesh.vertices {
            *v = center + rotation * (*v - center);
        }
        new.mesh.recompute_all_normals();
        new.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut new = self.clone();
        for v in &mut new.mesh.vertices {
            *v = scale_point_around_center(*v, center, factor);
        }
        new.mesh.recompute_all_normals();
        new.into()
    }

    fn push_pull(&self, face_id: FaceId, distance: f32) -> Option<BoxedEntity> {
        let fi = face_id.0 as usize;
        if fi >= self.mesh.faces.len() {
            return None;
        }
        let normal = self.mesh.faces[fi].normal;
        let face_verts = self.mesh.vertices_of_face(face_id.0);

        let mut new = self.clone();
        // Translate face vertices along the face normal
        for &vi in &face_verts {
            new.mesh.vertices[vi as usize] += normal * distance;
        }
        new.mesh.recompute_face_normal(face_id.0);
        Some(new.into())
    }

    fn push_pull_affordance(&self, face_id: FaceId) -> PushPullAffordance {
        if (face_id.0 as usize) < self.mesh.faces.len() {
            PushPullAffordance::Allowed
        } else {
            PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace)
        }
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field(
                "vertices",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.mesh.vertices.len() as f32)),
            ),
            property_field(
                "faces",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.mesh.faces.len() as f32)),
            ),
            property_field(
                "edges",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.mesh.edge_count() as f32)),
            ),
            property_field(
                "material",
                PropertyValueKind::Text,
                material_assignment_display_id(self.material_assignment.as_ref())
                    .map(PropertyValue::Text),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        if matches!(property_name, "material" | "material_assignment") {
            return self.set_material_assignment(material_assignment_option_from_value(value)?);
        }
        Err("EditableMesh properties are not directly editable".to_string())
    }

    fn material_assignment(&self) -> Option<MaterialAssignment> {
        self.material_assignment.clone()
    }

    fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        snapshot.material_assignment = assignment;
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        // No parametric handles — mesh editing is via face/edge/vertex selection
        Vec::new()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        let (min, max) = self.mesh.bounds();
        Some(EntityBounds { min, max })
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(ModelingSnapshotJson::EditableMesh(self.clone()))
            .unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        // Spawn or update the entity with the EditableMesh component
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mut entity_mut = world.entity_mut(entity);
            entity_mut.insert((self.mesh.clone(), NeedsMesh));
            if let Some(log) = &self.operation_log {
                entity_mut.insert(log.clone());
            }
            if let Some(material_assignment) = &self.material_assignment {
                entity_mut.insert(material_assignment.clone());
            } else {
                entity_mut.remove::<MaterialAssignment>();
            }
            // Remove parametric components if this was promoted
            entity_mut.remove::<BoxPrimitive>();
            entity_mut.remove::<CylinderPrimitive>();
            entity_mut.remove::<SpherePrimitive>();
            entity_mut.remove::<PlanePrimitive>();
            entity_mut.remove::<ShapeRotation>();
        } else {
            let mut entity = world.spawn((
                self.element_id,
                self.mesh.clone(),
                Transform::IDENTITY,
                Visibility::default(),
                NeedsMesh,
            ));
            if let Some(log) = &self.operation_log {
                entity.insert(log.clone());
            }
            if let Some(material_assignment) = &self.material_assignment {
                entity.insert(material_assignment.clone());
            }
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(Transform::IDENTITY)
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        for (a, b) in self.mesh.edge_segments() {
            gizmos.line(a, b, color);
        }
    }

    fn preview_line_count(&self) -> usize {
        self.mesh.edge_count()
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl AuthoredEntityFactory for EditableMeshFactory {
    fn type_name(&self) -> &'static str {
        "mesh"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let mesh = entity_ref.get::<EditableMesh>()?;
        Some(
            EditableMeshSnapshot {
                element_id,
                mesh: mesh.clone(),
                operation_log: entity_ref.get::<OperationLog>().cloned(),
                material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<ModelingSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            ModelingSnapshotJson::EditableMesh(snapshot) => Ok(snapshot.into()),
            _ => Err("Snapshot JSON did not match the expected entity type".to_string()),
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        // Create from a promote request: { "promote_from": "box", "element_id": N }
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;

        if let Some(promote_from) = object.get("promote_from").and_then(|v| v.as_str()) {
            let element_id_val = object
                .get("element_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "Missing element_id for promotion".to_string())?;
            let element_id = ElementId(element_id_val);

            // Find the entity and promote it
            let entity = {
                let mut __q = world.try_query::<EntityRef>().unwrap();
                __q.iter(world)
                    .find(|e| e.get::<ElementId>().copied() == Some(element_id))
                    .map(|e| e.id())
                    .ok_or_else(|| "Entity not found for promotion".to_string())?
            };
            let entity_ref = world.get_entity(entity).map_err(|e| e.to_string())?;

            // Capture the original snapshot for the OperationLog
            let registry = world.resource::<CapabilityRegistry>();
            let original_snapshot = registry.capture_snapshot(&entity_ref, world);
            let origin_json = original_snapshot
                .as_ref()
                .map(|s| s.to_json())
                .unwrap_or(Value::Null);

            let mesh = match promote_from {
                "box" => {
                    let primitive = entity_ref
                        .get::<BoxPrimitive>()
                        .ok_or("Entity is not a box")?;
                    let rotation = entity_ref
                        .get::<ShapeRotation>()
                        .copied()
                        .unwrap_or_default();
                    EditableMesh::from_box(primitive, &rotation)
                }
                "cylinder" => {
                    let primitive = entity_ref
                        .get::<CylinderPrimitive>()
                        .ok_or("Entity is not a cylinder")?;
                    let rotation = entity_ref
                        .get::<ShapeRotation>()
                        .copied()
                        .unwrap_or_default();
                    EditableMesh::from_cylinder(primitive, &rotation, 24)
                }
                "sphere" => {
                    let primitive = entity_ref
                        .get::<SpherePrimitive>()
                        .ok_or("Entity is not a sphere")?;
                    let rotation = entity_ref
                        .get::<ShapeRotation>()
                        .copied()
                        .unwrap_or_default();
                    EditableMesh::from_sphere(primitive, &rotation, 24, 12)
                }
                "plane" => {
                    let primitive = entity_ref
                        .get::<PlanePrimitive>()
                        .ok_or("Entity is not a plane")?;
                    EditableMesh::from_plane(primitive)
                }
                _ => return Err(format!("Cannot promote from type '{promote_from}'")),
            };

            let operation_log = OperationLog {
                origin: OperationOrigin {
                    type_name: promote_from.to_string(),
                    params: origin_json,
                },
                ops: Vec::new(),
            };

            // Preserve the same element_id so references remain valid
            Ok(EditableMeshSnapshot {
                element_id,
                mesh,
                operation_log: Some(operation_log),
                material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
            }
            .into())
        } else {
            Err("EditableMesh requires a 'promote_from' field".to_string())
        }
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(mesh) = entity_ref.get::<EditableMesh>() else {
            return;
        };
        for (a, b) in mesh.edge_segments() {
            gizmos.line(a, b, color);
        }
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        entity_ref
            .get::<EditableMesh>()
            .map(|m| m.edge_count())
            .unwrap_or(0)
    }

    fn hit_test_face(&self, world: &World, entity: Entity, ray: Ray3d) -> Option<FaceHitCandidate> {
        let entity_ref = world.get_entity(entity).ok()?;
        let element_id = *entity_ref.get::<ElementId>()?;
        let mesh = entity_ref.get::<EditableMesh>()?;

        let mut best: Option<(f32, u32)> = None;
        for (face_idx, face) in mesh.faces.iter().enumerate() {
            if face.half_edge == u32::MAX {
                continue;
            }
            let tris = mesh.triangulate_face(face_idx as u32);
            for tri in &tris {
                let v0 = mesh.vertices[tri[0] as usize];
                let v1 = mesh.vertices[tri[1] as usize];
                let v2 = mesh.vertices[tri[2] as usize];
                if let Some(t) = ray_triangle_intersection(ray, v0, v1, v2) {
                    if best.is_none() || t < best.unwrap().0 {
                        best = Some((t, face_idx as u32));
                    }
                }
            }
        }

        let (distance, face_idx) = best?;
        let face = &mesh.faces[face_idx as usize];
        Some(FaceHitCandidate {
            entity,
            element_id,
            distance,
            face_id: FaceId(face_idx),
            generated_face_ref: None,
            normal: face.normal,
            centroid: mesh.face_centroid(face_idx),
        })
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut __q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in __q.iter(world) {
            let (Some(_element_id), Some(mesh)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<EditableMesh>(),
            ) else {
                continue;
            };
            *summary.entity_counts.entry("mesh".to_string()).or_insert(0) += 1;
            let (min, max) = mesh.bounds();
            summary.bounding_points.push((min + max) * 0.5);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::identity::ElementId;
    use crate::plugins::materials::MaterialAssignment;
    use crate::plugins::modeling::generic_snapshot::PrimitiveSnapshot;
    use crate::plugins::modeling::primitive_trait::Primitive;

    type BoxSnapshot = PrimitiveSnapshot<BoxPrimitive>;
    type CylinderSnapshot = PrimitiveSnapshot<CylinderPrimitive>;
    type PlaneSnapshot = PrimitiveSnapshot<PlanePrimitive>;

    #[test]
    fn plane_property_fields_use_vec3_corners() {
        let snapshot = PlaneSnapshot {
            element_id: ElementId(12),
            primitive: PlanePrimitive {
                corner_a: Vec2::new(-2.0, -1.0),
                corner_b: Vec2::new(3.0, 4.0),
                elevation: 0.75,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };

        let fields = snapshot.property_fields();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].kind, PropertyValueKind::Vec3);
        assert_eq!(
            fields[0].value,
            Some(PropertyValue::Vec3(Vec3::new(-2.0, 0.75, -1.0)))
        );
    }

    #[test]
    fn primitive_material_assignment_can_be_set_through_property_json() {
        let snapshot = PlaneSnapshot {
            element_id: ElementId(12),
            primitive: PlanePrimitive {
                corner_a: Vec2::ZERO,
                corner_b: Vec2::ONE,
                elevation: 0.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let assigned = snapshot
            .set_property_json(
                "material_assignment",
                &serde_json::json!({ "type": "single", "spec": null, "render": "material_def.v1/mat.red" }),
            )
            .expect("material assignment property should parse");

        assert_eq!(
            assigned.material_assignment(),
            Some(MaterialAssignment::new("mat.red"))
        );
        assert_eq!(
            assigned
                .property_fields()
                .into_iter()
                .find(|field| field.name == "material")
                .and_then(|field| field.value),
            Some(PropertyValue::Text("mat.red".to_string()))
        );
        assert_eq!(
            snapshot
                .set_property_json("material", &Value::String("mat.blue".to_string()))
                .expect("material id string should assign")
                .material_assignment(),
            Some(MaterialAssignment::new("mat.blue"))
        );
        assert_eq!(
            assigned
                .set_property_json("material_assignment", &Value::Null)
                .expect("null material assignment should clear")
                .material_assignment(),
            None
        );
    }

    #[test]
    fn modeling_snapshots_round_trip_through_json() {
        let snapshots = [
            ModelingSnapshotJson::Polyline(PolylineSnapshot {
                element_id: ElementId(6),
                primitive: Polyline {
                    points: vec![
                        Vec3::new(0.0, 0.0, 0.0),
                        Vec3::new(1.0, 0.0, 1.0),
                        Vec3::new(2.0, 0.5, 0.5),
                    ],
                },
                layer: None,
                elevation_metadata: None,
                material_assignment: None,
            }),
            ModelingSnapshotJson::TriangleMesh(TriangleMeshSnapshot {
                element_id: ElementId(7),
                primitive: TriangleMesh {
                    vertices: vec![
                        Vec3::new(0.0, 0.0, 0.0),
                        Vec3::new(1.0, 0.0, 0.0),
                        Vec3::new(0.0, 1.0, 0.0),
                    ],
                    faces: vec![[0, 1, 2]],
                    normals: None,
                    name: Some("Tri".to_string()),
                },
                layer: None,
                material_assignment: None,
            }),
        ];

        for snapshot in snapshots {
            let json =
                serde_json::to_string_pretty(&snapshot).expect("snapshot should serialize to JSON");
            let deserialized: ModelingSnapshotJson =
                serde_json::from_str(&json).expect("snapshot should deserialize from JSON");
            assert_eq!(deserialized, snapshot);
        }
    }

    #[test]
    fn scale_by_scales_box_centre_and_half_extents() {
        let snapshot = BoxSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(2.0, 1.0, 1.0),
                half_extents: Vec3::new(1.0, 2.0, 3.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };

        let scaled = snapshot.scale_by(Vec3::new(2.0, 0.5, 3.0), Vec3::new(1.0, 1.0, 1.0));
        let scaled_json = scaled.to_json();

        // PrimitiveSnapshot serialises the primitive directly (not wrapped in an enum).
        assert_eq!(scaled_json["centre"], serde_json::json!([3.0, 1.0, 1.0]));
        assert_eq!(
            scaled_json["half_extents"],
            serde_json::json!([2.0, 1.0, 9.0])
        );
    }

    #[test]
    fn box_apply_with_previous_skips_remesh_for_translate_only_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(1),
                BoxPrimitive {
                    centre: Vec3::new(0.0, 0.5, 0.0),
                    half_extents: Vec3::new(1.0, 0.5, 1.0),
                },
                ShapeRotation::default(),
                Transform::IDENTITY,
            ))
            .id();

        let previous = BoxSnapshot {
            element_id: ElementId(1),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let updated = BoxSnapshot {
            element_id: ElementId(1),
            primitive: BoxPrimitive {
                centre: Vec3::new(2.0, 0.5, -1.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(!entity_ref.contains::<NeedsMesh>());
        let transform = entity_ref
            .get::<Transform>()
            .expect("translated box should keep a transform");
        assert_eq!(transform.translation, Vec3::new(2.0, 0.5, -1.0));
    }

    #[test]
    fn box_apply_with_previous_marks_remesh_for_size_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(1),
                BoxPrimitive {
                    centre: Vec3::new(0.0, 0.5, 0.0),
                    half_extents: Vec3::new(1.0, 0.5, 1.0),
                },
                ShapeRotation::default(),
            ))
            .id();

        let previous = BoxSnapshot {
            element_id: ElementId(1),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let updated = BoxSnapshot {
            element_id: ElementId(1),
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, 0.5, 0.0),
                half_extents: Vec3::new(2.0, 0.5, 1.0),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(entity_ref.contains::<NeedsMesh>());
    }

    #[test]
    fn cylinder_apply_with_previous_skips_remesh_for_translate_only_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(2),
                CylinderPrimitive {
                    centre: Vec3::new(0.0, 1.0, 0.0),
                    radius: 0.5,
                    height: 2.0,
                },
                ShapeRotation::default(),
                Transform::IDENTITY,
            ))
            .id();

        let previous = CylinderSnapshot {
            element_id: ElementId(2),
            primitive: CylinderPrimitive {
                centre: Vec3::new(0.0, 1.0, 0.0),
                radius: 0.5,
                height: 2.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let updated = CylinderSnapshot {
            element_id: ElementId(2),
            primitive: CylinderPrimitive {
                centre: Vec3::new(1.5, 1.0, -0.75),
                radius: 0.5,
                height: 2.0,
            },
            rotation: ShapeRotation(Quat::from_rotation_y(0.25)),
            material_assignment: None,
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(!entity_ref.contains::<NeedsMesh>());
        let transform = entity_ref
            .get::<Transform>()
            .expect("translated cylinder should keep a transform");
        assert_eq!(
            transform.translation,
            Vec3::new(1.5, 1.0, -0.75),
            "translate-only cylinder changes should stay on the fast path",
        );
    }

    #[test]
    fn cylinder_apply_with_previous_marks_remesh_for_radius_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(2),
                CylinderPrimitive {
                    centre: Vec3::new(0.0, 1.0, 0.0),
                    radius: 0.5,
                    height: 2.0,
                },
                ShapeRotation::default(),
            ))
            .id();

        let previous = CylinderSnapshot {
            element_id: ElementId(2),
            primitive: CylinderPrimitive {
                centre: Vec3::new(0.0, 1.0, 0.0),
                radius: 0.5,
                height: 2.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let updated = CylinderSnapshot {
            element_id: ElementId(2),
            primitive: CylinderPrimitive {
                centre: Vec3::new(0.0, 1.0, 0.0),
                radius: 0.75,
                height: 2.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(entity_ref.contains::<NeedsMesh>());
    }

    #[test]
    fn plane_apply_with_previous_skips_remesh_for_translate_only_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(3),
                PlanePrimitive {
                    corner_a: Vec2::new(-1.0, -1.0),
                    corner_b: Vec2::new(1.0, 1.0),
                    elevation: 0.0,
                },
                ShapeRotation::default(),
                Transform::IDENTITY,
            ))
            .id();

        let previous = PlaneSnapshot {
            element_id: ElementId(3),
            primitive: PlanePrimitive {
                corner_a: Vec2::new(-1.0, -1.0),
                corner_b: Vec2::new(1.0, 1.0),
                elevation: 0.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let updated = PlaneSnapshot {
            element_id: ElementId(3),
            primitive: PlanePrimitive {
                corner_a: Vec2::new(2.0, 1.0),
                corner_b: Vec2::new(4.0, 3.0),
                elevation: 0.5,
            },
            rotation: ShapeRotation(Quat::from_rotation_y(0.5)),
            material_assignment: None,
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(!entity_ref.contains::<NeedsMesh>());
        let transform = entity_ref
            .get::<Transform>()
            .expect("translated plane should keep a transform");
        assert_eq!(
            transform.translation,
            updated
                .primitive
                .entity_transform(updated.rotation.0)
                .translation,
        );
    }

    #[test]
    fn plane_apply_with_previous_marks_remesh_for_size_changes() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(3),
                PlanePrimitive {
                    corner_a: Vec2::new(-1.0, -1.0),
                    corner_b: Vec2::new(1.0, 1.0),
                    elevation: 0.0,
                },
                ShapeRotation::default(),
            ))
            .id();

        let previous = PlaneSnapshot {
            element_id: ElementId(3),
            primitive: PlanePrimitive {
                corner_a: Vec2::new(-1.0, -1.0),
                corner_b: Vec2::new(1.0, 1.0),
                elevation: 0.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };
        let updated = PlaneSnapshot {
            element_id: ElementId(3),
            primitive: PlanePrimitive {
                corner_a: Vec2::new(-1.0, -1.0),
                corner_b: Vec2::new(2.0, 1.0),
                elevation: 0.0,
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
        };

        updated.apply_with_previous(&mut world, Some(&previous));

        let entity_ref = world.entity(entity);
        assert!(entity_ref.contains::<NeedsMesh>());
    }
}
