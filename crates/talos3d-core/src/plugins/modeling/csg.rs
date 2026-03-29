//! CSG (Constructive Solid Geometry) component, evaluation, and snapshot.
//!
//! A CsgNode defines a boolean combination of two operand entities.
//! The operands remain as live parametric entities (hidden from rendering).
//! The evaluation pipeline produces the result mesh.

use std::any::Any;
use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, AuthoredEntity, BoxedEntity, EntityBounds,
        HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityRegistry, FaceHitCandidate, FaceId, HitCandidate,
    },
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        modeling::{
            bsp_csg::{self, BooleanOp},
            mesh_generation::{NeedsEvaluation, NeedsMesh},
            primitives::ShapeRotation,
            snapshots::ray_triangle_intersection,
        },
    },
};

/// A CSG node that defines a boolean combination of two operand entities.
///
/// The operands are referenced by ElementId and remain as live entities.
/// The CSG node owns the evaluated result mesh.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsgNode {
    /// The "base" operand (left side of the operation).
    pub operand_a: ElementId,
    /// The "tool" operand (right side of the operation).
    pub operand_b: ElementId,
    /// The boolean operation to apply.
    pub op: BooleanOp,
}

/// Marker on entities that are operands of a CsgNode.
/// These entities are hidden from rendering but remain editable.
#[derive(Component, Debug, Clone)]
pub struct CsgOperand {
    /// The CSG node entity that owns this operand.
    pub owner: ElementId,
}

/// The evaluated result mesh of a CSG operation (cached, recomputed on changes).
#[derive(Component, Debug, Clone)]
pub struct EvaluatedCsg {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

/// Tracks profile extrusions that were drawn on a parent face.
///
/// When a closed polyline is drawn on a face, the resulting ProfileExtrusion
/// is registered here. On the first push/pull confirm, if the user pushed
/// inward (negative distance), a CSG Difference is automatically created
/// between the parent and the profile extrusion.
#[derive(Resource, Default, Debug, Clone)]
pub struct CsgParentMap {
    /// Maps child ElementId → parent ElementId.
    pub parents: HashMap<ElementId, ElementId>,
}

// ---------------------------------------------------------------------------
// CsgSnapshot — AuthoredEntity implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CsgSnapshot {
    pub element_id: ElementId,
    pub csg_node: CsgNode,
}

impl PartialEq for CsgSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id && self.csg_node == other.csg_node
    }
}

impl AuthoredEntity for CsgSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "csg"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!(
            "{:?}({}, {})",
            self.csg_node.op, self.csg_node.operand_a.0, self.csg_node.operand_b.0
        )
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        // CSG nodes don't have their own position — operands do.
        self.box_clone()
    }

    fn rotate_by(&self, _rotation: bevy::math::Quat) -> BoxedEntity {
        self.box_clone()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn push_pull(&self, _face_id: FaceId, _distance: f32) -> Option<BoxedEntity> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![property_field_with(
            "op",
            "Operation",
            PropertyValueKind::Text,
            Some(PropertyValue::Text(format!("{:?}", self.csg_node.op))),
            true,
        )]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snap = self.clone();
        match property_name {
            "op" => {
                let op_str = value
                    .as_str()
                    .ok_or_else(|| "op must be a string".to_string())?;
                snap.csg_node.op = match op_str.to_lowercase().as_str() {
                    "union" => BooleanOp::Union,
                    "difference" => BooleanOp::Difference,
                    "intersection" => BooleanOp::Intersection,
                    _ => return Err(format!("Unknown op: {op_str}")),
                };
            }
            _ => {
                return Err(invalid_property_error("csg", &["op"]));
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
        serde_json::to_value(&self.csg_node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.csg_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        } else {
            world.spawn((
                self.element_id,
                self.csg_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        // Mark operands as CSG operands and hide them
        for operand_id in [self.csg_node.operand_a, self.csg_node.operand_b] {
            if let Some(entity) = find_entity_by_element_id(world, operand_id) {
                world.entity_mut(entity).insert((
                    CsgOperand {
                        owner: self.element_id,
                    },
                    Visibility::Hidden,
                ));
            }
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        // Restore operand visibility
        for operand_id in [self.csg_node.operand_a, self.csg_node.operand_b] {
            if let Some(entity) = find_entity_by_element_id(world, operand_id) {
                world.entity_mut(entity).remove::<CsgOperand>();
                world.entity_mut(entity).insert(Visibility::Visible);
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
        other.type_name() == "csg" && other.to_json() == self.to_json()
    }
}

impl From<CsgSnapshot> for BoxedEntity {
    fn from(snapshot: CsgSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// ---------------------------------------------------------------------------
// CsgFactory — AuthoredEntityFactory implementation
// ---------------------------------------------------------------------------

pub struct CsgFactory;

impl AuthoredEntityFactory for CsgFactory {
    fn type_name(&self) -> &'static str {
        "csg"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let csg_node = entity_ref.get::<CsgNode>()?;
        Some(
            CsgSnapshot {
                element_id,
                csg_node: csg_node.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(|v| v.as_u64())
                .ok_or("Missing or invalid element_id")?,
        );
        let csg_node: CsgNode = serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        Ok(CsgSnapshot {
            element_id,
            csg_node,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();
        let op_str = request
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'op' field")?;
        let op = match op_str.to_lowercase().as_str() {
            "union" => BooleanOp::Union,
            "difference" => BooleanOp::Difference,
            "intersection" => BooleanOp::Intersection,
            _ => return Err(format!("Unknown op: {op_str}")),
        };
        let operand_a = request
            .get("operand_a")
            .and_then(|v| v.as_u64())
            .map(ElementId)
            .ok_or("Missing 'operand_a'")?;
        let operand_b = request
            .get("operand_b")
            .and_then(|v| v.as_u64())
            .map(ElementId)
            .ok_or("Missing 'operand_b'")?;

        Ok(CsgSnapshot {
            element_id,
            csg_node: CsgNode {
                operand_a,
                operand_b,
                op,
            },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Some(evaluated) = world.get::<EvaluatedCsg>(entity) else {
            return;
        };
        // Draw triangle edges as wireframe outline
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
        let mut query = world.try_query::<(Entity, &EvaluatedCsg)>().unwrap();
        for (entity, evaluated) in query.iter(world) {
            if evaluated.vertices.is_empty() {
                continue;
            }
            // AABB hit test on evaluated mesh
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
// Evaluation system
// ---------------------------------------------------------------------------

/// System: when a primitive that is a CSG operand changes (gets NeedsMesh),
/// propagate NeedsEvaluation to the owning CSG node.
pub fn propagate_operand_changes(
    mut commands: Commands,
    changed_operands: Query<&CsgOperand, With<NeedsMesh>>,
    csg_entities: Query<(Entity, &ElementId), With<CsgNode>>,
) {
    for operand in &changed_operands {
        for (csg_entity, csg_element_id) in &csg_entities {
            if *csg_element_id == operand.owner {
                commands.entity(csg_entity).try_insert(NeedsEvaluation);
            }
        }
    }
}

/// System: evaluate all CSG nodes that have NeedsEvaluation.
pub fn evaluate_csg_nodes(
    mut commands: Commands,
    dirty_csg: Query<(Entity, &CsgNode), With<NeedsEvaluation>>,
    registry: Res<CapabilityRegistry>,
    world: &World,
) {
    for (entity, csg_node) in &dirty_csg {
        let mesh_a = get_operand_triangles(world, &registry, csg_node.operand_a);
        let mesh_b = get_operand_triangles(world, &registry, csg_node.operand_b);

        if let (Some(tris_a), Some(tris_b)) = (mesh_a, mesh_b) {
            let result = bsp_csg::boolean(&tris_a, &tris_b, csg_node.op);
            commands.entity(entity).try_insert((
                EvaluatedCsg {
                    vertices: result.vertices,
                    normals: result.normals,
                    indices: result.indices,
                },
                NeedsMesh,
            ));
            commands.entity(entity).remove::<NeedsEvaluation>();
        }
    }
}

/// Get triangles from an operand entity via its factory's to_editable_mesh.
fn get_operand_triangles(
    world: &World,
    _registry: &CapabilityRegistry,
    element_id: ElementId,
) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let mut q = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    let entity_ref = q
        .iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(element_id))?;

    // Try each known primitive type to get triangles.
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

// ---------------------------------------------------------------------------
// CSG face hit testing (for face-edit mode on CsgNode entities)
// ---------------------------------------------------------------------------

/// Ray-casting parity test: cast a ray from `point` and count how many
/// triangles it crosses. Odd count = inside, even = outside.
///
/// Uses `+X` as the ray direction (arbitrary; any fixed direction works).
pub fn point_inside_mesh(point: Vec3, triangles: &[bsp_csg::CsgTriangle]) -> bool {
    let ray = bevy::math::Ray3d {
        origin: point,
        direction: bevy::math::Dir3::X,
    };
    let mut crossings: u32 = 0;
    for tri in triangles {
        let [a, b, c] = tri.vertices;
        if ray_triangle_intersection(ray, a, b, c).is_some() {
            crossings += 1;
        }
    }
    crossings % 2 == 1
}

/// Get the triangle set for an entity — tries primitives first, then EvaluatedCsg.
fn get_entity_triangles(world: &World, entity: Entity) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let entity_ref = world.get_entity(entity).ok()?;

    // Try all known primitive types
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
            // Fall back to EvaluatedCsg for nested CSG operands
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
}

/// Resolve an entity from an ElementId using a read-only World reference.
fn find_entity_by_element_id_ref(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
    q.iter(world)
        .find_map(|(entity, eid)| (*eid == element_id).then_some(entity))
}

/// Perform a face-level hit test against a CsgNode entity.
///
/// For each operand, the hit is only valid if it satisfies the boolean
/// semantics:
/// - `Difference(A, B)`: hits on A valid if outside B; hits on B valid if inside A.
/// - `Union(A, B)`: hits on either operand valid (pick nearest).
/// - `Intersection(A, B)`: hits on A valid if inside B; hits on B valid if inside A.
///
/// Returns the nearest valid hit, with the **operand** entity/element_id
/// (not the CsgNode entity).
pub fn csg_face_hit_test(
    world: &World,
    csg_entity: Entity,
    ray: bevy::math::Ray3d,
) -> Option<FaceHitCandidate> {
    let csg_node = world.get::<CsgNode>(csg_entity)?.clone();

    // Resolve operand entities
    let operand_a_entity = find_entity_by_element_id_ref(world, csg_node.operand_a)?;
    let operand_b_entity = find_entity_by_element_id_ref(world, csg_node.operand_b)?;

    // Hit test each operand — if the operand is itself a CsgNode, recurse.
    let hit_a = face_hit_test_entity(world, operand_a_entity, ray);
    let hit_b = face_hit_test_entity(world, operand_b_entity, ray);

    // Get triangles for the containment tests (may be None for degenerate meshes)
    let tris_a = get_entity_triangles(world, operand_a_entity).unwrap_or_default();
    let tris_b = get_entity_triangles(world, operand_b_entity).unwrap_or_default();

    // Validate hits according to boolean semantics
    let valid_a = hit_a
        .as_ref()
        .map(|h| {
            let hit_point = ray.origin + ray.direction * h.distance;
            match csg_node.op {
                BooleanOp::Difference => !point_inside_mesh(hit_point, &tris_b),
                BooleanOp::Union => true,
                BooleanOp::Intersection => point_inside_mesh(hit_point, &tris_b),
            }
        })
        .unwrap_or(false);

    let valid_b = hit_b
        .as_ref()
        .map(|h| {
            let hit_point = ray.origin + ray.direction * h.distance;
            match csg_node.op {
                BooleanOp::Difference => point_inside_mesh(hit_point, &tris_a),
                BooleanOp::Union => true,
                BooleanOp::Intersection => point_inside_mesh(hit_point, &tris_a),
            }
        })
        .unwrap_or(false);

    // Pick the nearest valid hit
    match (
        valid_a.then_some(hit_a).flatten(),
        valid_b.then_some(hit_b).flatten(),
    ) {
        (Some(a), Some(b)) => {
            if a.distance <= b.distance {
                Some(a)
            } else {
                Some(b)
            }
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Perform a face-level hit test on a single entity — recurses if the entity is a CsgNode.
fn face_hit_test_entity(
    world: &World,
    entity: Entity,
    ray: bevy::math::Ray3d,
) -> Option<FaceHitCandidate> {
    // If this operand is itself a CsgNode, recurse
    if world.get::<CsgNode>(entity).is_some() {
        return csg_face_hit_test(world, entity, ray);
    }

    // Otherwise use the generic primitive face hit test
    let entity_ref = world.get_entity(entity).ok()?;
    let element_id = *entity_ref.get::<ElementId>()?;
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default();

    // Try all primitive types
    face_hit_test_primitive::<crate::plugins::modeling::primitives::BoxPrimitive>(
        &entity_ref,
        entity,
        element_id,
        rotation.0,
        ray,
    )
    .or_else(|| {
        face_hit_test_primitive::<crate::plugins::modeling::primitives::CylinderPrimitive>(
            &entity_ref,
            entity,
            element_id,
            rotation.0,
            ray,
        )
    })
    .or_else(|| {
        face_hit_test_primitive::<crate::plugins::modeling::primitives::PlanePrimitive>(
            &entity_ref,
            entity,
            element_id,
            rotation.0,
            ray,
        )
    })
    .or_else(|| {
        face_hit_test_primitive::<crate::plugins::modeling::profile::ProfileExtrusion>(
            &entity_ref,
            entity,
            element_id,
            rotation.0,
            ray,
        )
    })
    .or_else(|| {
        face_hit_test_primitive::<crate::plugins::modeling::profile::ProfileSweep>(
            &entity_ref,
            entity,
            element_id,
            rotation.0,
            ray,
        )
    })
    .or_else(|| {
        face_hit_test_primitive::<crate::plugins::modeling::profile::ProfileRevolve>(
            &entity_ref,
            entity,
            element_id,
            rotation.0,
            ray,
        )
    })
}

fn face_hit_test_primitive<P: crate::plugins::modeling::primitive_trait::Primitive>(
    entity_ref: &bevy::ecs::world::EntityRef,
    entity: Entity,
    element_id: ElementId,
    rotation: bevy::math::Quat,
    ray: bevy::math::Ray3d,
) -> Option<FaceHitCandidate> {
    let primitive = entity_ref.get::<P>()?;
    let mesh = primitive.to_editable_mesh(rotation)?;

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
    let face_id = FaceId(face_idx);
    Some(FaceHitCandidate {
        entity,
        element_id,
        distance,
        face_id,
        generated_face_ref: primitive.generated_face_ref(face_id),
        normal: face.normal,
        centroid: mesh.face_centroid(face_idx),
    })
}
