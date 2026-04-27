//! Array geometry nodes: linear and polar pattern tools.
//!
//! A [`LinearArrayNode`] creates N copies of a source entity spaced evenly
//! along a direction vector.  A [`PolarArrayNode`] creates N copies rotated
//! evenly around an axis.
//!
//! Both are composition nodes (like [`super::mirror::MirrorNode`]): they
//! reference a source entity, produce evaluated geometry (all N copies merged
//! into one mesh), and maintain a live dependency link.

use std::any::Any;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, AuthoredEntity, BoxedEntity, EntityBounds,
        HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, CapabilityRegistry, HitCandidate},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        modeling::{
            bsp_csg,
            csg::EvaluatedCsg,
            mesh_generation::{NeedsEvaluation, NeedsMesh},
            mirror::EvaluatedMirror,
            primitives::ShapeRotation,
        },
    },
};

// ---------------------------------------------------------------------------
// LinearArrayNode
// ---------------------------------------------------------------------------

/// A linear array node that copies a source entity along a direction vector.
///
/// The source entity remains live and editable.  This node tracks changes to
/// the source and re-evaluates automatically.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearArrayNode {
    /// The source entity to array.
    pub source: ElementId,
    /// Number of copies (includes the original). Minimum 2.
    pub count: u32,
    /// Spacing vector (direction × distance between successive copies).
    /// For example, `Vec3::X * 2.0` spaces copies 2 m apart along X.
    pub spacing: Vec3,
}

// ---------------------------------------------------------------------------
// PolarArrayNode
// ---------------------------------------------------------------------------

/// A polar array node that copies a source entity around a rotation axis.
///
/// The source entity remains live and editable.  This node tracks changes to
/// the source and re-evaluates automatically.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolarArrayNode {
    /// The source entity to array.
    pub source: ElementId,
    /// Number of copies (includes the original). Minimum 2.
    pub count: u32,
    /// Rotation axis (will be normalised on evaluation).
    pub axis: Vec3,
    /// Total sweep angle in degrees. 360.0 gives a full circle.
    pub total_angle_degrees: f32,
    /// Centre point of rotation.
    pub center: Vec3,
}

// ---------------------------------------------------------------------------
// ArrayOperand marker
// ---------------------------------------------------------------------------

/// Marker component placed on the *source* entity of an array node.
///
/// Presence causes mesh changes on the source to propagate `NeedsEvaluation`
/// to every owning array node.
#[derive(Component, Debug, Clone)]
pub struct ArrayOperand {
    /// The `ElementId` of the array node that owns this source.
    pub owner: ElementId,
}

// ---------------------------------------------------------------------------
// EvaluatedArray
// ---------------------------------------------------------------------------

/// Cached evaluated result of a linear or polar array operation.
///
/// Contains all N copies of the source mesh merged into a single triangle soup.
#[derive(Component, Debug, Clone)]
pub struct EvaluatedArray {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

// ---------------------------------------------------------------------------
// LinearArraySnapshot — AuthoredEntity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LinearArraySnapshot {
    pub element_id: ElementId,
    pub node: LinearArrayNode,
}

impl PartialEq for LinearArraySnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id && self.node == other.node
    }
}

impl AuthoredEntity for LinearArraySnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "linear_array"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("LinearArray({})", self.node.source.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.box_clone()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn push_pull(
        &self,
        _face_id: crate::capability_registry::FaceId,
        _distance: f32,
    ) -> Option<BoxedEntity> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let s = &self.node.spacing;
        vec![
            property_field_with(
                "count",
                "Count",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.count as f32)),
                true,
            ),
            property_field_with(
                "spacing_x",
                "Spacing X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(s.x)),
                true,
            ),
            property_field_with(
                "spacing_y",
                "Spacing Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(s.y)),
                true,
            ),
            property_field_with(
                "spacing_z",
                "Spacing Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(s.z)),
                true,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snap = self.clone();
        let as_f32 = |v: &Value| -> Result<f32, String> {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| format!("Expected a number for '{property_name}'"))
        };
        match property_name {
            "count" => {
                let c = as_f32(value)? as u32;
                snap.node.count = c.max(2);
            }
            "spacing_x" => snap.node.spacing.x = as_f32(value)?,
            "spacing_y" => snap.node.spacing.y = as_f32(value)?,
            "spacing_z" => snap.node.spacing.z = as_f32(value)?,
            _ => {
                return Err(invalid_property_error(
                    "linear_array",
                    &["count", "spacing_x", "spacing_y", "spacing_z"],
                ));
            }
        }
        Ok(snap.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        None
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(&self.node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        } else {
            world.spawn((
                self.element_id,
                self.node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        if let Some(source_entity) = find_entity_by_element_id(world, self.node.source) {
            world.entity_mut(source_entity).insert(ArrayOperand {
                owner: self.element_id,
            });
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        if let Some(source_entity) = find_entity_by_element_id(world, self.node.source) {
            let still_ours = world
                .get::<ArrayOperand>(source_entity)
                .map(|op| op.owner == self.element_id)
                .unwrap_or(false);
            if still_ours {
                world.entity_mut(source_entity).remove::<ArrayOperand>();
            }
        }
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        None
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn preview_line_count(&self) -> usize {
        0
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == "linear_array" && other.to_json() == self.to_json()
    }
}

impl From<LinearArraySnapshot> for BoxedEntity {
    fn from(snapshot: LinearArraySnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// ---------------------------------------------------------------------------
// PolarArraySnapshot — AuthoredEntity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PolarArraySnapshot {
    pub element_id: ElementId,
    pub node: PolarArrayNode,
}

impl PartialEq for PolarArraySnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id && self.node == other.node
    }
}

impl AuthoredEntity for PolarArraySnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "polar_array"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("PolarArray({})", self.node.source.0)
    }

    fn center(&self) -> Vec3 {
        self.node.center
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.box_clone()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn push_pull(
        &self,
        _face_id: crate::capability_registry::FaceId,
        _distance: f32,
    ) -> Option<BoxedEntity> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let a = &self.node.axis;
        let c = &self.node.center;
        vec![
            property_field_with(
                "count",
                "Count",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.count as f32)),
                true,
            ),
            property_field_with(
                "axis_x",
                "Axis X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(a.x)),
                true,
            ),
            property_field_with(
                "axis_y",
                "Axis Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(a.y)),
                true,
            ),
            property_field_with(
                "axis_z",
                "Axis Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(a.z)),
                true,
            ),
            property_field_with(
                "total_angle_degrees",
                "Total Angle (deg)",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.total_angle_degrees)),
                true,
            ),
            property_field_with(
                "center_x",
                "Center X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(c.x)),
                true,
            ),
            property_field_with(
                "center_y",
                "Center Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(c.y)),
                true,
            ),
            property_field_with(
                "center_z",
                "Center Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(c.z)),
                true,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snap = self.clone();
        let as_f32 = |v: &Value| -> Result<f32, String> {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| format!("Expected a number for '{property_name}'"))
        };
        match property_name {
            "count" => {
                let c = as_f32(value)? as u32;
                snap.node.count = c.max(2);
            }
            "axis_x" => snap.node.axis.x = as_f32(value)?,
            "axis_y" => snap.node.axis.y = as_f32(value)?,
            "axis_z" => snap.node.axis.z = as_f32(value)?,
            "total_angle_degrees" => snap.node.total_angle_degrees = as_f32(value)?,
            "center_x" => snap.node.center.x = as_f32(value)?,
            "center_y" => snap.node.center.y = as_f32(value)?,
            "center_z" => snap.node.center.z = as_f32(value)?,
            _ => {
                return Err(invalid_property_error(
                    "polar_array",
                    &[
                        "count",
                        "axis_x",
                        "axis_y",
                        "axis_z",
                        "total_angle_degrees",
                        "center_x",
                        "center_y",
                        "center_z",
                    ],
                ));
            }
        }
        Ok(snap.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        None
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(&self.node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        } else {
            world.spawn((
                self.element_id,
                self.node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        if let Some(source_entity) = find_entity_by_element_id(world, self.node.source) {
            world.entity_mut(source_entity).insert(ArrayOperand {
                owner: self.element_id,
            });
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        if let Some(source_entity) = find_entity_by_element_id(world, self.node.source) {
            let still_ours = world
                .get::<ArrayOperand>(source_entity)
                .map(|op| op.owner == self.element_id)
                .unwrap_or(false);
            if still_ours {
                world.entity_mut(source_entity).remove::<ArrayOperand>();
            }
        }
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        None
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn preview_line_count(&self) -> usize {
        0
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == "polar_array" && other.to_json() == self.to_json()
    }
}

impl From<PolarArraySnapshot> for BoxedEntity {
    fn from(snapshot: PolarArraySnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// ---------------------------------------------------------------------------
// LinearArrayFactory
// ---------------------------------------------------------------------------

pub struct LinearArrayFactory;

impl AuthoredEntityFactory for LinearArrayFactory {
    fn type_name(&self) -> &'static str {
        "linear_array"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<LinearArrayNode>()?.clone();
        Some(LinearArraySnapshot { element_id, node }.into())
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(|v| v.as_u64())
                .ok_or("Missing or invalid element_id")?,
        );
        let node: LinearArrayNode =
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        Ok(LinearArraySnapshot { element_id, node }.into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();

        let source = request
            .get("source")
            .and_then(|v| v.as_u64())
            .map(ElementId)
            .ok_or("Missing 'source' field (u64)")?;

        let count = request
            .get("count")
            .and_then(|v| v.as_u64())
            .map(|v| (v as u32).max(2))
            .ok_or("Missing 'count' field (u32)")?;

        let spacing = parse_spacing_from_request(request)?;

        Ok(LinearArraySnapshot {
            element_id,
            node: LinearArrayNode {
                source,
                count,
                spacing,
            },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        draw_array_selection(world, entity, gizmos, color);
    }

    fn hit_test(&self, world: &World, ray: bevy::math::Ray3d) -> Option<HitCandidate> {
        hit_test_array(world, ray)
    }

    fn dependency_edges(
        &self,
        world: &World,
        entity: Entity,
    ) -> crate::plugins::modeling::dependency_graph::EntityDependencies {
        use crate::plugins::modeling::dependency_graph::EntityDependencies;
        let Some(node) = world.get::<LinearArrayNode>(entity) else {
            return EntityDependencies::empty();
        };
        EntityDependencies::empty().with_edge(node.source, "array_source")
    }
}

// ---------------------------------------------------------------------------
// PolarArrayFactory
// ---------------------------------------------------------------------------

pub struct PolarArrayFactory;

impl AuthoredEntityFactory for PolarArrayFactory {
    fn type_name(&self) -> &'static str {
        "polar_array"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<PolarArrayNode>()?.clone();
        Some(PolarArraySnapshot { element_id, node }.into())
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(|v| v.as_u64())
                .ok_or("Missing or invalid element_id")?,
        );
        let node: PolarArrayNode =
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        Ok(PolarArraySnapshot { element_id, node }.into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();

        let source = request
            .get("source")
            .and_then(|v| v.as_u64())
            .map(ElementId)
            .ok_or("Missing 'source' field (u64)")?;

        let count = request
            .get("count")
            .and_then(|v| v.as_u64())
            .map(|v| (v as u32).max(2))
            .ok_or("Missing 'count' field (u32)")?;

        let axis = parse_axis_from_request(request)?;

        let total_angle_degrees = request
            .get("total_angle_degrees")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(360.0);

        let center = request
            .get("center")
            .and_then(|v| parse_vec3_opt(v))
            .unwrap_or(Vec3::ZERO);

        Ok(PolarArraySnapshot {
            element_id,
            node: PolarArrayNode {
                source,
                count,
                axis,
                total_angle_degrees,
                center,
            },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        draw_array_selection(world, entity, gizmos, color);
    }

    fn hit_test(&self, world: &World, ray: bevy::math::Ray3d) -> Option<HitCandidate> {
        hit_test_array(world, ray)
    }

    fn dependency_edges(
        &self,
        world: &World,
        entity: Entity,
    ) -> crate::plugins::modeling::dependency_graph::EntityDependencies {
        use crate::plugins::modeling::dependency_graph::EntityDependencies;
        let Some(node) = world.get::<PolarArrayNode>(entity) else {
            return EntityDependencies::empty();
        };
        EntityDependencies::empty().with_edge(node.source, "array_source")
    }
}

// ---------------------------------------------------------------------------
// Shared factory helpers
// ---------------------------------------------------------------------------

fn draw_array_selection(world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
    let Some(evaluated) = world.get::<EvaluatedArray>(entity) else {
        return;
    };
    for tri in evaluated.indices.chunks(3) {
        if tri.len() < 3 {
            continue;
        }
        let (a, b, c) = (
            evaluated.vertices[tri[0] as usize],
            evaluated.vertices[tri[1] as usize],
            evaluated.vertices[tri[2] as usize],
        );
        gizmos.line(a, b, color);
        gizmos.line(b, c, color);
        gizmos.line(c, a, color);
    }
}

fn hit_test_array(world: &World, ray: bevy::math::Ray3d) -> Option<HitCandidate> {
    use crate::plugins::modeling::primitive_trait::ray_aabb_intersection;

    let mut best: Option<HitCandidate> = None;
    let mut query = world.try_query::<(Entity, &EvaluatedArray)>().unwrap();
    for (entity, evaluated) in query.iter(world) {
        if evaluated.vertices.is_empty() {
            continue;
        }
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for v in &evaluated.vertices {
            min = min.min(*v);
            max = max.max(*v);
        }
        if let Some(distance) = ray_aabb_intersection(ray, min, max) {
            if best.is_none() || distance < best.as_ref().unwrap().distance {
                best = Some(HitCandidate { entity, distance });
            }
        }
    }
    best
}

/// Parse a spacing `Vec3` from a JSON create request.
///
/// Accepts:
/// - `"spacing": [x, y, z]` array
/// - `"spacing_x"`, `"spacing_y"`, `"spacing_z"` flat scalars
/// - `"axis": "X"` / `"Y"` / `"Z"` + `"distance"` shorthand
///
/// Falls back to `Vec3::X * 1.0` if nothing is provided.
pub fn parse_spacing_from_request(request: &Value) -> Result<Vec3, String> {
    // Full [x, y, z] array
    if let Some(arr) = request.get("spacing").and_then(|v| v.as_array()) {
        if arr.len() == 3 {
            let x = arr[0].as_f64().ok_or("Expected number in spacing[0]")? as f32;
            let y = arr[1].as_f64().ok_or("Expected number in spacing[1]")? as f32;
            let z = arr[2].as_f64().ok_or("Expected number in spacing[2]")? as f32;
            return Ok(Vec3::new(x, y, z));
        }
    }
    // Flat spacing_x / spacing_y / spacing_z
    let has_flat = request.get("spacing_x").is_some()
        || request.get("spacing_y").is_some()
        || request.get("spacing_z").is_some();
    if has_flat {
        let x = request
            .get("spacing_x")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32;
        let y = request
            .get("spacing_y")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32;
        let z = request
            .get("spacing_z")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32;
        return Ok(Vec3::new(x, y, z));
    }
    // Axis shorthand: "X" / "Y" / "Z" * distance
    if let Some(axis_str) = request.get("axis").and_then(|v| v.as_str()) {
        let distance = request
            .get("distance")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;
        let dir = match axis_str.to_uppercase().as_str() {
            "X" => Vec3::X,
            "Y" => Vec3::Y,
            "Z" => Vec3::Z,
            other => {
                return Err(format!(
                    "Unknown axis shorthand '{other}'. Use 'X', 'Y', or 'Z', or provide 'spacing' array."
                ))
            }
        };
        return Ok(dir * distance);
    }
    // Default: 1 m along X
    Ok(Vec3::X)
}

/// Parse a rotation axis from a JSON create request.
///
/// Accepts:
/// - `"axis": [x, y, z]` array
/// - `"axis": "X"` / `"Y"` / `"Z"` shorthand
///
/// Falls back to `Vec3::Y` if nothing is provided.
pub fn parse_axis_from_request(request: &Value) -> Result<Vec3, String> {
    if let Some(axis_val) = request.get("axis") {
        if let Some(s) = axis_val.as_str() {
            return match s.to_uppercase().as_str() {
                "X" => Ok(Vec3::X),
                "Y" => Ok(Vec3::Y),
                "Z" => Ok(Vec3::Z),
                other => Err(format!(
                    "Unknown axis shorthand '{other}'. Use 'X', 'Y', 'Z', or a [x,y,z] array."
                )),
            };
        }
        if let Some(arr) = axis_val.as_array() {
            if arr.len() == 3 {
                let x = arr[0].as_f64().ok_or("Expected number in axis[0]")? as f32;
                let y = arr[1].as_f64().ok_or("Expected number in axis[1]")? as f32;
                let z = arr[2].as_f64().ok_or("Expected number in axis[2]")? as f32;
                return Ok(Vec3::new(x, y, z));
            }
        }
    }
    Ok(Vec3::Y)
}

fn parse_vec3_opt(v: &Value) -> Option<Vec3> {
    let arr = v.as_array()?;
    if arr.len() != 3 {
        return None;
    }
    let x = arr[0].as_f64()? as f32;
    let y = arr[1].as_f64()? as f32;
    let z = arr[2].as_f64()? as f32;
    Some(Vec3::new(x, y, z))
}

// ---------------------------------------------------------------------------
// Evaluation systems
// ---------------------------------------------------------------------------

/// When a source entity tagged with [`ArrayOperand`] gains [`NeedsMesh`],
/// propagate [`NeedsEvaluation`] to its owning array node.
pub fn propagate_array_source_changes(
    mut commands: Commands,
    changed_sources: Query<&ArrayOperand, With<NeedsMesh>>,
    array_entities: Query<(Entity, &ElementId), Or<(With<LinearArrayNode>, With<PolarArrayNode>)>>,
) {
    for operand in &changed_sources {
        for (array_entity, array_element_id) in &array_entities {
            if *array_element_id == operand.owner {
                commands.entity(array_entity).try_insert(NeedsEvaluation);
            }
        }
    }
}

/// Evaluate all [`LinearArrayNode`] entities that carry [`NeedsEvaluation`].
///
/// For each dirty node, retrieves the source mesh, translates N copies by
/// `spacing * i`, and merges them into a single [`EvaluatedArray`].
pub fn evaluate_linear_array_nodes(
    mut commands: Commands,
    dirty_nodes: Query<(Entity, &LinearArrayNode), With<NeedsEvaluation>>,
    registry: Res<CapabilityRegistry>,
    world: &World,
) {
    for (entity, node) in &dirty_nodes {
        let Some(tris) = get_source_triangles(world, &registry, node.source) else {
            continue;
        };

        let src_vert_count = tris.len() * 3;
        let total = node.count as usize;

        let mut vertices: Vec<Vec3> = Vec::with_capacity(src_vert_count * total);
        let mut normals: Vec<Vec3> = Vec::with_capacity(src_vert_count * total);
        let mut indices: Vec<u32> = Vec::with_capacity(src_vert_count * total);

        for i in 0..total {
            let offset = node.spacing * i as f32;
            let base = (i * src_vert_count) as u32;

            for (j, tri) in tris.iter().enumerate() {
                let tri_base = base + (j * 3) as u32;
                let [a, b, c] = tri.vertices;

                let ta = a + offset;
                let tb = b + offset;
                let tc = c + offset;

                let face_normal = (tb - ta).cross(tc - ta).normalize_or_zero();
                vertices.extend_from_slice(&[ta, tb, tc]);
                normals.extend_from_slice(&[face_normal, face_normal, face_normal]);
                indices.extend_from_slice(&[tri_base, tri_base + 1, tri_base + 2]);
            }
        }

        commands.entity(entity).try_insert((
            EvaluatedArray {
                vertices,
                normals,
                indices,
            },
            NeedsMesh,
        ));
        commands.entity(entity).remove::<NeedsEvaluation>();
    }
}

/// Evaluate all [`PolarArrayNode`] entities that carry [`NeedsEvaluation`].
///
/// For each dirty node, retrieves the source mesh and rotates N copies evenly
/// around `axis` by `total_angle_degrees / count * i` degrees, then merges
/// them into a single [`EvaluatedArray`].
pub fn evaluate_polar_array_nodes(
    mut commands: Commands,
    dirty_nodes: Query<(Entity, &PolarArrayNode), With<NeedsEvaluation>>,
    registry: Res<CapabilityRegistry>,
    world: &World,
) {
    for (entity, node) in &dirty_nodes {
        let Some(tris) = get_source_triangles(world, &registry, node.source) else {
            continue;
        };

        let axis = node.axis.normalize_or_zero();
        if axis == Vec3::ZERO {
            continue;
        }

        let src_vert_count = tris.len() * 3;
        let total = node.count as usize;

        let mut vertices: Vec<Vec3> = Vec::with_capacity(src_vert_count * total);
        let mut normals: Vec<Vec3> = Vec::with_capacity(src_vert_count * total);
        let mut indices: Vec<u32> = Vec::with_capacity(src_vert_count * total);

        for i in 0..total {
            let angle_deg = node.total_angle_degrees * (i as f32 / node.count as f32);
            let rotation = Quat::from_axis_angle(axis, angle_deg.to_radians());
            let base = (i * src_vert_count) as u32;

            for (j, tri) in tris.iter().enumerate() {
                let tri_base = base + (j * 3) as u32;
                let [a, b, c] = tri.vertices;

                // Translate to origin, rotate, translate back.
                let ra = node.center + rotation * (a - node.center);
                let rb = node.center + rotation * (b - node.center);
                let rc = node.center + rotation * (c - node.center);

                let face_normal = (rb - ra).cross(rc - ra).normalize_or_zero();
                vertices.extend_from_slice(&[ra, rb, rc]);
                normals.extend_from_slice(&[face_normal, face_normal, face_normal]);
                indices.extend_from_slice(&[tri_base, tri_base + 1, tri_base + 2]);
            }
        }

        commands.entity(entity).try_insert((
            EvaluatedArray {
                vertices,
                normals,
                indices,
            },
            NeedsMesh,
        ));
        commands.entity(entity).remove::<NeedsEvaluation>();
    }
}

// ---------------------------------------------------------------------------
// Internal: get triangles from a source entity (mirrors mirror.rs pattern)
// ---------------------------------------------------------------------------

fn get_source_triangles(
    world: &World,
    _registry: &CapabilityRegistry,
    element_id: ElementId,
) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let mut q = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(element_id))?;

    try_get_primitive_triangles::<crate::plugins::modeling::primitives::BoxPrimitive>(&entity_ref)
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::primitives::CylinderPrimitive>(
                &entity_ref,
            )
        })
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::primitives::PlanePrimitive>(
                &entity_ref,
            )
        })
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::profile::ProfileExtrusion>(
                &entity_ref,
            )
        })
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::profile::ProfileSweep>(
                &entity_ref,
            )
        })
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::profile::ProfileRevolve>(
                &entity_ref,
            )
        })
        .or_else(|| {
            entity_ref.get::<EvaluatedCsg>().map(|evaluated| {
                evaluated
                    .indices
                    .chunks(3)
                    .filter(|c| c.len() == 3)
                    .map(|c| {
                        bsp_csg::CsgTriangle::new(
                            evaluated.vertices[c[0] as usize],
                            evaluated.vertices[c[1] as usize],
                            evaluated.vertices[c[2] as usize],
                        )
                    })
                    .collect()
            })
        })
        .or_else(|| {
            entity_ref.get::<EvaluatedMirror>().map(|evaluated| {
                evaluated
                    .indices
                    .chunks(3)
                    .filter(|c| c.len() == 3)
                    .map(|c| {
                        bsp_csg::CsgTriangle::new(
                            evaluated.vertices[c[0] as usize],
                            evaluated.vertices[c[1] as usize],
                            evaluated.vertices[c[2] as usize],
                        )
                    })
                    .collect()
            })
        })
        .or_else(|| {
            // Also accept an already-evaluated array as a source (chained arrays).
            entity_ref.get::<EvaluatedArray>().map(|evaluated| {
                evaluated
                    .indices
                    .chunks(3)
                    .filter(|c| c.len() == 3)
                    .map(|c| {
                        bsp_csg::CsgTriangle::new(
                            evaluated.vertices[c[0] as usize],
                            evaluated.vertices[c[1] as usize],
                            evaluated.vertices[c[2] as usize],
                        )
                    })
                    .collect()
            })
        })
}

fn try_get_primitive_triangles<P: crate::plugins::modeling::primitive_trait::Primitive>(
    entity_ref: &bevy::ecs::world::EntityRef,
) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let primitive = entity_ref.get::<P>()?;
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default();
    let mesh = primitive.to_editable_mesh(rotation.0)?;
    Some(bsp_csg::triangles_from_editable_mesh(&mesh))
}

#[cfg(test)]
mod typereg_tests {
    use super::*;
    use crate::capability_registry::AuthoredEntityFactory;
    use crate::plugins::modeling::dependency_graph::EntityDependencies;
    use bevy::prelude::*;

    #[test]
    fn linear_array_factory_declares_edge_on_source() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(10),
                LinearArrayNode {
                    source: ElementId(20),
                    count: 3,
                    spacing: Vec3::X,
                },
            ))
            .id();

        let edges = LinearArrayFactory.dependency_edges(&world, entity);
        let expected = EntityDependencies::empty().with_edge(ElementId(20), "array_source");
        assert_eq!(edges, expected);
    }

    #[test]
    fn linear_array_factory_returns_empty_when_component_missing() {
        let mut world = World::new();
        let entity = world.spawn(ElementId(10)).id();
        let edges = LinearArrayFactory.dependency_edges(&world, entity);
        assert_eq!(edges, EntityDependencies::empty());
    }

    #[test]
    fn polar_array_factory_declares_edge_on_source() {
        let mut world = World::new();
        let entity = world
            .spawn((
                ElementId(11),
                PolarArrayNode {
                    source: ElementId(21),
                    count: 6,
                    axis: Vec3::Z,
                    total_angle_degrees: 360.0,
                    center: Vec3::ZERO,
                },
            ))
            .id();

        let edges = PolarArrayFactory.dependency_edges(&world, entity);
        let expected = EntityDependencies::empty().with_edge(ElementId(21), "array_source");
        assert_eq!(edges, expected);
    }
}
