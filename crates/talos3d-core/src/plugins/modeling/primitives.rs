use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::editable_mesh::EditableMesh;
use super::primitive_trait::{MeshGenerator, MeshMaterialKind, Primitive, PrimitivePushPullResult};
use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, scalar_from_json,
        vec2_from_json, vec3_from_json, EntityBounds, HandleInfo, HandleKind, PropertyFieldDef,
        PropertyValue, PropertyValueKind, PushPullAffordance, PushPullBlockReason,
    },
    capability_registry::{FaceId, GeneratedFaceRef},
    plugins::math::scale_point_around_center,
};

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoxPrimitive {
    pub centre: Vec3,
    pub half_extents: Vec3,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CylinderPrimitive {
    pub centre: Vec3,
    pub radius: f32,
    pub height: f32,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanePrimitive {
    pub corner_a: Vec2,
    pub corner_b: Vec2,
    pub elevation: f32,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Polyline {
    pub points: Vec<Vec3>,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElevationMetadata {
    pub source_layer: String,
    pub elevation: f32,
    pub survey_source_id: Option<String>,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriangleMesh {
    pub vertices: Vec<Vec3>,
    pub faces: Vec<[u32; 3]>,
    pub normals: Option<Vec<Vec3>>,
    pub name: Option<String>,
}

/// Triangular prism (wedge) for representing diagonal cuts.
///
/// The wedge is a box with one corner cut off diagonally. The `cut_face` and
/// `cut_corner` fields describe which face was cut and which corner was removed.
/// The result is 5 faces: 2 triangular end-caps and 3 rectangular sides.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WedgePrimitive {
    pub centre: Vec3,
    /// Full extents (width, height, depth) of the bounding box before the cut.
    pub extents: Vec3,
    /// Which face of the bounding box the diagonal cut is on.
    pub cut_face: CutFace,
    /// Which corner of that face is removed by the diagonal.
    pub cut_corner: CutCorner,
}

/// Which face of the bounding box contains the diagonal cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CutFace {
    /// Cut is on the XY faces (front/back, Z normal).
    XY,
    /// Cut is on the XZ faces (top/bottom, Y normal).
    XZ,
    /// Cut is on the YZ faces (left/right, X normal).
    YZ,
}

/// Which corner of the cut face is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CutCorner {
    /// In local face coordinates: (-u, -v) corner removed.
    MinMin,
    /// (+u, -v) corner removed.
    MaxMin,
    /// (+u, +v) corner removed.
    MaxMax,
    /// (-u, +v) corner removed.
    MinMax,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ShapeRotation(pub Quat);

impl Default for ShapeRotation {
    fn default() -> Self {
        Self(Quat::IDENTITY)
    }
}

// ---------------------------------------------------------------------------
// Primitive impl for BoxPrimitive
// ---------------------------------------------------------------------------

impl Primitive for BoxPrimitive {
    const TYPE_NAME: &'static str = "box";

    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            centre: self.centre + delta,
            half_extents: self.half_extents,
        }
    }

    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat) {
        (
            Self {
                centre: rotation * self.centre,
                half_extents: self.half_extents,
            },
            rotation * current_rotation,
        )
    }

    fn scaled(&self, factor: Vec3, center: Vec3) -> Self {
        Self {
            centre: scale_point_around_center(self.centre, center, factor),
            half_extents: Vec3::new(
                self.half_extents.x * factor.x.abs(),
                self.half_extents.y * factor.y.abs(),
                self.half_extents.z * factor.z.abs(),
            ),
        }
    }

    fn shape_eq(&self, other: &Self) -> bool {
        self.half_extents == other.half_extents
    }

    fn entity_transform(&self, rotation: Quat) -> Transform {
        Transform::from_translation(self.centre).with_rotation(rotation)
    }

    fn bounds(&self, rotation: Quat) -> Option<EntityBounds> {
        let corners = box_primitive_corners(self, rotation);
        Some(bounds_from_points(&corners))
    }

    fn draw_wireframe(&self, gizmos: &mut Gizmos, rotation: Quat, color: Color) {
        let corners = box_primitive_corners(self, rotation);
        let top = &corners[4..8];
        let bottom = &corners[0..4];
        for index in 0..4 {
            let next_index = (index + 1) % 4;
            gizmos.line(bottom[index], bottom[next_index], color);
            gizmos.line(top[index], top[next_index], color);
            gizmos.line(bottom[index], top[index], color);
        }
    }

    fn wireframe_line_count(&self) -> usize {
        12
    }

    fn push_pull(
        &self,
        face_id: FaceId,
        distance: f32,
        rotation: Quat,
        _element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>> {
        let (axis, sign) = face_id.box_axis_sign();
        let local_shift = sign * distance * 0.5;
        let mut local_half = [
            self.half_extents.x,
            self.half_extents.y,
            self.half_extents.z,
        ];
        local_half[axis] = (local_half[axis] + local_shift).abs().max(0.005);
        let new_half_extents = Vec3::from_array(local_half);

        let unit = [Vec3::X, Vec3::Y, Vec3::Z][axis];
        let world_dir = rotation * unit;
        let new_centre = self.centre + world_dir * local_shift;

        Some(PrimitivePushPullResult::SameType(
            Self {
                centre: new_centre,
                half_extents: new_half_extents,
            },
            rotation,
        ))
    }

    fn push_pull_affordance(&self, face_id: FaceId) -> PushPullAffordance {
        if face_id.box_axis_sign().0 < 3 {
            PushPullAffordance::Allowed
        } else {
            PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace)
        }
    }

    fn generated_face_ref(&self, face_id: FaceId) -> Option<GeneratedFaceRef> {
        let (axis, sign) = face_id.box_axis_sign();
        (axis < 3).then_some(GeneratedFaceRef::BoxFace {
            axis: axis as u8,
            positive: sign > 0.0,
        })
    }

    fn property_fields(&self, _rotation: Quat) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "center",
                "center",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.centre)),
                true,
            ),
            property_field(
                "half_extents",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.half_extents)),
            ),
        ]
    }

    fn set_property(&self, name: &str, value: &Value) -> Result<Self, String> {
        let mut prim = self.clone();
        match name {
            "centre" | "center" => prim.centre = vec3_from_json(value)?,
            "half_extents" => prim.half_extents = vec3_from_json(value)?,
            _ => return Err(invalid_property_error("box", &["center", "half_extents"])),
        }
        Ok(prim)
    }

    fn handles(&self, rotation: Quat) -> Vec<HandleInfo> {
        let corners = box_primitive_corners(self, rotation);
        let mut handles: Vec<HandleInfo> = corners
            .into_iter()
            .enumerate()
            .map(|(index, position)| HandleInfo {
                id: format!("corner_{index}"),
                position,
                kind: HandleKind::Vertex,
                label: format!("Corner {}", index + 1),
            })
            .collect();
        handles.push(HandleInfo {
            id: "centre".to_string(),
            position: self.centre,
            kind: HandleKind::Center,
            label: "Centre".to_string(),
        });
        handles
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3, _rotation: Quat) -> Option<Self> {
        None
    }

    fn label(&self) -> String {
        format!(
            "Box at ({:.2}, {:.2}, {:.2})",
            self.centre.x, self.centre.y, self.centre.z
        )
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(value.clone()).map_err(|e| e.to_string())
    }

    fn to_editable_mesh(&self, rotation: Quat) -> Option<EditableMesh> {
        Some(EditableMesh::from_box(self, &ShapeRotation(rotation)))
    }
}

impl MeshGenerator for BoxPrimitive {
    fn to_bevy_mesh(&self, _rotation: Quat) -> Mesh {
        Mesh::from(Cuboid::new(
            self.half_extents.x * 2.0,
            self.half_extents.y * 2.0,
            self.half_extents.z * 2.0,
        ))
    }
}

// ---------------------------------------------------------------------------
// Primitive impl for CylinderPrimitive
// ---------------------------------------------------------------------------

const CYLINDER_OUTLINE_SEGMENTS: usize = 24;

impl Primitive for CylinderPrimitive {
    const TYPE_NAME: &'static str = "cylinder";

    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            centre: self.centre + delta,
            radius: self.radius,
            height: self.height,
        }
    }

    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat) {
        (
            Self {
                centre: rotation * self.centre,
                radius: self.radius,
                height: self.height,
            },
            rotation * current_rotation,
        )
    }

    fn scaled(&self, factor: Vec3, center: Vec3) -> Self {
        Self {
            centre: scale_point_around_center(self.centre, center, factor),
            radius: self.radius * factor.x.abs().max(factor.z.abs()),
            height: self.height * factor.y.abs(),
        }
    }

    fn shape_eq(&self, other: &Self) -> bool {
        self.radius == other.radius && self.height == other.height
    }

    fn entity_transform(&self, rotation: Quat) -> Transform {
        Transform::from_translation(self.centre).with_rotation(rotation)
    }

    fn bounds(&self, _rotation: Quat) -> Option<EntityBounds> {
        Some(EntityBounds {
            min: self.centre - Vec3::new(self.radius, self.height * 0.5, self.radius),
            max: self.centre + Vec3::new(self.radius, self.height * 0.5, self.radius),
        })
    }

    fn draw_wireframe(&self, gizmos: &mut Gizmos, _rotation: Quat, color: Color) {
        let bottom_y = self.centre.y - self.height * 0.5;
        let top_y = self.centre.y + self.height * 0.5;
        let mut previous_bottom = None;
        let mut previous_top = None;
        let mut first_bottom = None;
        let mut first_top = None;

        for index in 0..=CYLINDER_OUTLINE_SEGMENTS {
            let angle = index as f32 / CYLINDER_OUTLINE_SEGMENTS as f32 * std::f32::consts::TAU;
            let offset = Vec3::new(self.radius * angle.cos(), 0.0, self.radius * angle.sin());
            let bottom = Vec3::new(self.centre.x, bottom_y, self.centre.z) + offset;
            let top = Vec3::new(self.centre.x, top_y, self.centre.z) + offset;

            if let Some(prev) = previous_bottom {
                gizmos.line(prev, bottom, color);
            } else {
                first_bottom = Some(bottom);
            }
            if let Some(prev) = previous_top {
                gizmos.line(prev, top, color);
            } else {
                first_top = Some(top);
            }

            if index % (CYLINDER_OUTLINE_SEGMENTS / 4) == 0 && index < CYLINDER_OUTLINE_SEGMENTS {
                gizmos.line(bottom, top, color);
            }

            previous_bottom = Some(bottom);
            previous_top = Some(top);
        }

        if let (Some(first), Some(last)) = (first_bottom, previous_bottom) {
            gizmos.line(last, first, color);
        }
        if let (Some(first), Some(last)) = (first_top, previous_top) {
            gizmos.line(last, first, color);
        }
    }

    fn wireframe_line_count(&self) -> usize {
        CYLINDER_OUTLINE_SEGMENTS * 2 + 6
    }

    fn push_pull(
        &self,
        face_id: FaceId,
        distance: f32,
        _rotation: Quat,
        _element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>> {
        match face_id.0 {
            // Top cap (face 0): push upward extends height, shifts centre up
            0 => {
                let raw = self.height + distance;
                let new_height = raw.abs().max(0.005);
                let world_up = _rotation * Vec3::Y;
                Some(PrimitivePushPullResult::SameType(
                    Self {
                        centre: self.centre + world_up * (distance * 0.5),
                        radius: self.radius,
                        height: new_height,
                    },
                    _rotation,
                ))
            }
            // Bottom cap (face 1): push downward extends height, shifts centre down
            1 => {
                let raw = self.height + distance;
                let new_height = raw.abs().max(0.005);
                let world_up = _rotation * Vec3::Y;
                Some(PrimitivePushPullResult::SameType(
                    Self {
                        centre: self.centre - world_up * (distance * 0.5),
                        radius: self.radius,
                        height: new_height,
                    },
                    _rotation,
                ))
            }
            // Side surface: not pushable
            _ => None,
        }
    }

    fn push_pull_affordance(&self, face_id: FaceId) -> PushPullAffordance {
        match face_id.0 {
            0 | 1 => PushPullAffordance::Allowed,
            i if i >= 2 => PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace),
            _ => PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace),
        }
    }

    fn generated_face_ref(&self, face_id: FaceId) -> Option<GeneratedFaceRef> {
        match face_id.0 {
            0 => Some(GeneratedFaceRef::CylinderTop),
            1 => Some(GeneratedFaceRef::CylinderBottom),
            i if i >= 2 => Some(GeneratedFaceRef::CylinderSide),
            _ => None,
        }
    }

    fn property_fields(&self, _rotation: Quat) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "center",
                "center",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.centre)),
                true,
            ),
            property_field(
                "radius",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.radius)),
            ),
            property_field(
                "height",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.height)),
            ),
        ]
    }

    fn set_property(&self, name: &str, value: &Value) -> Result<Self, String> {
        let mut prim = self.clone();
        match name {
            "centre" | "center" => prim.centre = vec3_from_json(value)?,
            "radius" => prim.radius = scalar_from_json(value)?,
            "height" => prim.height = scalar_from_json(value)?,
            _ => {
                return Err(invalid_property_error(
                    "cylinder",
                    &["center", "radius", "height"],
                ))
            }
        }
        Ok(prim)
    }

    fn handles(&self, _rotation: Quat) -> Vec<HandleInfo> {
        vec![
            HandleInfo {
                id: "centre".to_string(),
                position: self.centre,
                kind: HandleKind::Center,
                label: "Centre".to_string(),
            },
            HandleInfo {
                id: "top".to_string(),
                position: self.centre + Vec3::Y * (self.height * 0.5),
                kind: HandleKind::Parameter,
                label: "Top cap center".to_string(),
            },
            HandleInfo {
                id: "radius".to_string(),
                position: self.centre + Vec3::X * self.radius,
                kind: HandleKind::Parameter,
                label: "Radius handle".to_string(),
            },
        ]
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3, _rotation: Quat) -> Option<Self> {
        match handle_id {
            "radius" => Some(Self {
                radius: cursor.xz().distance(self.centre.xz()).max(0.01),
                ..self.clone()
            }),
            _ => None,
        }
    }

    fn label(&self) -> String {
        format!(
            "Cylinder at ({:.2}, {:.2}, {:.2})",
            self.centre.x, self.centre.y, self.centre.z
        )
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(value.clone()).map_err(|e| e.to_string())
    }

    fn to_editable_mesh(&self, rotation: Quat) -> Option<EditableMesh> {
        Some(EditableMesh::from_cylinder(
            self,
            &ShapeRotation(rotation),
            24,
        ))
    }
}

impl MeshGenerator for CylinderPrimitive {
    fn to_bevy_mesh(&self, _rotation: Quat) -> Mesh {
        Mesh::from(Cylinder::new(self.radius, self.height))
    }
}

// ---------------------------------------------------------------------------
// Primitive impl for PlanePrimitive
// ---------------------------------------------------------------------------

impl Primitive for PlanePrimitive {
    const TYPE_NAME: &'static str = "plane";

    fn centre(&self) -> Vec3 {
        let mid = (self.corner_a + self.corner_b) * 0.5;
        Vec3::new(mid.x, self.elevation, mid.y)
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            corner_a: self.corner_a + delta.xz(),
            corner_b: self.corner_b + delta.xz(),
            elevation: self.elevation + delta.y,
        }
    }

    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat) {
        (self.clone(), rotation * current_rotation)
    }

    fn scaled(&self, factor: Vec3, center: Vec3) -> Self {
        let corner_a_3d = scale_point_around_center(
            Vec3::new(self.corner_a.x, self.elevation, self.corner_a.y),
            center,
            factor,
        );
        let corner_b_3d = scale_point_around_center(
            Vec3::new(self.corner_b.x, self.elevation, self.corner_b.y),
            center,
            factor,
        );
        Self {
            corner_a: corner_a_3d.xz(),
            corner_b: corner_b_3d.xz(),
            elevation: (corner_a_3d.y + corner_b_3d.y) * 0.5,
        }
    }

    fn shape_eq(&self, other: &Self) -> bool {
        plane_size(self) == plane_size(other)
    }

    fn entity_transform(&self, rotation: Quat) -> Transform {
        let centre = (self.corner_a + self.corner_b) * 0.5;
        Transform::from_xyz(centre.x, self.elevation, centre.y)
            .with_rotation(rotation * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
    }

    fn bounds(&self, rotation: Quat) -> Option<EntityBounds> {
        let corners = plane_primitive_corners(self, rotation);
        Some(bounds_from_points(&corners))
    }

    fn draw_wireframe(&self, gizmos: &mut Gizmos, rotation: Quat, color: Color) {
        let corners = plane_primitive_corners(self, rotation);
        for index in 0..corners.len() {
            let next = (index + 1) % corners.len();
            gizmos.line(corners[index], corners[next], color);
        }
    }

    fn wireframe_line_count(&self) -> usize {
        4
    }

    fn push_pull(
        &self,
        _face_id: FaceId,
        distance: f32,
        rotation: Quat,
        element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>> {
        // A plane has only one face. Pushing it promotes to a box.
        let half_w = (self.corner_b.x - self.corner_a.x).abs() * 0.5;
        let half_d = (self.corner_b.y - self.corner_a.y).abs() * 0.5;
        let half_h = (distance * 0.5).abs().max(0.005);
        let mid = (self.corner_a + self.corner_b) * 0.5;
        let centre = Vec3::new(mid.x, self.elevation + distance * 0.5, mid.y);

        use super::generic_snapshot::PrimitiveSnapshot;
        use super::primitives::ShapeRotation;

        Some(PrimitivePushPullResult::Promoted(
            PrimitiveSnapshot {
                element_id,
                primitive: BoxPrimitive {
                    centre,
                    half_extents: Vec3::new(half_w.max(0.005), half_h, half_d.max(0.005)),
                },
                rotation: ShapeRotation(rotation),
            }
            .into(),
        ))
    }

    fn push_pull_affordance(&self, _face_id: FaceId) -> PushPullAffordance {
        PushPullAffordance::Allowed
    }

    fn generated_face_ref(&self, _face_id: FaceId) -> Option<GeneratedFaceRef> {
        Some(GeneratedFaceRef::PlaneFace)
    }

    fn property_fields(&self, _rotation: Quat) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "corner_a",
                "corner_a",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(plane_corner_vec3(
                    self.corner_a,
                    self.elevation,
                ))),
                true,
            ),
            property_field_with(
                "corner_b",
                "corner_b",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(plane_corner_vec3(
                    self.corner_b,
                    self.elevation,
                ))),
                true,
            ),
        ]
    }

    fn set_property(&self, name: &str, value: &Value) -> Result<Self, String> {
        let mut prim = self.clone();
        match name {
            "corner_a" => {
                let corner = plane_corner_from_json(value, prim.elevation)?;
                prim.corner_a = corner.xz();
                prim.elevation = corner.y;
            }
            "corner_b" => {
                let corner = plane_corner_from_json(value, prim.elevation)?;
                prim.corner_b = corner.xz();
                prim.elevation = corner.y;
            }
            "elevation" => prim.elevation = scalar_from_json(value)?,
            _ => return Err(invalid_property_error("plane", &["corner_a", "corner_b"])),
        }
        Ok(prim)
    }

    fn handles(&self, rotation: Quat) -> Vec<HandleInfo> {
        let corners = plane_primitive_corners(self, rotation);
        let mut handles: Vec<HandleInfo> = corners
            .into_iter()
            .enumerate()
            .map(|(index, position)| HandleInfo {
                id: format!("corner_{index}"),
                position,
                kind: HandleKind::Vertex,
                label: format!("Corner {}", index + 1),
            })
            .collect();
        handles.push(HandleInfo {
            id: "centre".to_string(),
            position: self.centre(),
            kind: HandleKind::Center,
            label: "Centre".to_string(),
        });
        handles
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3, _rotation: Quat) -> Option<Self> {
        let mut prim = self.clone();
        match handle_id {
            "corner_0" | "corner_1" => prim.corner_a = cursor.xz(),
            "corner_2" | "corner_3" => prim.corner_b = cursor.xz(),
            _ => return None,
        }
        Some(prim)
    }

    fn label(&self) -> String {
        format!("Plane at elevation {:.2}", self.elevation)
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(value.clone()).map_err(|e| e.to_string())
    }

    fn to_editable_mesh(&self, _rotation: Quat) -> Option<EditableMesh> {
        Some(EditableMesh::from_plane(self))
    }
}

impl MeshGenerator for PlanePrimitive {
    const MATERIAL_KIND: MeshMaterialKind = MeshMaterialKind::Plane;

    fn to_bevy_mesh(&self, _rotation: Quat) -> Mesh {
        let size = (self.corner_b - self.corner_a).abs();
        Mesh::from(Rectangle::new(size.x.max(0.001), size.y.max(0.001)))
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn box_primitive_corners(primitive: &BoxPrimitive, rotation: Quat) -> [Vec3; 8] {
    let h = primitive.half_extents;
    let corners = [
        Vec3::new(-h.x, -h.y, -h.z),
        Vec3::new(-h.x, -h.y, h.z),
        Vec3::new(h.x, -h.y, h.z),
        Vec3::new(h.x, -h.y, -h.z),
        Vec3::new(-h.x, h.y, -h.z),
        Vec3::new(-h.x, h.y, h.z),
        Vec3::new(h.x, h.y, h.z),
        Vec3::new(h.x, h.y, -h.z),
    ];
    corners.map(|corner| primitive.centre + rotation * corner)
}

pub fn bounds_from_points(points: &[Vec3]) -> EntityBounds {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for point in points {
        min = min.min(*point);
        max = max.max(*point);
    }
    EntityBounds { min, max }
}

fn plane_primitive_corners(primitive: &PlanePrimitive, rotation: Quat) -> [Vec3; 4] {
    let size = primitive.corner_b - primitive.corner_a;
    let half_size = size.abs() * 0.5;
    let centre_2d = (primitive.corner_a + primitive.corner_b) * 0.5;
    let corners = [
        Vec3::new(-half_size.x, 0.0, -half_size.y),
        Vec3::new(-half_size.x, 0.0, half_size.y),
        Vec3::new(half_size.x, 0.0, half_size.y),
        Vec3::new(half_size.x, 0.0, -half_size.y),
    ];
    let centre = Vec3::new(centre_2d.x, primitive.elevation, centre_2d.y);
    corners.map(|corner| centre + rotation * corner)
}

fn plane_size(primitive: &PlanePrimitive) -> Vec2 {
    (primitive.corner_b - primitive.corner_a).abs()
}

fn plane_corner_vec3(corner: Vec2, elevation: f32) -> Vec3 {
    Vec3::new(corner.x, elevation, corner.y)
}

fn plane_corner_from_json(value: &Value, fallback_elevation: f32) -> Result<Vec3, String> {
    vec3_from_json(value).or_else(|_| {
        let corner = vec2_from_json(value)?;
        Ok(Vec3::new(corner.x, fallback_elevation, corner.y))
    })
}
