//! Mirror geometry node: reflects a source entity across a plane.
//!
//! A `MirrorNode` references a single source entity by `ElementId` and
//! re-evaluates whenever the source changes.  The reflected mesh is stored in
//! `EvaluatedMirror` and rendered like any other evaluated mesh.

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
            primitives::ShapeRotation,
        },
    },
};

// ---------------------------------------------------------------------------
// MirrorPlane
// ---------------------------------------------------------------------------

/// A plane defined by an origin point and a unit normal vector.
///
/// Reflection of point `p`:
/// `p_reflected = p - 2 * dot(p - origin, normal) * normal`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorPlane {
    /// A point lying on the plane.
    pub origin: Vec3,
    /// Unit normal of the plane (must be normalised on construction).
    pub normal: Vec3,
}

impl MirrorPlane {
    /// Custom plane with arbitrary origin and normal.
    #[must_use]
    pub fn new(origin: Vec3, normal: Vec3) -> Self {
        Self {
            origin,
            normal: normal.normalize(),
        }
    }

    /// XY plane (normal = +Z, origin = 0).
    #[must_use]
    pub fn xy() -> Self {
        Self {
            origin: Vec3::ZERO,
            normal: Vec3::Z,
        }
    }

    /// XZ plane (normal = +Y, origin = 0).
    #[must_use]
    pub fn xz() -> Self {
        Self {
            origin: Vec3::ZERO,
            normal: Vec3::Y,
        }
    }

    /// YZ plane (normal = +X, origin = 0).
    #[must_use]
    pub fn yz() -> Self {
        Self {
            origin: Vec3::ZERO,
            normal: Vec3::X,
        }
    }

    /// Reflect a point (or direction with `translate = false`) across this plane.
    #[inline]
    pub fn reflect_point(&self, p: Vec3) -> Vec3 {
        p - 2.0 * (p - self.origin).dot(self.normal) * self.normal
    }

    /// Reflect a direction vector (no translation component).
    #[inline]
    pub fn reflect_normal(&self, n: Vec3) -> Vec3 {
        n - 2.0 * n.dot(self.normal) * self.normal
    }
}

impl TryFrom<&str> for MirrorPlane {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s.to_uppercase().as_str() {
            "XY" => Ok(Self::xy()),
            "XZ" => Ok(Self::xz()),
            "YZ" => Ok(Self::yz()),
            other => Err(format!(
                "Unknown plane shortcut '{other}'. Valid values: XY, XZ, YZ"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// MirrorNode component
// ---------------------------------------------------------------------------

/// A mirror geometry node that reflects a source entity across a plane.
///
/// The source entity remains live and editable.  This node tracks changes to
/// the source and re-evaluates automatically.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorNode {
    /// The source entity to mirror.
    pub source: ElementId,
    /// The mirror plane.
    pub plane: MirrorPlane,
    /// Whether to merge vertices that lie exactly on the mirror plane.
    pub merge: bool,
}

/// Marker component placed on the *source* entity of a mirror node.
///
/// Presence of this marker causes mesh changes on the source to propagate
/// `NeedsEvaluation` to the owning mirror node.
#[derive(Component, Debug, Clone)]
pub struct MirrorOperand {
    /// The `ElementId` of the `MirrorNode` that owns this source.
    pub owner: ElementId,
}

/// Cached evaluated result of a mirror operation.
#[derive(Component, Debug, Clone)]
pub struct EvaluatedMirror {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

// ---------------------------------------------------------------------------
// MirrorSnapshot — AuthoredEntity implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MirrorSnapshot {
    pub element_id: ElementId,
    pub mirror_node: MirrorNode,
}

impl PartialEq for MirrorSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id && self.mirror_node == other.mirror_node
    }
}

impl AuthoredEntity for MirrorSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "mirror"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("Mirror({})", self.mirror_node.source.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    /// Mirror nodes derive their position from the source — translation is a no-op.
    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn rotate_by(&self, _rotation: bevy::math::Quat) -> BoxedEntity {
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
        let n = &self.mirror_node.plane.normal;
        let o = &self.mirror_node.plane.origin;
        vec![
            property_field_with(
                "plane_normal_x",
                "Plane Normal X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(n.x)),
                true,
            ),
            property_field_with(
                "plane_normal_y",
                "Plane Normal Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(n.y)),
                true,
            ),
            property_field_with(
                "plane_normal_z",
                "Plane Normal Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(n.z)),
                true,
            ),
            property_field_with(
                "plane_origin_x",
                "Plane Origin X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(o.x)),
                true,
            ),
            property_field_with(
                "plane_origin_y",
                "Plane Origin Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(o.y)),
                true,
            ),
            property_field_with(
                "plane_origin_z",
                "Plane Origin Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(o.z)),
                true,
            ),
            property_field_with(
                "merge",
                "Merge seam vertices",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.mirror_node.merge.to_string())),
                true,
            ),
        ]
    }

    fn set_property_json(
        &self,
        property_name: &str,
        value: &Value,
    ) -> Result<BoxedEntity, String> {
        let mut snap = self.clone();
        let as_f32 = |v: &Value| -> Result<f32, String> {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| format!("Expected a number for '{property_name}'"))
        };
        match property_name {
            "plane_normal_x" => snap.mirror_node.plane.normal.x = as_f32(value)?,
            "plane_normal_y" => snap.mirror_node.plane.normal.y = as_f32(value)?,
            "plane_normal_z" => snap.mirror_node.plane.normal.z = as_f32(value)?,
            "plane_origin_x" => snap.mirror_node.plane.origin.x = as_f32(value)?,
            "plane_origin_y" => snap.mirror_node.plane.origin.y = as_f32(value)?,
            "plane_origin_z" => snap.mirror_node.plane.origin.z = as_f32(value)?,
            "merge" => {
                snap.mirror_node.merge = if let Some(b) = value.as_bool() {
                    b
                } else if let Some(s) = value.as_str() {
                    s.parse::<bool>().map_err(|_| "Expected 'true' or 'false' for 'merge'".to_string())?
                } else {
                    return Err("Expected a bool for 'merge'".to_string());
                };
            }
            _ => {
                return Err(invalid_property_error(
                    "mirror",
                    &[
                        "plane_normal_x",
                        "plane_normal_y",
                        "plane_normal_z",
                        "plane_origin_x",
                        "plane_origin_y",
                        "plane_origin_z",
                        "merge",
                    ],
                ));
            }
        }
        // Re-normalise in case the caller modified individual components.
        let len = snap.mirror_node.plane.normal.length();
        if len > 1e-6 {
            snap.mirror_node.plane.normal /= len;
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
        serde_json::to_value(&self.mirror_node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world
                .entity_mut(entity)
                .insert((self.mirror_node.clone(), NeedsEvaluation, Visibility::Visible));
        } else {
            world.spawn((
                self.element_id,
                self.mirror_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        // Tag the source entity so that its changes propagate NeedsEvaluation back here.
        if let Some(source_entity) = find_entity_by_element_id(world, self.mirror_node.source) {
            world.entity_mut(source_entity).insert(MirrorOperand {
                owner: self.element_id,
            });
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        // Remove the MirrorOperand marker from the source if it still points at us.
        if let Some(source_entity) = find_entity_by_element_id(world, self.mirror_node.source) {
            let still_ours = world
                .get::<MirrorOperand>(source_entity)
                .map(|op| op.owner == self.element_id)
                .unwrap_or(false);
            if still_ours {
                world.entity_mut(source_entity).remove::<MirrorOperand>();
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
        other.type_name() == "mirror" && other.to_json() == self.to_json()
    }
}

impl From<MirrorSnapshot> for BoxedEntity {
    fn from(snapshot: MirrorSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// ---------------------------------------------------------------------------
// MirrorFactory — AuthoredEntityFactory implementation
// ---------------------------------------------------------------------------

pub struct MirrorFactory;

impl AuthoredEntityFactory for MirrorFactory {
    fn type_name(&self) -> &'static str {
        "mirror"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let mirror_node = entity_ref.get::<MirrorNode>()?.clone();
        Some(MirrorSnapshot { element_id, mirror_node }.into())
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(|v| v.as_u64())
                .ok_or("Missing or invalid element_id")?,
        );
        let mirror_node: MirrorNode =
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        Ok(MirrorSnapshot { element_id, mirror_node }.into())
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

        let plane = parse_plane_from_request(request)?;

        let merge = request
            .get("merge")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(MirrorSnapshot {
            element_id,
            mirror_node: MirrorNode { source, plane, merge },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Some(evaluated) = world.get::<EvaluatedMirror>(entity) else {
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

    fn hit_test(&self, world: &World, ray: bevy::math::Ray3d) -> Option<HitCandidate> {
        use crate::plugins::modeling::primitive_trait::ray_aabb_intersection;

        let mut best: Option<HitCandidate> = None;
        let mut query = world.try_query::<(Entity, &EvaluatedMirror)>().unwrap();
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
}

// ---------------------------------------------------------------------------
// Helper: parse plane from a create/update request value
// ---------------------------------------------------------------------------

/// Parse a mirror plane from a JSON request object.
///
/// Accepts:
/// - `"plane": "XY"` / `"XZ"` / `"YZ"` shortcut string
/// - `"plane": { "origin": [x,y,z], "normal": [x,y,z] }` object
/// - `"plane_origin": [x,y,z]` + `"plane_normal": [x,y,z]` top-level fields
///
/// Falls back to the XZ plane (mirror across Y axis — most common in architecture).
pub fn parse_plane_from_request(request: &Value) -> Result<MirrorPlane, String> {
    if let Some(plane_val) = request.get("plane") {
        if let Some(s) = plane_val.as_str() {
            return MirrorPlane::try_from(s);
        }
        if plane_val.is_object() {
            let origin = parse_vec3(plane_val.get("origin"))?;
            let normal = parse_vec3(plane_val.get("normal"))?;
            return Ok(MirrorPlane::new(origin, normal));
        }
    }
    // Top-level flat fields
    if let (Some(origin_val), Some(normal_val)) =
        (request.get("plane_origin"), request.get("plane_normal"))
    {
        let origin = parse_vec3(Some(origin_val))?;
        let normal = parse_vec3(Some(normal_val))?;
        return Ok(MirrorPlane::new(origin, normal));
    }
    // Default: XZ plane
    Ok(MirrorPlane::xz())
}

fn parse_vec3(v: Option<&Value>) -> Result<Vec3, String> {
    let arr = v
        .and_then(|v| v.as_array())
        .ok_or("Expected a [x, y, z] array")?;
    if arr.len() != 3 {
        return Err(format!("Expected 3 elements, got {}", arr.len()));
    }
    let x = arr[0].as_f64().ok_or("Expected number")? as f32;
    let y = arr[1].as_f64().ok_or("Expected number")? as f32;
    let z = arr[2].as_f64().ok_or("Expected number")? as f32;
    Ok(Vec3::new(x, y, z))
}

// ---------------------------------------------------------------------------
// Evaluation systems
// ---------------------------------------------------------------------------

/// When a source entity tagged with `MirrorOperand` gains `NeedsMesh`,
/// propagate `NeedsEvaluation` to its owning `MirrorNode`.
pub fn propagate_mirror_source_changes(
    mut commands: Commands,
    changed_sources: Query<&MirrorOperand, With<NeedsMesh>>,
    mirror_entities: Query<(Entity, &ElementId), With<MirrorNode>>,
) {
    for operand in &changed_sources {
        for (mirror_entity, mirror_element_id) in &mirror_entities {
            if *mirror_element_id == operand.owner {
                commands.entity(mirror_entity).try_insert(NeedsEvaluation);
            }
        }
    }
}

/// Evaluate all `MirrorNode` entities that carry `NeedsEvaluation`.
///
/// For each dirty mirror, retrieves the source mesh triangles, reflects all
/// vertices and normals across the mirror plane, reverses triangle winding
/// (to keep outward-facing normals correct after the reflection), and stores
/// the result in `EvaluatedMirror`.
pub fn evaluate_mirror_nodes(
    mut commands: Commands,
    dirty_mirrors: Query<(Entity, &MirrorNode), With<NeedsEvaluation>>,
    registry: Res<CapabilityRegistry>,
    world: &World,
) {
    for (entity, mirror_node) in &dirty_mirrors {
        let Some(tris) = get_source_triangles(world, &registry, mirror_node.source) else {
            continue;
        };

        let plane = &mirror_node.plane;

        // Reflect each triangle and reverse winding.
        let mut vertices: Vec<Vec3> = Vec::with_capacity(tris.len() * 3);
        let mut normals: Vec<Vec3> = Vec::with_capacity(tris.len() * 3);
        let mut indices: Vec<u32> = Vec::with_capacity(tris.len() * 3);

        for (i, tri) in tris.iter().enumerate() {
            let base = (i * 3) as u32;
            let [a, b, c] = tri.vertices;

            let ra = plane.reflect_point(a);
            let rb = plane.reflect_point(b);
            let rc = plane.reflect_point(c);

            // Recompute a face normal from reflected positions for accuracy.
            let face_normal = (rb - ra).cross(rc - ra).normalize_or_zero();
            // Use reflected face normal for all three vertices of this triangle.
            let rn = plane.reflect_normal(face_normal);

            vertices.extend_from_slice(&[ra, rb, rc]);
            normals.extend_from_slice(&[rn, rn, rn]);

            // Swap b and c to reverse winding so the normal still points outward.
            indices.extend_from_slice(&[base, base + 2, base + 1]);
        }

        commands.entity(entity).try_insert((
            EvaluatedMirror {
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
// Internal: get triangles from a source entity
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
            try_get_primitive_triangles::<
                crate::plugins::modeling::primitives::CylinderPrimitive,
            >(&entity_ref)
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
            // Fall back to EvaluatedCsg for CSG operands used as mirror sources.
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
            // Also accept an already-evaluated mirror as a source (chained mirrors).
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
