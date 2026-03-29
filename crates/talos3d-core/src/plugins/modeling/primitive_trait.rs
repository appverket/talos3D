use bevy::{math::Ray3d, prelude::*};

use crate::{
    authored_entity::{
        BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef, PushPullAffordance,
        PushPullBlockReason,
    },
    capability_registry::{FaceId, GeneratedFaceRef},
};

use super::editable_mesh::EditableMesh;

/// Slab-method ray vs axis-aligned bounding box intersection.
/// Returns the distance along the ray to the nearest intersection point, or `None`.
pub fn ray_aabb_intersection(ray: Ray3d, min: Vec3, max: Vec3) -> Option<f32> {
    let inv_dir = Vec3::new(
        1.0 / ray.direction.x,
        1.0 / ray.direction.y,
        1.0 / ray.direction.z,
    );

    let t1 = (min - ray.origin) * inv_dir;
    let t2 = (max - ray.origin) * inv_dir;

    let t_min_v = t1.min(t2);
    let t_max_v = t1.max(t2);

    let t_enter = t_min_v.x.max(t_min_v.y).max(t_min_v.z);
    let t_exit = t_max_v.x.min(t_max_v.y).min(t_max_v.z);

    if t_exit < 0.0 || t_enter > t_exit {
        return None;
    }

    // If t_enter < 0, the ray starts inside the box — use t_exit.
    let t = if t_enter < 0.0 { t_exit } else { t_enter };
    if t > 1e-5 {
        Some(t)
    } else {
        None
    }
}

/// Result of a push/pull operation on a primitive.
pub enum PrimitivePushPullResult<P> {
    /// The push/pull produced a new instance of the same primitive type.
    SameType(P, Quat),
    /// The push/pull promoted the primitive to a different type (e.g. Plane -> Box).
    Promoted(BoxedEntity),
}

/// Core trait abstracting over parametric shape types.
///
/// Each method receives rotation as a raw `Quat` so the trait stays decoupled
/// from the `ShapeRotation` newtype.  The generic snapshot/factory wrappers
/// handle the conversion.
pub trait Primitive: Component + Clone + Send + Sync + 'static {
    /// Short, stable identifier used in serialisation and type-name matching.
    const TYPE_NAME: &'static str;

    /// Geometric centre of the primitive in world space.
    fn centre(&self) -> Vec3;

    /// Return a copy translated by `delta`.
    fn translated(&self, delta: Vec3) -> Self;

    /// Return a copy rotated by `rotation` given the current accumulated rotation.
    /// Also returns the new accumulated rotation quaternion.
    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat);

    /// Return a copy scaled by `factor` around `center`.
    fn scaled(&self, factor: Vec3, center: Vec3) -> Self;

    /// Cheap equality check on shape parameters (ignoring position/rotation).
    /// Used by `apply_with_previous` to decide whether to regenerate the mesh.
    fn shape_eq(&self, other: &Self) -> bool;

    /// Compute the Bevy `Transform` for the entity.
    fn entity_transform(&self, rotation: Quat) -> Transform;

    /// Axis-aligned bounding box in world space, if computable.
    fn bounds(&self, rotation: Quat) -> Option<EntityBounds>;

    /// Draw a wireframe outline via gizmos.
    fn draw_wireframe(&self, gizmos: &mut Gizmos, rotation: Quat, color: Color);

    /// Number of line segments the wireframe draws (for line-count budgets).
    fn wireframe_line_count(&self) -> usize;

    /// Push/pull a face by `distance`.  Returns `None` if unsupported for the given face.
    ///
    /// `element_id` is provided so that `Promoted` variants can embed the correct identity
    /// in the promoted entity (e.g. Plane -> Box).
    fn push_pull(
        &self,
        face_id: FaceId,
        distance: f32,
        rotation: Quat,
        element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>>;

    /// Whether push/pull is semantically valid for the given face.
    fn push_pull_affordance(&self, _face_id: FaceId) -> PushPullAffordance {
        PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace)
    }

    /// Resolve a raw topology face ID to a stable semantic reference when possible.
    fn generated_face_ref(&self, _face_id: FaceId) -> Option<GeneratedFaceRef> {
        None
    }

    /// Editable property fields for the property panel.
    fn property_fields(&self, rotation: Quat) -> Vec<PropertyFieldDef>;

    /// Apply a JSON property edit, returning a new primitive or an error.
    fn set_property(&self, name: &str, value: &serde_json::Value) -> Result<Self, String>;

    /// Handle points for direct manipulation.
    fn handles(&self, rotation: Quat) -> Vec<HandleInfo>;

    /// Drag a handle to a new cursor position, returning the updated primitive.
    fn drag_handle(&self, handle_id: &str, cursor: Vec3, rotation: Quat) -> Option<Self>;

    /// Human-readable label (e.g. "Box at (1.00, 2.00, 3.00)").
    fn label(&self) -> String;

    /// Serialise the primitive to JSON.
    fn to_json(&self) -> serde_json::Value;

    /// Deserialise a primitive from JSON.
    fn from_json(value: &serde_json::Value) -> Result<Self, String>;

    /// Ray intersection test using the AABB from `bounds()`.
    /// Returns the distance along the ray, or `None` if no intersection.
    fn hit_test_ray(&self, rotation: Quat, ray: Ray3d) -> Option<f32> {
        let bounds = self.bounds(rotation)?;
        ray_aabb_intersection(ray, bounds.min, bounds.max)
    }

    /// Promote to an `EditableMesh` for topology-mutating operations.
    /// Returns `None` if the primitive does not support mesh promotion.
    fn to_editable_mesh(&self, rotation: Quat) -> Option<EditableMesh> {
        let _ = rotation;
        None
    }
}

/// Which material resource a primitive should use for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshMaterialKind {
    /// Opaque `PrimitiveMaterial` (default for solid shapes).
    Primitive,
    /// Semi-transparent `PlaneMaterial` (for planes).
    Plane,
}

/// Extension trait for primitives that can produce a Bevy `Mesh`.
pub trait MeshGenerator: Primitive {
    /// Which material to use for this shape. Defaults to `Primitive`.
    const MATERIAL_KIND: MeshMaterialKind = MeshMaterialKind::Primitive;

    fn to_bevy_mesh(&self, rotation: Quat) -> Mesh;
}
