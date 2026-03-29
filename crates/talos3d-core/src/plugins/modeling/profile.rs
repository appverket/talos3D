use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::editable_mesh::{self, EditableMesh};
use super::primitive_trait::{MeshGenerator, Primitive, PrimitivePushPullResult};
use super::triangulate::ear_clip_triangulate;
use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, scalar_from_json,
        vec3_from_json, EntityBounds, HandleInfo, HandleKind, PropertyFieldDef, PropertyValue,
        PropertyValueKind, PushPullAffordance, PushPullBlockReason,
    },
    capability_registry::{FaceId, GeneratedFaceRef},
    plugins::math::scale_point_around_center,
};

use super::primitives::bounds_from_points;

// ---------------------------------------------------------------------------
// Profile2d — 2D profile contour
// ---------------------------------------------------------------------------

/// A closed 2D contour made of line and arc segments.
///
/// The profile starts at `start` and each segment advances to its endpoint.
/// The contour is implicitly closed: the last segment's endpoint connects
/// back to `start` with a straight line (unless it already coincides).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile2d {
    /// Starting point of the contour.
    pub start: Vec2,
    /// Ordered segments forming the contour.
    pub segments: Vec<ProfileSegment>,
}

/// A single segment in a 2D profile contour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "seg", rename_all = "snake_case")]
pub enum ProfileSegment {
    /// Straight line to the given point.
    LineTo { to: Vec2 },
    /// Circular arc to the given point.
    ///
    /// `bulge` is the tangent of 1/4 the included arc angle (DWG/DXF convention).
    /// Positive bulges left relative to the travel direction.
    /// A bulge of 0 is equivalent to a straight line.
    ArcTo { to: Vec2, bulge: f32 },
}

impl ProfileSegment {
    /// The endpoint of this segment.
    pub fn endpoint(&self) -> Vec2 {
        match self {
            ProfileSegment::LineTo { to } => *to,
            ProfileSegment::ArcTo { to, .. } => *to,
        }
    }
}

/// Default number of line segments per full circle when tessellating arcs.
const ARC_SEGMENTS_PER_CIRCLE: u32 = 32;

impl Profile2d {
    /// Create a rectangular profile centred at the origin.
    ///
    /// The rectangle extends from `(-width/2, -depth/2)` to `(width/2, depth/2)`.
    /// Winding is counter-clockwise.
    pub fn rectangle(width: f32, depth: f32) -> Self {
        let hw = width * 0.5;
        let hd = depth * 0.5;
        Profile2d {
            start: Vec2::new(-hw, -hd),
            segments: vec![
                ProfileSegment::LineTo {
                    to: Vec2::new(hw, -hd),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(hw, hd),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(-hw, hd),
                },
            ],
        }
    }

    /// Create an L-shaped profile.
    ///
    /// The L is formed from a rectangle with a rectangular notch removed from one corner.
    /// `outer_width` and `outer_depth` define the bounding rectangle.
    /// `notch_width` and `notch_depth` define the removed corner.
    pub fn l_shape(outer_width: f32, outer_depth: f32, notch_width: f32, notch_depth: f32) -> Self {
        Profile2d {
            start: Vec2::ZERO,
            segments: vec![
                ProfileSegment::LineTo {
                    to: Vec2::new(outer_width, 0.0),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(outer_width, outer_depth - notch_depth),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(outer_width - notch_width, outer_depth - notch_depth),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(outer_width - notch_width, outer_depth),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(0.0, outer_depth),
                },
            ],
        }
    }

    /// All vertices of the contour in order, including the start point.
    /// For segments with arcs, the arc is tessellated into line segments.
    pub fn tessellate(&self, segments_per_circle: u32) -> Vec<Vec2> {
        let mut points = vec![self.start];
        let mut current = self.start;

        for segment in &self.segments {
            match segment {
                ProfileSegment::LineTo { to } => {
                    points.push(*to);
                    current = *to;
                }
                ProfileSegment::ArcTo { to, bulge } => {
                    if bulge.abs() < 1e-6 {
                        // Effectively a straight line.
                        points.push(*to);
                    } else {
                        let arc_points = tessellate_arc(current, *to, *bulge, segments_per_circle);
                        // arc_points excludes the start point, includes the end.
                        points.extend(arc_points);
                    }
                    current = *to;
                }
            }
        }

        // The contour is implicitly closed — caller handles closing.
        points
    }

    /// Number of profile segments (not tessellated vertices).
    /// The implicit closing segment (last point → start) is counted
    /// only if the contour does not already close.
    pub fn segment_count(&self) -> usize {
        let n = self.segments.len();
        if n == 0 {
            return 0;
        }
        let last_endpoint = self.segments.last().unwrap().endpoint();
        if (last_endpoint - self.start).length_squared() < 1e-6 {
            n
        } else {
            n + 1 // implicit closing line
        }
    }

    /// Whether the profile contour winds counter-clockwise.
    pub fn is_ccw(&self) -> bool {
        let pts = self.tessellate(ARC_SEGMENTS_PER_CIRCLE);
        signed_area_2d(&pts) > 0.0
    }

    /// Return a copy with reversed winding.
    pub fn reversed(&self) -> Self {
        let mut all_points: Vec<Vec2> = std::iter::once(self.start)
            .chain(self.segments.iter().map(|s| s.endpoint()))
            .collect();

        // Check if the last point closes back to start (explicit close).
        let explicitly_closed = all_points.len() > 1
            && (all_points.last().unwrap() - all_points[0]).length_squared() < 1e-6;
        if explicitly_closed {
            all_points.pop();
        }

        all_points.reverse();
        let new_start = all_points[0];
        // Reverse loses arc info — convert arcs to lines for now.
        // TODO: preserve arc segments with negated bulge on reversal.
        let new_segments: Vec<ProfileSegment> = all_points[1..]
            .iter()
            .map(|&to| ProfileSegment::LineTo { to })
            .collect();

        Profile2d {
            start: new_start,
            segments: new_segments,
        }
    }

    /// Translate all points by an offset.
    pub fn translated(&self, offset: Vec2) -> Self {
        Profile2d {
            start: self.start + offset,
            segments: self
                .segments
                .iter()
                .map(|seg| match seg {
                    ProfileSegment::LineTo { to } => ProfileSegment::LineTo { to: *to + offset },
                    ProfileSegment::ArcTo { to, bulge } => ProfileSegment::ArcTo {
                        to: *to + offset,
                        bulge: *bulge,
                    },
                })
                .collect(),
        }
    }

    /// Scale all points around a centre.
    pub fn scaled(&self, factor: Vec2, center: Vec2) -> Self {
        let scale_pt = |p: Vec2| -> Vec2 { center + (p - center) * factor };
        Profile2d {
            start: scale_pt(self.start),
            segments: self
                .segments
                .iter()
                .map(|seg| match seg {
                    ProfileSegment::LineTo { to } => ProfileSegment::LineTo { to: scale_pt(*to) },
                    ProfileSegment::ArcTo { to, bulge } => ProfileSegment::ArcTo {
                        to: scale_pt(*to),
                        bulge: *bulge,
                    },
                })
                .collect(),
        }
    }

    /// Offset a single segment by moving it along its outward normal.
    ///
    /// `segment_index` is the index into `self.segments` (or the implicit closing
    /// segment if `segment_index == self.segments.len()`).
    ///
    /// For line segments, both the segment's start and end points are shifted
    /// perpendicular to the edge direction. Adjacent segments are adjusted to
    /// maintain connectivity.
    ///
    /// Returns `None` if the index is out of range or offsetting is not supported
    /// for this segment type yet.
    pub fn offset_segment(&self, segment_index: usize, distance: f32) -> Option<Self> {
        let seg_count = self.segment_count();
        if segment_index >= seg_count {
            return None;
        }

        // Get start and end points for this segment.
        let seg_start = if segment_index == 0 {
            self.start
        } else {
            self.segments[segment_index - 1].endpoint()
        };

        let is_implicit_close = segment_index == self.segments.len();
        let seg_end = if is_implicit_close {
            self.start
        } else {
            self.segments[segment_index].endpoint()
        };

        // For arcs, we don't support offsetting yet.
        if !is_implicit_close {
            if let ProfileSegment::ArcTo { .. } = &self.segments[segment_index] {
                return None;
            }
        }

        // Compute the outward normal for this edge (90° CCW rotation of edge direction).
        let edge = seg_end - seg_start;
        if edge.length_squared() < 1e-10 {
            return None;
        }
        let normal = Vec2::new(-edge.y, edge.x).normalize();
        let offset = normal * distance;

        // Simple approach: offset both endpoints of this segment.
        // This is correct for parallel offsets but doesn't handle
        // corner intersection cleanup (that's a future enhancement).
        let mut new_profile = self.clone();

        if segment_index == 0 {
            new_profile.start = self.start + offset;
        } else {
            match &mut new_profile.segments[segment_index - 1] {
                ProfileSegment::LineTo { to } => *to = *to + offset,
                ProfileSegment::ArcTo { to, .. } => *to = *to + offset,
            }
        }

        if is_implicit_close {
            // The implicit close segment ends at start, which we already moved above.
            // We also need to move the last explicit segment's endpoint.
            if let Some(last_seg) = new_profile.segments.last_mut() {
                match last_seg {
                    ProfileSegment::LineTo { to } => *to = *to + offset,
                    ProfileSegment::ArcTo { to, .. } => *to = *to + offset,
                }
            }
        } else {
            match &mut new_profile.segments[segment_index] {
                ProfileSegment::LineTo { to } => *to = *to + offset,
                ProfileSegment::ArcTo { to, .. } => *to = *to + offset,
            }
        }

        Some(new_profile)
    }

    /// Compute the 2D axis-aligned bounding box of the tessellated profile.
    pub fn bounds_2d(&self) -> (Vec2, Vec2) {
        let pts = self.tessellate(ARC_SEGMENTS_PER_CIRCLE);
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for p in &pts {
            min = min.min(*p);
            max = max.max(*p);
        }
        (min, max)
    }
}

/// Signed area of a 2D polygon (positive = CCW).
fn signed_area_2d(pts: &[Vec2]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut area = 0.0f32;
    for i in 0..n {
        let j = (i + 1) % n;
        area += pts[i].x * pts[j].y;
        area -= pts[j].x * pts[i].y;
    }
    area * 0.5
}

/// Tessellate a circular arc defined by bulge factor.
///
/// Returns intermediate points (excluding `from`, including `to`).
fn tessellate_arc(from: Vec2, to: Vec2, bulge: f32, segments_per_circle: u32) -> Vec<Vec2> {
    // Bulge = tan(angle/4).
    let angle = 4.0 * bulge.atan();
    let n_segments = ((angle.abs() / std::f32::consts::TAU) * segments_per_circle as f32)
        .ceil()
        .max(2.0) as u32;

    // Chord midpoint and sagitta.
    let chord = to - from;
    let chord_len = chord.length();
    if chord_len < 1e-8 {
        return vec![to];
    }

    let mid = (from + to) * 0.5;
    let sagitta = bulge * chord_len * 0.5;
    let perp = Vec2::new(-chord.y, chord.x).normalize();
    let apex = mid + perp * sagitta;

    // Compute arc center from endpoints and bulge.
    let radius = chord_len * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
    let center_offset = radius - sagitta.abs();
    let center_dir = if bulge > 0.0 { -perp } else { perp };
    let center = apex + center_dir * center_offset;

    // Start and end angles.
    let start_angle = (from - center).to_angle();

    let mut points = Vec::with_capacity(n_segments as usize);
    for i in 1..=n_segments {
        let t = i as f32 / n_segments as f32;
        let a = start_angle + angle * t;
        let p = center + Vec2::new(a.cos(), a.sin()) * radius.abs();
        points.push(p);
    }

    // Ensure the last point is exactly `to` to avoid floating-point drift.
    if let Some(last) = points.last_mut() {
        *last = to;
    }

    points
}

// ---------------------------------------------------------------------------
// ProfileExtrusion — a Profile2d extruded along local Y
// ---------------------------------------------------------------------------

/// A solid formed by extruding a 2D profile along the local Y axis.
///
/// The profile lies in the local XZ plane (profile X → local X, profile Y → local Z).
/// The extrusion rises from `y = -height/2` to `y = +height/2` relative to `centre`.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileExtrusion {
    /// World-space centre of the extrusion's bounding volume.
    pub centre: Vec3,
    /// The 2D profile contour.
    pub profile: Profile2d,
    /// Total height of the extrusion along local Y.
    pub height: f32,
}

// Face ID layout:
//   0 = top cap (+Y)
//   1 = bottom cap (-Y)
//   2..2+N = side faces, one per profile segment (including implicit closing segment)
impl ProfileExtrusion {
    /// Number of side faces (one per profile segment, including closing).
    fn side_face_count(&self) -> u32 {
        self.profile.segment_count() as u32
    }

    /// Map a FaceId to a human/AI-readable face name.
    ///
    /// - `FaceId(0)` → `"top"`
    /// - `FaceId(1)` → `"bottom"`
    /// - `FaceId(2..2+N)` → `"side:0"`, `"side:1"`, etc.
    pub fn face_name(&self, face_id: FaceId) -> Option<String> {
        let n = self.side_face_count();
        match face_id.0 {
            0 => Some("top".to_string()),
            1 => Some("bottom".to_string()),
            i if i >= 2 && i < 2 + n => Some(format!("side:{}", i - 2)),
            _ => None,
        }
    }

    /// Parse a face name back to a FaceId.
    ///
    /// Accepts `"top"`, `"bottom"`, `"side:N"`.
    pub fn face_id_from_name(&self, name: &str) -> Option<FaceId> {
        match name {
            "top" => Some(FaceId(0)),
            "bottom" => Some(FaceId(1)),
            s if s.starts_with("side:") => {
                let idx: u32 = s[5..].parse().ok()?;
                if idx < self.side_face_count() {
                    Some(FaceId(2 + idx))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn semantic_face_ref(&self, face_id: FaceId) -> Option<GeneratedFaceRef> {
        match face_id.0 {
            0 => Some(GeneratedFaceRef::ProfileTop),
            1 => Some(GeneratedFaceRef::ProfileBottom),
            side_face if side_face >= 2 => {
                let segment_index = (side_face - 2) as usize;
                if segment_index < self.profile.segments.len() {
                    match self.profile.segments[segment_index] {
                        ProfileSegment::LineTo { .. } => {
                            Some(GeneratedFaceRef::ProfileSideSegment(segment_index as u32))
                        }
                        ProfileSegment::ArcTo { .. } => Some(
                            GeneratedFaceRef::ProfileSideArcSegment(segment_index as u32),
                        ),
                    }
                } else if segment_index == self.profile.segments.len()
                    && self.profile.segment_count() > self.profile.segments.len()
                {
                    Some(GeneratedFaceRef::ProfileSideClosingSegment)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl Primitive for ProfileExtrusion {
    const TYPE_NAME: &'static str = "profile_extrusion";

    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            centre: self.centre + delta,
            profile: self.profile.clone(),
            height: self.height,
        }
    }

    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat) {
        (
            Self {
                centre: rotation * self.centre,
                profile: self.profile.clone(),
                height: self.height,
            },
            rotation * current_rotation,
        )
    }

    fn scaled(&self, factor: Vec3, center: Vec3) -> Self {
        let profile_scale = Vec2::new(factor.x.abs(), factor.z.abs());
        let profile_center = Vec2::ZERO; // profile is in local space
        Self {
            centre: scale_point_around_center(self.centre, center, factor),
            profile: self.profile.scaled(profile_scale, profile_center),
            height: self.height * factor.y.abs(),
        }
    }

    fn shape_eq(&self, other: &Self) -> bool {
        self.profile == other.profile && self.height == other.height
    }

    fn entity_transform(&self, rotation: Quat) -> Transform {
        Transform::from_translation(self.centre).with_rotation(rotation)
    }

    fn bounds(&self, rotation: Quat) -> Option<EntityBounds> {
        let (pmin, pmax) = self.profile.bounds_2d();
        let half_h = self.height * 0.5;
        // Profile X → local X, profile Y → local Z
        let local_corners = [
            Vec3::new(pmin.x, -half_h, pmin.y),
            Vec3::new(pmin.x, -half_h, pmax.y),
            Vec3::new(pmax.x, -half_h, pmin.y),
            Vec3::new(pmax.x, -half_h, pmax.y),
            Vec3::new(pmin.x, half_h, pmin.y),
            Vec3::new(pmin.x, half_h, pmax.y),
            Vec3::new(pmax.x, half_h, pmin.y),
            Vec3::new(pmax.x, half_h, pmax.y),
        ];
        let world_corners: Vec<Vec3> = local_corners
            .iter()
            .map(|c| self.centre + rotation * *c)
            .collect();
        Some(bounds_from_points(&world_corners))
    }

    fn draw_wireframe(&self, gizmos: &mut Gizmos, rotation: Quat, color: Color) {
        let pts = self.profile.tessellate(ARC_SEGMENTS_PER_CIRCLE);
        let half_h = self.height * 0.5;

        let to_world =
            |p: Vec2, y: f32| -> Vec3 { self.centre + rotation * Vec3::new(p.x, y, p.y) };

        let n = pts.len();
        for i in 0..n {
            let j = (i + 1) % n;
            // Bottom edge
            gizmos.line(to_world(pts[i], -half_h), to_world(pts[j], -half_h), color);
            // Top edge
            gizmos.line(to_world(pts[i], half_h), to_world(pts[j], half_h), color);
        }
        // Vertical edges at profile vertices
        for p in &pts {
            gizmos.line(to_world(*p, -half_h), to_world(*p, half_h), color);
        }
    }

    fn wireframe_line_count(&self) -> usize {
        let n = self.profile.tessellate(ARC_SEGMENTS_PER_CIRCLE).len();
        n * 3 // bottom edges + top edges + vertical edges
    }

    fn push_pull(
        &self,
        face_id: FaceId,
        distance: f32,
        rotation: Quat,
        _element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>> {
        match face_id.0 {
            // Top cap: extend height upward (or inward when distance is negative)
            0 => {
                let raw = self.height + distance;
                let new_height = raw.abs().max(0.005);
                let world_up = rotation * Vec3::Y;
                Some(PrimitivePushPullResult::SameType(
                    Self {
                        centre: self.centre + world_up * (distance * 0.5),
                        profile: self.profile.clone(),
                        height: new_height,
                    },
                    rotation,
                ))
            }
            // Bottom cap: extend height downward (or inward when distance is negative)
            1 => {
                let raw = self.height + distance;
                let new_height = raw.abs().max(0.005);
                let world_up = rotation * Vec3::Y;
                Some(PrimitivePushPullResult::SameType(
                    Self {
                        centre: self.centre - world_up * (distance * 0.5),
                        profile: self.profile.clone(),
                        height: new_height,
                    },
                    rotation,
                ))
            }
            // Side-face push/pull is intentionally unsupported for profile extrusions.
            // Cap push/pull preserves a coherent one-axis semantic model; side-face moves do not.
            _ => None,
        }
    }

    fn push_pull_affordance(&self, face_id: FaceId) -> PushPullAffordance {
        match self.semantic_face_ref(face_id) {
            Some(GeneratedFaceRef::ProfileTop | GeneratedFaceRef::ProfileBottom) => {
                PushPullAffordance::Allowed
            }
            Some(
                GeneratedFaceRef::ProfileSideSegment(_)
                | GeneratedFaceRef::ProfileSideArcSegment(_)
                | GeneratedFaceRef::ProfileSideClosingSegment,
            ) => PushPullAffordance::Blocked(PushPullBlockReason::CapOnly),
            None => PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace),
            _ => PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace),
        }
    }

    fn generated_face_ref(&self, face_id: FaceId) -> Option<GeneratedFaceRef> {
        self.semantic_face_ref(face_id)
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
            "height" => prim.height = scalar_from_json(value)?,
            "profile" => {
                prim.profile = serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid profile: {e}"))?;
            }
            _ => {
                return Err(invalid_property_error(
                    "profile_extrusion",
                    &["center", "height", "profile"],
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
        ]
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3, _rotation: Quat) -> Option<Self> {
        None
    }

    fn label(&self) -> String {
        format!(
            "Profile extrusion at ({:.2}, {:.2}, {:.2})",
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
        Some(build_extrusion_editable_mesh(self, rotation))
    }
}

impl MeshGenerator for ProfileExtrusion {
    fn to_bevy_mesh(&self, _rotation: Quat) -> Mesh {
        build_extrusion_bevy_mesh(self)
    }
}

// ---------------------------------------------------------------------------
// Mesh generation
// ---------------------------------------------------------------------------

/// Build a Bevy mesh from a profile extrusion.
fn build_extrusion_bevy_mesh(extrusion: &ProfileExtrusion) -> Mesh {
    let mut profile = extrusion.profile.clone();
    if !profile.is_ccw() {
        profile = profile.reversed();
    }

    let pts = profile.tessellate(ARC_SEGMENTS_PER_CIRCLE);
    let n = pts.len();
    if n < 3 {
        return Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
    }

    let half_h = extrusion.height * 0.5;

    // Profile X → local X, Profile Y → local Z
    let to_3d = |p: Vec2, y: f32| -> [f32; 3] { [p.x, y, p.y] };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Ear-clip triangulate the 2D profile for the caps.
    let cap_tris = ear_clip_triangulate(&pts);

    // Note on winding: ear_clip_triangulate produces CCW triangles in 2D (X,Y).
    // But to_3d maps 2D (X,Y) to 3D (X, height, Y) — this flips the handedness.
    // So CCW in 2D becomes CW in 3D. We need to reverse the winding for the top cap
    // (which should face +Y) and keep it as-is for the bottom cap (which should face -Y).

    // --- Top cap (face 0) — needs reversed winding to face +Y ---
    let top_base = positions.len() as u32;
    for p in &pts {
        positions.push(to_3d(*p, half_h));
        normals.push([0.0, 1.0, 0.0]);
    }
    for [a, b, c] in &cap_tris {
        indices.push(top_base + a);
        indices.push(top_base + c);
        indices.push(top_base + b);
    }

    // --- Bottom cap (face 1) — ear-clip winding is already correct for -Y ---
    let bottom_base = positions.len() as u32;
    for p in &pts {
        positions.push(to_3d(*p, -half_h));
        normals.push([0.0, -1.0, 0.0]);
    }
    for [a, b, c] in &cap_tris {
        indices.push(bottom_base + a);
        indices.push(bottom_base + b);
        indices.push(bottom_base + c);
    }

    // --- Side faces ---
    // Compute normals from the actual 3D triangle vertices to guarantee
    // consistency with the winding order (avoids the to_3d handedness pitfall).
    for i in 0..n {
        let j = (i + 1) % n;
        let p0 = pts[i];
        let p1 = pts[j];

        let v0 = Vec3::from_array(to_3d(p0, -half_h));
        let v1 = Vec3::from_array(to_3d(p1, -half_h));
        let v2 = Vec3::from_array(to_3d(p1, half_h));
        // Normal from the first triangle's winding (matching the index order below)
        let face_normal = (v2 - v0).cross(v1 - v0).normalize_or_zero();
        let normal = [face_normal.x, face_normal.y, face_normal.z];

        let base = positions.len() as u32;
        positions.push(to_3d(p0, -half_h));
        positions.push(to_3d(p1, -half_h));
        positions.push(to_3d(p1, half_h));
        positions.push(to_3d(p0, half_h));
        normals.push(normal);
        normals.push(normal);
        normals.push(normal);
        normals.push(normal);

        // Two triangles — winding reversed for the to_3d handedness flip
        indices.push(base);
        indices.push(base + 2);
        indices.push(base + 1);
        indices.push(base);
        indices.push(base + 3);
        indices.push(base + 2);
    }

    let uv_count = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; uv_count]);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Build a half-edge EditableMesh for face hit testing and topology queries.
fn build_extrusion_editable_mesh(extrusion: &ProfileExtrusion, rotation: Quat) -> EditableMesh {
    let mut profile = extrusion.profile.clone();
    if !profile.is_ccw() {
        profile = profile.reversed();
    }

    let pts = profile.tessellate(ARC_SEGMENTS_PER_CIRCLE);
    let n = pts.len();
    let half_h = extrusion.height * 0.5;

    // Vertices: bottom ring [0..n), top ring [n..2n)
    let vertices: Vec<Vec3> = pts
        .iter()
        .map(|p| extrusion.centre + rotation * Vec3::new(p.x, -half_h, p.y))
        .chain(
            pts.iter()
                .map(|p| extrusion.centre + rotation * Vec3::new(p.x, half_h, p.y)),
        )
        .collect();

    // Faces: top cap, bottom cap, then N side quads.
    // Winding reversed from the naive order to match the to_3d handedness flip
    // (same fix as the Bevy render mesh).
    // Top cap: vertices [n..2n) in REVERSE order → normal points outward (+Y local)
    let top_face: Vec<u32> = (n as u32..2 * n as u32).rev().collect();
    // Bottom cap: vertices [0..n) in order → normal points outward (-Y local)
    let bottom_face: Vec<u32> = (0..n as u32).collect();
    // Side quads: reversed winding [i, i+n, j+n, j] instead of [i, j, j+n, i+n]
    let side_faces: Vec<Vec<u32>> = (0..n)
        .map(|i| {
            let j = (i + 1) % n;
            vec![i as u32, (i + n) as u32, (j + n) as u32, j as u32]
        })
        .collect();

    let mut all_faces = vec![top_face, bottom_face];
    all_faces.extend(side_faces);

    editable_mesh::build_mesh_from_polygons(&vertices, &all_faces)
}

// ---------------------------------------------------------------------------
// ProfileSweep — a Profile2d swept along a 3D path
// ---------------------------------------------------------------------------

/// A solid formed by sweeping a 2D profile along a 3D path.
///
/// The profile is oriented perpendicular to the path tangent at each path point,
/// using a parallel transport frame to avoid twisting.
///
/// Face ID layout:
///   0 = start cap (closed end at path start)
///   1 = end cap (closed end at path end)
///   2..2+N*(M-1) = side quads, where N = profile vertex count, M = path point count
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSweep {
    /// World-space offset applied to the entire sweep.
    pub centre: Vec3,
    /// The 2D cross-section profile.
    pub profile: Profile2d,
    /// 3D path points in local space. Must have at least 2 points.
    pub path: Vec<Vec3>,
}

impl ProfileSweep {
    /// Compute a parallel transport frame along the path.
    ///
    /// Returns (tangent, normal, binormal) at each path point.
    /// Uses the rotation-minimizing frame technique.
    fn compute_frames(&self) -> Vec<(Vec3, Vec3, Vec3)> {
        let path = &self.path;
        let n = path.len();
        if n < 2 {
            return vec![];
        }

        let mut frames = Vec::with_capacity(n);

        // First tangent
        let t0 = (path[1] - path[0]).normalize_or_zero();
        // Choose an initial normal perpendicular to t0
        let n0 = initial_normal(t0);
        let b0 = t0.cross(n0).normalize_or_zero();
        frames.push((t0, n0, b0));

        // Propagate using rotation-minimizing frames (double reflection method)
        for i in 1..n {
            let tangent = if i < n - 1 {
                (path[i + 1] - path[i - 1]).normalize_or_zero()
            } else {
                (path[i] - path[i - 1]).normalize_or_zero()
            };

            let (_, prev_n, prev_b) = frames[i - 1];
            let prev_t = frames[i - 1].0;

            // Reflect the previous frame across the bisecting plane
            let v1 = path[i] - path[i - 1];
            let c1 = v1.dot(v1);
            if c1 < 1e-10 {
                frames.push((tangent, prev_n, prev_b));
                continue;
            }

            let r_n = prev_n - (2.0 / c1) * v1.dot(prev_n) * v1;
            let r_t = prev_t - (2.0 / c1) * v1.dot(prev_t) * v1;

            // Second reflection to align with the actual tangent
            let v2 = tangent - r_t;
            let c2 = v2.dot(v2);
            let normal = if c2 < 1e-10 {
                r_n
            } else {
                r_n - (2.0 / c2) * v2.dot(r_n) * v2
            };

            let binormal = tangent.cross(normal).normalize_or_zero();
            let normal = binormal.cross(tangent).normalize_or_zero();

            frames.push((tangent, normal, binormal));
        }

        frames
    }

    /// Generate swept vertices: profile placed at each path point.
    fn swept_vertices(&self) -> (Vec<Vec3>, usize, usize) {
        let mut profile = self.profile.clone();
        if !profile.is_ccw() {
            profile = profile.reversed();
        }

        let pts = profile.tessellate(ARC_SEGMENTS_PER_CIRCLE);
        let frames = self.compute_frames();
        let n_profile = pts.len();
        let n_path = self.path.len();

        let mut vertices = Vec::with_capacity(n_profile * n_path);

        for (i, frame) in frames.iter().enumerate() {
            let (_, normal, binormal) = frame;
            let path_point = self.path[i];

            for p in &pts {
                // Profile X → normal direction, Profile Y → binormal direction
                let world_pos = self.centre + path_point + *normal * p.x + *binormal * p.y;
                vertices.push(world_pos);
            }
        }

        (vertices, n_profile, n_path)
    }
}

/// Choose an initial normal perpendicular to a tangent vector.
fn initial_normal(tangent: Vec3) -> Vec3 {
    // Pick the axis least aligned with tangent
    let abs_t = tangent.abs();
    let seed = if abs_t.x <= abs_t.y && abs_t.x <= abs_t.z {
        Vec3::X
    } else if abs_t.y <= abs_t.z {
        Vec3::Y
    } else {
        Vec3::Z
    };
    tangent.cross(seed).normalize_or_zero()
}

impl Primitive for ProfileSweep {
    const TYPE_NAME: &'static str = "profile_sweep";

    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            centre: self.centre + delta,
            profile: self.profile.clone(),
            path: self.path.clone(),
        }
    }

    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat) {
        (
            Self {
                centre: rotation * self.centre,
                profile: self.profile.clone(),
                path: self.path.iter().map(|p| rotation * *p).collect(),
            },
            rotation * current_rotation,
        )
    }

    fn scaled(&self, factor: Vec3, center: Vec3) -> Self {
        let profile_scale = Vec2::new(factor.x.abs(), factor.y.abs());
        Self {
            centre: scale_point_around_center(self.centre, center, factor),
            profile: self.profile.scaled(profile_scale, Vec2::ZERO),
            path: self.path.iter().map(|p| *p * factor).collect(),
        }
    }

    fn shape_eq(&self, other: &Self) -> bool {
        self.profile == other.profile && self.path == other.path
    }

    fn entity_transform(&self, rotation: Quat) -> Transform {
        Transform::from_translation(self.centre).with_rotation(rotation)
    }

    fn bounds(&self, rotation: Quat) -> Option<EntityBounds> {
        let (vertices, _, _) = self.swept_vertices();
        if vertices.is_empty() {
            return None;
        }
        let world_verts: Vec<Vec3> = vertices
            .iter()
            .map(|v| rotation * (*v - self.centre) + self.centre)
            .collect();
        Some(bounds_from_points(&world_verts))
    }

    fn draw_wireframe(&self, gizmos: &mut Gizmos, rotation: Quat, color: Color) {
        let (vertices, n_profile, n_path) = self.swept_vertices();
        if vertices.is_empty() {
            return;
        }

        let to_world = |v: Vec3| -> Vec3 { rotation * (v - self.centre) + self.centre };

        // Draw profile outlines at each path station
        for station in 0..n_path {
            let base = station * n_profile;
            for i in 0..n_profile {
                let j = (i + 1) % n_profile;
                gizmos.line(
                    to_world(vertices[base + i]),
                    to_world(vertices[base + j]),
                    color,
                );
            }
        }

        // Draw longitudinal edges connecting stations
        for i in 0..n_profile {
            for station in 0..n_path - 1 {
                gizmos.line(
                    to_world(vertices[station * n_profile + i]),
                    to_world(vertices[(station + 1) * n_profile + i]),
                    color,
                );
            }
        }
    }

    fn wireframe_line_count(&self) -> usize {
        let n_profile = self.profile.tessellate(ARC_SEGMENTS_PER_CIRCLE).len();
        let n_path = self.path.len();
        n_profile * n_path + n_profile * (n_path - 1)
    }

    fn push_pull(
        &self,
        _face_id: FaceId,
        _distance: f32,
        _rotation: Quat,
        _element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>> {
        // Push/pull is not naturally defined for sweeps.
        None
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
                "path_length",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.path_length())),
            ),
        ]
    }

    fn set_property(&self, name: &str, value: &Value) -> Result<Self, String> {
        let mut prim = self.clone();
        match name {
            "centre" | "center" => prim.centre = vec3_from_json(value)?,
            "profile" => {
                prim.profile = serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid profile: {e}"))?;
            }
            "path" => {
                prim.path = serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid path: {e}"))?;
            }
            _ => {
                return Err(invalid_property_error(
                    "profile_sweep",
                    &["center", "profile", "path"],
                ))
            }
        }
        Ok(prim)
    }

    fn handles(&self, _rotation: Quat) -> Vec<HandleInfo> {
        let mut handles = vec![HandleInfo {
            id: "centre".to_string(),
            position: self.centre,
            kind: HandleKind::Center,
            label: "Centre".to_string(),
        }];
        // Path point handles
        for (i, p) in self.path.iter().enumerate() {
            handles.push(HandleInfo {
                id: format!("path_{i}"),
                position: self.centre + *p,
                kind: HandleKind::Parameter,
                label: format!("Path point {}", i + 1),
            });
        }
        handles
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3, _rotation: Quat) -> Option<Self> {
        if let Some(idx_str) = handle_id.strip_prefix("path_") {
            let idx: usize = idx_str.parse().ok()?;
            if idx < self.path.len() {
                let mut new = self.clone();
                new.path[idx] = cursor - self.centre;
                return Some(new);
            }
        }
        None
    }

    fn label(&self) -> String {
        format!("Profile sweep ({} path points)", self.path.len(),)
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(value.clone()).map_err(|e| e.to_string())
    }

    fn to_editable_mesh(&self, _rotation: Quat) -> Option<EditableMesh> {
        Some(build_sweep_editable_mesh(self))
    }
}

impl ProfileSweep {
    fn path_length(&self) -> f32 {
        self.path.windows(2).map(|w| (w[1] - w[0]).length()).sum()
    }
}

impl MeshGenerator for ProfileSweep {
    fn to_bevy_mesh(&self, _rotation: Quat) -> Mesh {
        build_sweep_bevy_mesh(self)
    }
}

// ---------------------------------------------------------------------------
// Sweep mesh generation
// ---------------------------------------------------------------------------

fn build_sweep_bevy_mesh(sweep: &ProfileSweep) -> Mesh {
    let (vertices, n_profile, n_path) = sweep.swept_vertices();
    if n_profile < 3 || n_path < 2 {
        return Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
    }

    // Vertices are in local+centre space; subtract centre for local mesh space
    let local_verts: Vec<[f32; 3]> = vertices
        .iter()
        .map(|v| {
            let local = *v - sweep.centre;
            [local.x, local.y, local.z]
        })
        .collect();

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // --- Start cap (face 0) ---
    let cap_base = positions.len() as u32;
    let cap_normal = sweep
        .compute_frames()
        .first()
        .map(|(t, _, _)| -*t)
        .unwrap_or(-Vec3::Z);
    for i in 0..n_profile {
        positions.push(local_verts[i]);
        normals.push([cap_normal.x, cap_normal.y, cap_normal.z]);
    }
    // Fan triangulation (reversed winding for start cap facing backward)
    for i in 1..n_profile as u32 - 1 {
        indices.push(cap_base);
        indices.push(cap_base + i + 1);
        indices.push(cap_base + i);
    }

    // --- End cap (face 1) ---
    let end_cap_base = positions.len() as u32;
    let end_offset = (n_path - 1) * n_profile;
    let cap_normal = sweep
        .compute_frames()
        .last()
        .map(|(t, _, _)| *t)
        .unwrap_or(Vec3::Z);
    for i in 0..n_profile {
        positions.push(local_verts[end_offset + i]);
        normals.push([cap_normal.x, cap_normal.y, cap_normal.z]);
    }
    for i in 1..n_profile as u32 - 1 {
        indices.push(end_cap_base);
        indices.push(end_cap_base + i);
        indices.push(end_cap_base + i + 1);
    }

    // --- Side quads ---
    for station in 0..n_path - 1 {
        for i in 0..n_profile {
            let j = (i + 1) % n_profile;

            let v00 = &local_verts[station * n_profile + i];
            let v01 = &local_verts[station * n_profile + j];
            let v10 = &local_verts[(station + 1) * n_profile + i];
            let v11 = &local_verts[(station + 1) * n_profile + j];

            // Compute face normal from cross product
            let p0 = Vec3::from_array(*v00);
            let p1 = Vec3::from_array(*v01);
            let p2 = Vec3::from_array(*v10);
            let normal = (p1 - p0).cross(p2 - p0).normalize_or_zero();
            let n_arr = [normal.x, normal.y, normal.z];

            let base = positions.len() as u32;
            positions.push(*v00);
            positions.push(*v01);
            positions.push(*v11);
            positions.push(*v10);
            normals.push(n_arr);
            normals.push(n_arr);
            normals.push(n_arr);
            normals.push(n_arr);

            indices.push(base);
            indices.push(base + 1);
            indices.push(base + 2);
            indices.push(base);
            indices.push(base + 2);
            indices.push(base + 3);
        }
    }

    let uv_count = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; uv_count]);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn build_sweep_editable_mesh(sweep: &ProfileSweep) -> EditableMesh {
    let (vertices, n_profile, n_path) = sweep.swept_vertices();
    if n_profile < 3 || n_path < 2 {
        return EditableMesh {
            vertices: vec![],
            half_edges: vec![],
            faces: vec![],
        };
    }

    // Start cap: first ring, reversed
    let start_cap: Vec<u32> = (0..n_profile as u32).rev().collect();
    // End cap: last ring, in order
    let end_offset = (n_path - 1) * n_profile;
    let end_cap: Vec<u32> = (end_offset as u32..(end_offset + n_profile) as u32).collect();

    let mut all_faces = vec![start_cap, end_cap];

    // Side quads
    for station in 0..n_path - 1 {
        for i in 0..n_profile {
            let j = (i + 1) % n_profile;
            let v00 = (station * n_profile + i) as u32;
            let v01 = (station * n_profile + j) as u32;
            let v10 = ((station + 1) * n_profile + i) as u32;
            let v11 = ((station + 1) * n_profile + j) as u32;
            all_faces.push(vec![v00, v01, v11, v10]);
        }
    }

    editable_mesh::build_mesh_from_polygons(&vertices, &all_faces)
}

// ---------------------------------------------------------------------------
// ProfileRevolve — a Profile2d revolved around an axis
// ---------------------------------------------------------------------------

/// A solid formed by revolving a 2D profile around a local axis.
///
/// The profile lies in the local XY plane. The revolution axis passes through
/// `axis_origin` in the direction `axis_direction` (in the profile's 2D plane
/// extended to 3D: profile X → local X, profile Y → local Y).
///
/// `angle` is the revolution angle in radians. A full revolution (TAU) creates
/// a closed solid; partial revolutions create an open solid with start and end caps.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileRevolve {
    /// World-space centre of the solid.
    pub centre: Vec3,
    /// The 2D profile to revolve.
    pub profile: Profile2d,
    /// Revolution angle in radians (TAU = full revolution).
    pub angle: f32,
    /// Number of angular steps for tessellation.
    pub segments: u32,
}

impl ProfileRevolve {
    fn is_full_revolution(&self) -> bool {
        (self.angle.abs() - std::f32::consts::TAU).abs() < 1e-4
    }

    /// Generate revolved vertices.
    ///
    /// Returns (vertices, n_profile, n_steps) where n_steps includes both
    /// start and end stations.
    fn revolved_vertices(&self) -> (Vec<Vec3>, usize, usize) {
        let mut profile = self.profile.clone();
        if !profile.is_ccw() {
            profile = profile.reversed();
        }

        let pts = profile.tessellate(ARC_SEGMENTS_PER_CIRCLE);
        let n_profile = pts.len();
        let full = self.is_full_revolution();
        let n_steps = if full {
            self.segments as usize
        } else {
            self.segments as usize + 1
        };

        let mut vertices = Vec::with_capacity(n_profile * n_steps);

        for step in 0..n_steps {
            let t = step as f32 / self.segments as f32;
            let theta = self.angle * t;
            let cos_t = theta.cos();
            let sin_t = theta.sin();

            for p in &pts {
                // Profile X → radial distance from Y axis
                // Profile Y → height along Y axis
                // Revolve around Y axis: x' = x*cos(θ), z' = x*sin(θ), y' = y
                let world_pos = self.centre + Vec3::new(p.x * cos_t, p.y, p.x * sin_t);
                vertices.push(world_pos);
            }
        }

        (vertices, n_profile, n_steps)
    }
}

impl Primitive for ProfileRevolve {
    const TYPE_NAME: &'static str = "profile_revolve";

    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            centre: self.centre + delta,
            ..self.clone()
        }
    }

    fn rotated(&self, rotation: Quat, current_rotation: Quat) -> (Self, Quat) {
        (
            Self {
                centre: rotation * self.centre,
                ..self.clone()
            },
            rotation * current_rotation,
        )
    }

    fn scaled(&self, factor: Vec3, center: Vec3) -> Self {
        let profile_scale = Vec2::new(factor.x.abs(), factor.y.abs());
        Self {
            centre: scale_point_around_center(self.centre, center, factor),
            profile: self.profile.scaled(profile_scale, Vec2::ZERO),
            ..self.clone()
        }
    }

    fn shape_eq(&self, other: &Self) -> bool {
        self.profile == other.profile
            && self.angle == other.angle
            && self.segments == other.segments
    }

    fn entity_transform(&self, rotation: Quat) -> Transform {
        Transform::from_translation(self.centre).with_rotation(rotation)
    }

    fn bounds(&self, rotation: Quat) -> Option<EntityBounds> {
        let (vertices, _, _) = self.revolved_vertices();
        if vertices.is_empty() {
            return None;
        }
        let world_verts: Vec<Vec3> = vertices
            .iter()
            .map(|v| rotation * (*v - self.centre) + self.centre)
            .collect();
        Some(bounds_from_points(&world_verts))
    }

    fn draw_wireframe(&self, gizmos: &mut Gizmos, rotation: Quat, color: Color) {
        let (vertices, n_profile, n_steps) = self.revolved_vertices();
        if vertices.is_empty() {
            return;
        }

        let to_world = |v: Vec3| -> Vec3 { rotation * (v - self.centre) + self.centre };
        let full = self.is_full_revolution();

        // Profile outlines at each station
        for station in 0..n_steps {
            let base = station * n_profile;
            for i in 0..n_profile {
                let j = (i + 1) % n_profile;
                gizmos.line(
                    to_world(vertices[base + i]),
                    to_world(vertices[base + j]),
                    color,
                );
            }
        }

        // Longitudinal edges
        let connect_steps = if full { n_steps } else { n_steps - 1 };
        for i in 0..n_profile {
            for station in 0..connect_steps {
                let next = (station + 1) % n_steps;
                gizmos.line(
                    to_world(vertices[station * n_profile + i]),
                    to_world(vertices[next * n_profile + i]),
                    color,
                );
            }
        }
    }

    fn wireframe_line_count(&self) -> usize {
        let n_profile = self.profile.tessellate(ARC_SEGMENTS_PER_CIRCLE).len();
        let full = self.is_full_revolution();
        let n_steps = if full {
            self.segments as usize
        } else {
            self.segments as usize + 1
        };
        let connect = if full { n_steps } else { n_steps - 1 };
        n_profile * n_steps + n_profile * connect
    }

    fn push_pull(
        &self,
        _face_id: FaceId,
        _distance: f32,
        _rotation: Quat,
        _element_id: crate::plugins::identity::ElementId,
    ) -> Option<PrimitivePushPullResult<Self>> {
        None
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
                "angle",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.angle.to_degrees())),
            ),
            property_field(
                "segments",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.segments as f32)),
            ),
        ]
    }

    fn set_property(&self, name: &str, value: &Value) -> Result<Self, String> {
        let mut prim = self.clone();
        match name {
            "centre" | "center" => prim.centre = vec3_from_json(value)?,
            "angle" => prim.angle = scalar_from_json(value)?.to_radians(),
            "segments" => prim.segments = scalar_from_json(value)? as u32,
            "profile" => {
                prim.profile = serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid profile: {e}"))?;
            }
            _ => {
                return Err(invalid_property_error(
                    "profile_revolve",
                    &["center", "angle", "segments", "profile"],
                ))
            }
        }
        Ok(prim)
    }

    fn handles(&self, _rotation: Quat) -> Vec<HandleInfo> {
        vec![HandleInfo {
            id: "centre".to_string(),
            position: self.centre,
            kind: HandleKind::Center,
            label: "Centre".to_string(),
        }]
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3, _rotation: Quat) -> Option<Self> {
        None
    }

    fn label(&self) -> String {
        format!(
            "Profile revolve ({:.0}°, {} segments)",
            self.angle.to_degrees(),
            self.segments,
        )
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(value.clone()).map_err(|e| e.to_string())
    }

    fn to_editable_mesh(&self, _rotation: Quat) -> Option<EditableMesh> {
        Some(build_revolve_editable_mesh(self))
    }
}

impl MeshGenerator for ProfileRevolve {
    fn to_bevy_mesh(&self, _rotation: Quat) -> Mesh {
        build_revolve_bevy_mesh(self)
    }
}

// ---------------------------------------------------------------------------
// Revolve mesh generation
// ---------------------------------------------------------------------------

fn build_revolve_bevy_mesh(revolve: &ProfileRevolve) -> Mesh {
    let (vertices, n_profile, n_steps) = revolve.revolved_vertices();
    if n_profile < 3 || n_steps < 2 {
        return Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
    }

    let full = revolve.is_full_revolution();
    let local_verts: Vec<[f32; 3]> = vertices
        .iter()
        .map(|v| {
            let local = *v - revolve.centre;
            [local.x, local.y, local.z]
        })
        .collect();

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // --- Caps (only for partial revolutions) ---
    if !full {
        // Start cap
        let cap_base = positions.len() as u32;
        for i in 0..n_profile {
            positions.push(local_verts[i]);
            normals.push([0.0, 0.0, -1.0]); // approximate
        }
        for i in 1..n_profile as u32 - 1 {
            indices.push(cap_base);
            indices.push(cap_base + i + 1);
            indices.push(cap_base + i);
        }

        // End cap
        let end_base = positions.len() as u32;
        let end_offset = (n_steps - 1) * n_profile;
        for i in 0..n_profile {
            positions.push(local_verts[end_offset + i]);
            normals.push([0.0, 0.0, 1.0]); // approximate
        }
        for i in 1..n_profile as u32 - 1 {
            indices.push(end_base);
            indices.push(end_base + i);
            indices.push(end_base + i + 1);
        }
    }

    // --- Side quads ---
    let connect_steps = if full { n_steps } else { n_steps - 1 };
    for station in 0..connect_steps {
        let next = (station + 1) % n_steps;
        for i in 0..n_profile {
            let j = (i + 1) % n_profile;

            let v00 = &local_verts[station * n_profile + i];
            let v01 = &local_verts[station * n_profile + j];
            let v10 = &local_verts[next * n_profile + i];
            let v11 = &local_verts[next * n_profile + j];

            let p0 = Vec3::from_array(*v00);
            let p1 = Vec3::from_array(*v01);
            let p2 = Vec3::from_array(*v10);
            let normal = (p1 - p0).cross(p2 - p0).normalize_or_zero();
            let n_arr = [normal.x, normal.y, normal.z];

            let base = positions.len() as u32;
            positions.push(*v00);
            positions.push(*v01);
            positions.push(*v11);
            positions.push(*v10);
            normals.push(n_arr);
            normals.push(n_arr);
            normals.push(n_arr);
            normals.push(n_arr);

            indices.push(base);
            indices.push(base + 1);
            indices.push(base + 2);
            indices.push(base);
            indices.push(base + 2);
            indices.push(base + 3);
        }
    }

    let uv_count = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; uv_count]);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn build_revolve_editable_mesh(revolve: &ProfileRevolve) -> EditableMesh {
    let (vertices, n_profile, n_steps) = revolve.revolved_vertices();
    if n_profile < 3 || n_steps < 2 {
        return EditableMesh {
            vertices: vec![],
            half_edges: vec![],
            faces: vec![],
        };
    }

    let full = revolve.is_full_revolution();
    let mut all_faces: Vec<Vec<u32>> = Vec::new();

    if !full {
        let start_cap: Vec<u32> = (0..n_profile as u32).rev().collect();
        let end_offset = (n_steps - 1) * n_profile;
        let end_cap: Vec<u32> = (end_offset as u32..(end_offset + n_profile) as u32).collect();
        all_faces.push(start_cap);
        all_faces.push(end_cap);
    }

    let connect_steps = if full { n_steps } else { n_steps - 1 };
    for station in 0..connect_steps {
        let next = (station + 1) % n_steps;
        for i in 0..n_profile {
            let j = (i + 1) % n_profile;
            all_faces.push(vec![
                (station * n_profile + i) as u32,
                (station * n_profile + j) as u32,
                (next * n_profile + j) as u32,
                (next * n_profile + i) as u32,
            ]);
        }
    }

    editable_mesh::build_mesh_from_polygons(&vertices, &all_faces)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_profile_tessellation() {
        let profile = Profile2d::rectangle(2.0, 4.0);
        let pts = profile.tessellate(32);
        assert_eq!(pts.len(), 4);
        // Should be CCW
        assert!(profile.is_ccw());
    }

    #[test]
    fn profile_json_roundtrip() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::new(1.0, 2.0, 3.0),
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        let json = extrusion.to_json();
        let restored = ProfileExtrusion::from_json(&json).unwrap();
        assert_eq!(extrusion, restored);
    }

    #[test]
    fn profile_extrusion_shape_eq() {
        let a = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        let b = ProfileExtrusion {
            centre: Vec3::new(5.0, 0.0, 0.0),
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        // Same shape, different position
        assert!(a.shape_eq(&b));

        let c = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(3.0, 4.0),
            height: 3.0,
        };
        assert!(!a.shape_eq(&c));
    }

    #[test]
    fn profile_extrusion_mesh_has_geometry() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        let mesh = build_extrusion_bevy_mesh(&extrusion);
        // A rectangular extrusion should have:
        // - 4 verts top cap + 4 verts bottom cap + 4 quads * 4 verts = 24 vertices
        let positions = mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap();
        match positions {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => {
                assert_eq!(v.len(), 24);
            }
            _ => panic!("unexpected attribute type"),
        }
    }

    #[test]
    fn l_shape_profile() {
        let profile = Profile2d::l_shape(3.0, 4.0, 1.0, 2.0);
        let pts = profile.tessellate(32);
        assert_eq!(pts.len(), 6); // 6 corners in an L-shape
        assert!(profile.is_ccw());

        // Verify it produces a valid extrusion mesh
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile,
            height: 2.8,
        };
        let mesh = build_extrusion_editable_mesh(&extrusion, Quat::IDENTITY);
        // L-shape: 6 side faces + top + bottom = 8 faces
        // (5 explicit segments + 1 implicit closing = 6 side faces)
        assert_eq!(mesh.faces.len(), 8);
    }

    #[test]
    fn arc_profile_tessellation() {
        let profile = Profile2d {
            start: Vec2::ZERO,
            segments: vec![
                ProfileSegment::LineTo {
                    to: Vec2::new(2.0, 0.0),
                },
                ProfileSegment::ArcTo {
                    to: Vec2::new(2.5, 0.5),
                    bulge: 0.4142, // ~45 degree arc (tan(45/4))
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(2.5, 2.0),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(0.0, 2.0),
                },
            ],
        };
        let pts = profile.tessellate(32);
        // Should have more than 5 points (4 line endpoints + arc subdivisions + start)
        assert!(pts.len() > 5);
    }

    #[test]
    fn json_format_is_ai_readable() {
        // Verify the JSON format is clean and AI-friendly.
        let extrusion = ProfileExtrusion {
            centre: Vec3::new(1.0, 1.5, 2.0),
            profile: Profile2d {
                start: Vec2::ZERO,
                segments: vec![
                    ProfileSegment::LineTo {
                        to: Vec2::new(3.0, 0.0),
                    },
                    ProfileSegment::ArcTo {
                        to: Vec2::new(3.5, 0.5),
                        bulge: 0.414,
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(3.5, 2.0),
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(0.0, 2.0),
                    },
                ],
            },
            height: 2.8,
        };
        let json = serde_json::to_string_pretty(&extrusion).unwrap();
        // Should contain segment type tags
        assert!(json.contains("\"seg\": \"line_to\""));
        assert!(json.contains("\"seg\": \"arc_to\""));
        assert!(json.contains("\"bulge\""));
        assert!(json.contains("\"height\": 2.8"));
    }

    #[test]
    fn push_pull_top_changes_height() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        let result = extrusion.push_pull(
            FaceId(0), // top
            1.0,
            Quat::IDENTITY,
            crate::plugins::identity::ElementId(1),
        );
        match result.unwrap() {
            PrimitivePushPullResult::SameType(new_ext, _) => {
                assert!((new_ext.height - 4.0).abs() < 1e-5);
            }
            _ => panic!("expected SameType"),
        }
    }

    #[test]
    fn push_pull_side_is_unsupported() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        assert_eq!(
            extrusion.push_pull_affordance(FaceId(2)),
            PushPullAffordance::Blocked(PushPullBlockReason::CapOnly)
        );
        // Face 2 = first side segment (bottom edge of rectangle)
        let result = extrusion.push_pull(
            FaceId(2),
            0.5,
            Quat::IDENTITY,
            crate::plugins::identity::ElementId(1),
        );
        assert!(
            result.is_none(),
            "profile extrusion side-face push/pull is intentionally unsupported"
        );
    }

    #[test]
    fn offset_segment_preserves_segment_count() {
        let profile = Profile2d::rectangle(2.0, 4.0);
        let original_count = profile.segment_count();
        let offset = profile.offset_segment(0, 0.5).unwrap();
        assert_eq!(offset.segment_count(), original_count);
    }

    #[test]
    fn face_naming_roundtrip() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        // Rectangle has 4 side segments + closing = 5 segments
        assert_eq!(extrusion.face_name(FaceId(0)), Some("top".to_string()));
        assert_eq!(extrusion.face_name(FaceId(1)), Some("bottom".to_string()));
        assert_eq!(extrusion.face_name(FaceId(2)), Some("side:0".to_string()));
        assert_eq!(extrusion.face_name(FaceId(3)), Some("side:1".to_string()));

        // Roundtrip
        assert_eq!(extrusion.face_id_from_name("top"), Some(FaceId(0)));
        assert_eq!(extrusion.face_id_from_name("bottom"), Some(FaceId(1)));
        assert_eq!(extrusion.face_id_from_name("side:0"), Some(FaceId(2)));
        assert_eq!(extrusion.face_id_from_name("side:2"), Some(FaceId(4)));

        // Out of range
        assert_eq!(extrusion.face_id_from_name("side:99"), None);
        assert_eq!(extrusion.face_id_from_name("front"), None);
    }

    #[test]
    fn generated_face_refs_are_semantic() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d {
                start: Vec2::ZERO,
                segments: vec![
                    ProfileSegment::LineTo {
                        to: Vec2::new(2.0, 0.0),
                    },
                    ProfileSegment::ArcTo {
                        to: Vec2::new(2.0, 2.0),
                        bulge: 0.4142,
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(0.0, 2.0),
                    },
                ],
            },
            height: 3.0,
        };

        assert_eq!(
            extrusion.semantic_face_ref(FaceId(0)),
            Some(GeneratedFaceRef::ProfileTop)
        );
        assert_eq!(
            extrusion.semantic_face_ref(FaceId(1)),
            Some(GeneratedFaceRef::ProfileBottom)
        );
        assert_eq!(
            extrusion.semantic_face_ref(FaceId(2)),
            Some(GeneratedFaceRef::ProfileSideSegment(0))
        );
        assert_eq!(
            extrusion.semantic_face_ref(FaceId(3)),
            Some(GeneratedFaceRef::ProfileSideArcSegment(1))
        );
        assert_eq!(
            extrusion.semantic_face_ref(FaceId(5)),
            Some(GeneratedFaceRef::ProfileSideClosingSegment)
        );
    }

    #[test]
    fn editable_mesh_from_extrusion_is_valid() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };
        let mesh = build_extrusion_editable_mesh(&extrusion, Quat::IDENTITY);
        // Should have 6 faces (top, bottom, 4 sides)
        assert_eq!(mesh.faces.len(), 6);
        // Should have 8 vertices
        assert_eq!(mesh.vertices.len(), 8);
    }

    // --- ProfileSweep tests ---

    #[test]
    fn sweep_straight_path() {
        let sweep = ProfileSweep {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(0.1, 0.05), // small cross-section
            path: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0)],
        };
        let mesh = build_sweep_bevy_mesh(&sweep);
        let positions = mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap();
        match positions {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => {
                // 4 profile verts * 2 path stations = 8 base verts
                // + 4 cap verts * 2 caps + 4 side quads * 4 verts = 32
                // caps: 4 + 4 = 8, sides: 4 * 4 * 1 segment = 16 → 24 total
                assert!(v.len() > 0, "mesh should have vertices");
            }
            _ => panic!("unexpected attribute type"),
        }
    }

    #[test]
    fn sweep_curved_path() {
        // A 90-degree curved path
        let sweep = ProfileSweep {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(0.1, 0.05),
            path: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 1.0),
                Vec3::new(2.0, 0.0, 2.0),
            ],
        };
        let mesh = build_sweep_editable_mesh(&sweep);
        // 2 caps + 4 profile edges * 3 path segments = 14 faces
        assert_eq!(mesh.faces.len(), 14);
        // 4 profile verts * 4 path points = 16 vertices
        assert_eq!(mesh.vertices.len(), 16);
    }

    #[test]
    fn sweep_json_roundtrip() {
        let sweep = ProfileSweep {
            centre: Vec3::new(1.0, 2.0, 3.0),
            profile: Profile2d::rectangle(0.1, 0.05),
            path: vec![
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, 3.0),
                Vec3::new(1.0, 0.0, 5.0),
            ],
        };
        let json = sweep.to_json();
        let restored = ProfileSweep::from_json(&json).unwrap();
        assert_eq!(sweep, restored);
    }

    #[test]
    fn sweep_shape_eq() {
        let a = ProfileSweep {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(0.1, 0.05),
            path: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0)],
        };
        let b = ProfileSweep {
            centre: Vec3::new(5.0, 0.0, 0.0), // different position
            profile: Profile2d::rectangle(0.1, 0.05),
            path: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0)],
        };
        assert!(a.shape_eq(&b));

        let c = ProfileSweep {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(0.2, 0.05), // different profile
            path: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0)],
        };
        assert!(!a.shape_eq(&c));
    }

    // --- ProfileRevolve tests ---

    #[test]
    fn revolve_full_creates_closed_solid() {
        let revolve = ProfileRevolve {
            centre: Vec3::ZERO,
            profile: Profile2d {
                start: Vec2::new(1.0, -0.5),
                segments: vec![
                    ProfileSegment::LineTo {
                        to: Vec2::new(1.5, -0.5),
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(1.5, 0.5),
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(1.0, 0.5),
                    },
                ],
            },
            angle: std::f32::consts::TAU,
            segments: 16,
        };
        let mesh = build_revolve_editable_mesh(&revolve);
        // Full revolution: no caps, just side quads
        // 4 profile edges * 16 segments = 64 faces
        assert_eq!(mesh.faces.len(), 64);
    }

    #[test]
    fn revolve_partial_has_caps() {
        let revolve = ProfileRevolve {
            centre: Vec3::ZERO,
            profile: Profile2d {
                start: Vec2::new(1.0, -0.5),
                segments: vec![
                    ProfileSegment::LineTo {
                        to: Vec2::new(1.5, -0.5),
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(1.5, 0.5),
                    },
                    ProfileSegment::LineTo {
                        to: Vec2::new(1.0, 0.5),
                    },
                ],
            },
            angle: std::f32::consts::FRAC_PI_2, // 90 degrees
            segments: 8,
        };
        let mesh = build_revolve_editable_mesh(&revolve);
        // Partial: 2 caps + 4 profile edges * 8 segments = 34 faces
        assert_eq!(mesh.faces.len(), 34);
    }

    #[test]
    fn revolve_json_roundtrip() {
        let revolve = ProfileRevolve {
            centre: Vec3::new(1.0, 2.0, 3.0),
            profile: Profile2d::rectangle(0.5, 1.0),
            angle: std::f32::consts::TAU,
            segments: 24,
        };
        let json = revolve.to_json();
        let restored = ProfileRevolve::from_json(&json).unwrap();
        assert_eq!(revolve, restored);
    }

    // --- Property editing tests ---

    #[test]
    fn set_profile_property_on_extrusion() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(2.0, 4.0),
            height: 3.0,
        };

        // Change profile to L-shape via set_property
        let l_profile = Profile2d::l_shape(3.0, 4.0, 1.0, 2.0);
        let profile_json = serde_json::to_value(&l_profile).unwrap();
        let modified = extrusion.set_property("profile", &profile_json).unwrap();

        assert_eq!(modified.profile, l_profile);
        assert_eq!(modified.height, 3.0); // height unchanged
        assert_eq!(modified.centre, Vec3::ZERO); // centre unchanged
    }

    #[test]
    fn set_path_property_on_sweep() {
        let sweep = ProfileSweep {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(0.1, 0.05),
            path: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0)],
        };

        let new_path = vec![
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, 3.0),
        ];
        let path_json = serde_json::to_value(&new_path).unwrap();
        let modified = sweep.set_property("path", &path_json).unwrap();

        assert_eq!(modified.path.len(), 3);
        assert_eq!(modified.profile, sweep.profile); // profile unchanged
    }

    // --- Coordinate system verification ---

    #[test]
    fn profile_extrusion_rotated_to_face_produces_correct_geometry() {
        // Simulate drawing a 1x1 square on the +X face of a box.
        // The drawing plane for +X face has:
        //   normal = (1,0,0), tangent = (0,0,1), bitangent = (0,1,0)
        //
        // Drawing 2D coords: (0,0), (1,0), (1,1), (0,1)
        // These should map to world coords on the +X face:
        //   tangent direction (Z) and bitangent direction (Y)

        let face_normal = Vec3::X;
        let face_tangent = Vec3::Z;
        let face_bitangent = Vec3::Y;

        let profile = Profile2d {
            start: Vec2::new(-0.5, -0.5),
            segments: vec![
                ProfileSegment::LineTo {
                    to: Vec2::new(0.5, -0.5),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(0.5, 0.5),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(-0.5, 0.5),
                },
            ],
        };

        let height = 1.0;
        let centre = Vec3::new(1.5, 0.0, 0.0); // 0.5 offset from face along normal

        // Build rotation using Mat3 from tangent frame
        let rotation = Quat::from_mat3(&Mat3::from_cols(face_tangent, face_normal, face_bitangent));

        let extrusion = ProfileExtrusion {
            centre,
            profile,
            height,
        };

        // Build the editable mesh with the rotation applied
        let mesh = extrusion.to_editable_mesh(rotation).unwrap();

        // The mesh should have 8 vertices (4 bottom ring + 4 top ring)
        assert_eq!(mesh.vertices.len(), 8);

        // All vertices should be near X >= 1.0 (on or beyond the +X face)
        for v in &mesh.vertices {
            assert!(
                v.x >= 0.9,
                "vertex {v:?} should be on the +X side (x >= 1.0)"
            );
        }

        // The Y and Z extent should be ~1.0 (from the 1x1 profile)
        let y_min = mesh
            .vertices
            .iter()
            .map(|v| v.y)
            .fold(f32::INFINITY, f32::min);
        let y_max = mesh
            .vertices
            .iter()
            .map(|v| v.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let z_min = mesh
            .vertices
            .iter()
            .map(|v| v.z)
            .fold(f32::INFINITY, f32::min);
        let z_max = mesh
            .vertices
            .iter()
            .map(|v| v.z)
            .fold(f32::NEG_INFINITY, f32::max);

        let y_extent = y_max - y_min;
        let z_extent = z_max - z_min;
        assert!(
            (y_extent - 1.0).abs() < 0.1,
            "Y extent should be ~1.0 (profile size), got {y_extent}"
        );
        assert!(
            (z_extent - 1.0).abs() < 0.1,
            "Z extent should be ~1.0 (profile size), got {z_extent}"
        );

        // The X extent should be ~1.0 (extrusion height along normal)
        let x_min = mesh
            .vertices
            .iter()
            .map(|v| v.x)
            .fold(f32::INFINITY, f32::min);
        let x_max = mesh
            .vertices
            .iter()
            .map(|v| v.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let x_extent = x_max - x_min;
        assert!(
            (x_extent - 1.0).abs() < 0.1,
            "X extent should be ~1.0 (extrusion height), got {x_extent}"
        );
    }

    #[test]
    fn profile_on_ground_plane_is_identity_rotation() {
        // Ground plane: normal=Y, tangent=X, bitangent=Z
        let rotation = Quat::from_mat3(&Mat3::from_cols(
            Vec3::X, // tangent
            Vec3::Y, // normal
            Vec3::Z, // bitangent
        ));
        // Should be (approximately) identity
        let angle = rotation.angle_between(Quat::IDENTITY);
        assert!(
            angle < 0.01,
            "Ground plane rotation should be identity, got angle {angle}"
        );
    }

    // --- Cap triangulation tests ---

    #[test]
    fn concave_l_shape_mesh_has_no_degenerate_triangles() {
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::l_shape(3.0, 4.0, 1.0, 2.0),
            height: 2.0,
        };
        let mesh = build_extrusion_bevy_mesh(&extrusion);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("unexpected attribute type"),
        };
        let idx = match mesh.indices().unwrap() {
            bevy::mesh::Indices::U32(v) => v.clone(),
            _ => panic!("unexpected index type"),
        };

        // Verify all triangles have non-zero area
        assert!(idx.len() >= 3, "mesh should have triangles");
        for tri in idx.chunks(3) {
            let a = Vec3::from_array(positions[tri[0] as usize]);
            let b = Vec3::from_array(positions[tri[1] as usize]);
            let c = Vec3::from_array(positions[tri[2] as usize]);
            let area = (b - a).cross(c - a).length() * 0.5;
            assert!(
                area > 1e-6,
                "degenerate triangle: {a:?}, {b:?}, {c:?} area={area}"
            );
        }
    }

    #[test]
    fn irregular_concave_polygon_mesh_is_valid() {
        // A concave pentagon (like what a user might draw on a face)
        let profile = Profile2d {
            start: Vec2::new(0.0, 0.0),
            segments: vec![
                ProfileSegment::LineTo {
                    to: Vec2::new(2.0, 0.0),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(2.0, 2.0),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(1.0, 1.0),
                }, // concavity
                ProfileSegment::LineTo {
                    to: Vec2::new(0.0, 2.0),
                },
            ],
        };
        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile,
            height: 1.0,
        };
        let mesh = build_extrusion_bevy_mesh(&extrusion);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("unexpected"),
        };
        let idx = match mesh.indices().unwrap() {
            bevy::mesh::Indices::U32(v) => v.clone(),
            _ => panic!("unexpected"),
        };

        // Should have geometry
        assert!(positions.len() > 0);
        assert!(
            idx.len() >= 9,
            "need at least 3 triangles, got {}",
            idx.len() / 3
        );

        // All triangles should have non-zero area
        for tri in idx.chunks(3) {
            let a = Vec3::from_array(positions[tri[0] as usize]);
            let b = Vec3::from_array(positions[tri[1] as usize]);
            let c = Vec3::from_array(positions[tri[2] as usize]);
            let area = (b - a).cross(c - a).length() * 0.5;
            assert!(area > 1e-6, "degenerate triangle at indices {:?}", tri);
        }
    }

    // --- End-to-end face drawing simulation ---

    /// Simulate exactly what happens when a user draws a polyline on the
    /// +X face of a box: project points to 2D, build profile, create extrusion,
    /// generate mesh, and verify every triangle has consistent winding.
    #[test]
    fn simulate_drawing_on_plus_x_face() {
        use crate::plugins::cursor::DrawingPlane;

        // Set up the drawing plane for the +X face of a box
        let face_centroid = Vec3::new(1.0, 0.0, 0.0);
        let face_normal = Vec3::X;
        let plane = DrawingPlane::from_face(face_centroid, face_normal);

        // Simulate 5 clicks on the face forming a pentagon-like shape.
        // World-space points on the +X plane (x=1.0):
        let world_points = vec![
            Vec3::new(1.0, -0.3, -0.5),
            Vec3::new(1.0, -0.3, 0.5),
            Vec3::new(1.0, 0.5, 0.3),
            Vec3::new(1.0, 0.6, 0.0),
            Vec3::new(1.0, 0.5, -0.3),
        ];

        // Project to 2D (same as DrawingPlane::project_to_2d)
        let points_2d: Vec<Vec2> = world_points
            .iter()
            .map(|p| plane.project_to_2d(*p))
            .collect();

        // Build profile (same as finish_face_drawing / polyline close)
        let start = points_2d[0];
        let segments: Vec<ProfileSegment> = points_2d[1..]
            .iter()
            .map(|&to| ProfileSegment::LineTo { to })
            .collect();
        let profile = Profile2d { start, segments };

        // Centre the profile
        let (pmin, pmax) = profile.bounds_2d();
        let mid_2d = (pmin + pmax) * 0.5;
        let centred_profile = profile.translated(-mid_2d);

        // Build rotation from tangent frame (same as finish_face_drawing)
        let rotation = Quat::from_mat3(&Mat3::from_cols(
            plane.tangent,
            plane.normal,
            plane.bitangent,
        ));

        let height = 1.0;
        let centre_on_plane = plane.to_world(mid_2d);
        let centre = centre_on_plane + plane.normal * (height * 0.5);

        let extrusion = ProfileExtrusion {
            centre,
            profile: centred_profile,
            height,
        };

        // Generate the mesh
        let mesh = build_extrusion_bevy_mesh(&extrusion);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("unexpected"),
        };
        let stored_normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("unexpected"),
        };
        let idx = match mesh.indices().unwrap() {
            bevy::mesh::Indices::U32(v) => v.clone(),
            _ => panic!("unexpected"),
        };

        assert!(idx.len() >= 9, "need triangles, got {}", idx.len() / 3);

        // Separate cap triangles from side triangles.
        // Cap vertices have y = ±half_h.
        let half_h = height * 0.5;
        let mut top_normals_consistent = true;
        let mut bottom_normals_consistent = true;
        let mut side_normals_consistent = true;

        for tri in idx.chunks(3) {
            let a = Vec3::from_array(positions[tri[0] as usize]);
            let b = Vec3::from_array(positions[tri[1] as usize]);
            let c = Vec3::from_array(positions[tri[2] as usize]);
            let normal = (b - a).cross(c - a);
            let area = normal.length() * 0.5;

            assert!(area > 1e-6, "degenerate triangle: {a:?}, {b:?}, {c:?}");

            // Check if this is a cap triangle (all Y coords same)
            let avg_y = (a.y + b.y + c.y) / 3.0;
            if (avg_y - half_h).abs() < 0.01 {
                // Top cap — normal should point +Y (local)
                if normal.y < 0.0 {
                    top_normals_consistent = false;
                    eprintln!(
                        "TOP CAP triangle has wrong normal: {normal:?} for {a:?}, {b:?}, {c:?}"
                    );
                }
            } else if (avg_y + half_h).abs() < 0.01 {
                // Bottom cap — normal should point -Y (local)
                if normal.y > 0.0 {
                    bottom_normals_consistent = false;
                    eprintln!(
                        "BOTTOM CAP triangle has wrong normal: {normal:?} for {a:?}, {b:?}, {c:?}"
                    );
                }
            } else {
                // Side face — normal should point OUTWARD (away from centroid in XZ plane)
                let tri_center = (a + b + c) / 3.0;
                let outward_xz = Vec3::new(tri_center.x, 0.0, tri_center.z).normalize_or_zero();
                let normal_xz = Vec3::new(normal.x, 0.0, normal.z);
                if outward_xz.length_squared() > 0.01 && normal_xz.dot(outward_xz) < 0.0 {
                    eprintln!("SIDE triangle has INWARD normal: normal_xz={normal_xz:?} outward={outward_xz:?} for {a:?}, {b:?}, {c:?}");
                    side_normals_consistent = false;
                }
            }
        }

        assert!(
            top_normals_consistent,
            "top cap has triangles with wrong winding"
        );
        assert!(
            bottom_normals_consistent,
            "bottom cap has triangles with wrong winding"
        );
        assert!(
            side_normals_consistent,
            "side faces have triangles with inward normals"
        );

        // Verify stored normals match geometric normals (for correct shading).
        for tri in idx.chunks(3) {
            let a = Vec3::from_array(positions[tri[0] as usize]);
            let b = Vec3::from_array(positions[tri[1] as usize]);
            let c = Vec3::from_array(positions[tri[2] as usize]);
            let geometric_normal = (b - a).cross(c - a).normalize_or_zero();
            let stored = Vec3::from_array(stored_normals[tri[0] as usize]);
            let dot = geometric_normal.dot(stored);
            assert!(
                dot > 0.0,
                "stored normal disagrees with geometric normal: stored={stored:?} geometric={geometric_normal:?} for triangle at {a:?}, {b:?}, {c:?}"
            );
        }

        // Now apply rotation and verify world-space bounds make sense.
        let world_positions: Vec<Vec3> = positions
            .iter()
            .map(|p| centre + rotation * Vec3::from_array(*p))
            .collect();

        // All world X coords should be >= face_centroid.x (extruding outward from +X face)
        let min_x = world_positions
            .iter()
            .map(|v| v.x)
            .fold(f32::INFINITY, f32::min);
        let max_x = world_positions
            .iter()
            .map(|v| v.x)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            min_x >= 0.5,
            "min X should be near the face (>=0.5), got {min_x}"
        );
        assert!(
            (max_x - min_x - height).abs() < 0.2,
            "X extent should be ~{height}, got {}",
            max_x - min_x
        );
    }

    /// Verify that a profile drawn CW (which `is_ccw` detects) gets reversed
    /// properly and still produces correct mesh.
    #[test]
    fn clockwise_drawn_profile_still_produces_valid_mesh() {
        // CW square (negative signed area)
        let profile = Profile2d {
            start: Vec2::new(0.0, 0.0),
            segments: vec![
                ProfileSegment::LineTo {
                    to: Vec2::new(0.0, 1.0),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(1.0, 1.0),
                },
                ProfileSegment::LineTo {
                    to: Vec2::new(1.0, 0.0),
                },
            ],
        };
        assert!(!profile.is_ccw(), "this profile should be CW");

        let extrusion = ProfileExtrusion {
            centre: Vec3::ZERO,
            profile,
            height: 1.0,
        };

        let mesh = build_extrusion_bevy_mesh(&extrusion);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("unexpected"),
        };
        let idx = match mesh.indices().unwrap() {
            bevy::mesh::Indices::U32(v) => v.clone(),
            _ => panic!("unexpected"),
        };

        assert!(idx.len() >= 9);

        // All cap triangles should have correct winding
        let half_h = 0.5;
        for tri in idx.chunks(3) {
            let a = Vec3::from_array(positions[tri[0] as usize]);
            let b = Vec3::from_array(positions[tri[1] as usize]);
            let c = Vec3::from_array(positions[tri[2] as usize]);
            let normal = (b - a).cross(c - a);

            let avg_y = (a.y + b.y + c.y) / 3.0;
            if (avg_y - half_h).abs() < 0.01 {
                assert!(
                    normal.y > 0.0,
                    "top cap triangle has wrong winding: {normal:?}"
                );
            } else if (avg_y + half_h).abs() < 0.01 {
                assert!(
                    normal.y < 0.0,
                    "bottom cap triangle has wrong winding: {normal:?}"
                );
            }
        }
    }

    /// Verify push/pull works on a rotated ProfileExtrusion (face on +X box face).
    #[test]
    fn push_pull_on_rotated_profile_extrusion() {
        use crate::plugins::cursor::DrawingPlane;

        let face_normal = Vec3::X;
        let plane = DrawingPlane::from_face(Vec3::new(1.0, 0.0, 0.0), face_normal);
        let rotation = Quat::from_mat3(&Mat3::from_cols(
            plane.tangent,
            plane.normal,
            plane.bitangent,
        ));

        let profile = Profile2d::rectangle(0.6, 0.6);
        let height = 1.0;
        let centre = Vec3::new(1.5, 0.0, 0.0);

        let extrusion = ProfileExtrusion {
            centre,
            profile,
            height,
        };

        // Push top face (FaceId 0) by 0.5
        let result = extrusion.push_pull(
            FaceId(0),
            0.5,
            rotation,
            crate::plugins::identity::ElementId(1),
        );
        assert!(result.is_some(), "push_pull on FaceId(0) should succeed");

        match result.unwrap() {
            PrimitivePushPullResult::SameType(new_ext, _new_rot) => {
                assert!(
                    (new_ext.height - 1.5).abs() < 0.01,
                    "height should increase by 0.5, got {}",
                    new_ext.height
                );
                // Centre should shift along the face normal
                let delta = new_ext.centre - centre;
                let along_normal = delta.dot(face_normal);
                assert!(
                    along_normal > 0.0,
                    "centre should move along face normal, got delta={delta:?}"
                );
            }
            _ => panic!("expected SameType"),
        }

        // Push bottom face (FaceId 1) by 0.3
        let result = extrusion.push_pull(
            FaceId(1),
            0.3,
            rotation,
            crate::plugins::identity::ElementId(1),
        );
        assert!(result.is_some(), "push_pull on FaceId(1) should succeed");

        // Push a side face (FaceId 2)
        let result = extrusion.push_pull(
            FaceId(2),
            0.2,
            rotation,
            crate::plugins::identity::ElementId(1),
        );
        assert!(
            result.is_none(),
            "side-face push/pull should stay unsupported in the end-to-end path too"
        );
    }

    /// Verify that the editable mesh face normals match what push_pull expects.
    #[test]
    fn editable_mesh_face_normal_matches_push_pull_direction() {
        use crate::plugins::cursor::DrawingPlane;

        let face_normal = Vec3::X;
        let plane = DrawingPlane::from_face(Vec3::new(1.0, 0.0, 0.0), face_normal);
        let rotation = Quat::from_mat3(&Mat3::from_cols(
            plane.tangent,
            plane.normal,
            plane.bitangent,
        ));

        let extrusion = ProfileExtrusion {
            centre: Vec3::new(1.5, 0.0, 0.0),
            profile: Profile2d::rectangle(0.6, 0.6),
            height: 1.0,
        };

        // Get editable mesh face normal for FaceId(0) (top cap)
        let editable = extrusion.to_editable_mesh(rotation).unwrap();
        let top_normal = editable.faces[0].normal;

        // push_pull uses rotation * Vec3::Y as the push direction
        let push_dir = rotation * Vec3::Y;

        let dot = top_normal.dot(push_dir);
        assert!(
            dot > 0.9,
            "editable mesh top face normal {top_normal:?} should align with push direction {push_dir:?}, dot={dot}"
        );

        // Bottom face normal should be opposite
        let bottom_normal = editable.faces[1].normal;
        let dot_bottom = bottom_normal.dot(-push_dir);
        assert!(
            dot_bottom > 0.9,
            "editable mesh bottom face normal {bottom_normal:?} should align with -{push_dir:?}, dot={dot_bottom}"
        );
    }

    /// Full end-to-end: simulate drawing on +X face, create extrusion,
    /// then push/pull the cap face and verify the geometry changes.
    #[test]
    fn end_to_end_draw_on_face_then_push_pull() {
        use crate::plugins::cursor::DrawingPlane;

        // --- Step 1: Set up drawing plane for +X face ---
        let face_centroid = Vec3::new(1.0, 0.0, 0.0);
        let face_normal = Vec3::X;
        let plane = DrawingPlane::from_face(face_centroid, face_normal);

        // --- Step 2: Simulate drawing 4 points (square) on the face ---
        let world_points = vec![
            Vec3::new(1.0, -0.3, -0.3),
            Vec3::new(1.0, -0.3, 0.3),
            Vec3::new(1.0, 0.3, 0.3),
            Vec3::new(1.0, 0.3, -0.3),
        ];
        let points_2d: Vec<Vec2> = world_points
            .iter()
            .map(|p| plane.project_to_2d(*p))
            .collect();

        // --- Step 3: Build profile (same as finish_face_drawing) ---
        let start = points_2d[0];
        let segments: Vec<ProfileSegment> = points_2d[1..]
            .iter()
            .map(|&to| ProfileSegment::LineTo { to })
            .collect();
        let profile = Profile2d { start, segments };
        let (pmin, pmax) = profile.bounds_2d();
        let mid_2d = (pmin + pmax) * 0.5;
        let centred_profile = profile.translated(-mid_2d);

        let rotation = Quat::from_mat3(&Mat3::from_cols(
            plane.tangent,
            plane.normal,
            plane.bitangent,
        ));
        let height = 1.0;
        let centre = plane.to_world(mid_2d) + plane.normal * (height * 0.5);

        let extrusion = ProfileExtrusion {
            centre,
            profile: centred_profile,
            height,
        };

        // --- Step 4: Verify initial geometry ---
        let mesh_before = build_extrusion_bevy_mesh(&extrusion);
        let pos_before = extract_positions(&mesh_before);
        let world_before: Vec<Vec3> = pos_before
            .iter()
            .map(|p| centre + rotation * Vec3::from_array(*p))
            .collect();
        let x_max_before = world_before
            .iter()
            .map(|v| v.x)
            .fold(f32::NEG_INFINITY, f32::max);
        eprintln!("Before push/pull: height={height}, x_max={x_max_before:.3}");

        // --- Step 5: Simulate face selection ---
        // Get the editable mesh, find FaceId(0) (top cap = far cap pointing +X)
        let editable = extrusion.to_editable_mesh(rotation).unwrap();
        let top_face_normal = editable.faces[0].normal;
        eprintln!("Top face (FaceId 0) normal: {top_face_normal:?}");
        assert!(
            top_face_normal.dot(face_normal) > 0.9,
            "top face should point along +X, got {top_face_normal:?}"
        );

        // --- Step 6: Push/pull FaceId(0) by +0.5 ---
        let result = extrusion.push_pull(
            FaceId(0),
            0.5,
            rotation,
            crate::plugins::identity::ElementId(99),
        );
        assert!(result.is_some(), "push_pull on cap face should succeed");

        let pushed = match result.unwrap() {
            PrimitivePushPullResult::SameType(ext, _rot) => ext,
            _ => panic!("expected SameType"),
        };

        // --- Step 7: Verify geometry changed ---
        assert!(
            (pushed.height - 1.5).abs() < 0.01,
            "height should be 1.5 after pushing 0.5, got {}",
            pushed.height
        );

        let mesh_after = build_extrusion_bevy_mesh(&pushed);
        let pos_after = extract_positions(&mesh_after);
        let world_after: Vec<Vec3> = pos_after
            .iter()
            .map(|p| pushed.centre + rotation * Vec3::from_array(*p))
            .collect();
        let x_max_after = world_after
            .iter()
            .map(|v| v.x)
            .fold(f32::NEG_INFINITY, f32::max);
        eprintln!(
            "After push/pull: height={}, x_max={x_max_after:.3}",
            pushed.height
        );

        assert!(
            x_max_after > x_max_before + 0.4,
            "x_max should increase by ~0.5 after push, before={x_max_before:.3} after={x_max_after:.3}"
        );

        // --- Step 8: Verify the pushed geometry is still valid ---
        let idx = extract_indices(&mesh_after);
        for tri in idx.chunks(3) {
            let a = Vec3::from_array(pos_after[tri[0] as usize]);
            let b = Vec3::from_array(pos_after[tri[1] as usize]);
            let c = Vec3::from_array(pos_after[tri[2] as usize]);
            let area = (b - a).cross(c - a).length() * 0.5;
            assert!(area > 1e-6, "degenerate triangle after push");
        }

        // --- Step 9: Push/pull AGAIN (the user's reported failure) ---
        let result2 = pushed.push_pull(
            FaceId(0),
            0.3,
            rotation,
            crate::plugins::identity::ElementId(99),
        );
        assert!(result2.is_some(), "second push_pull should also succeed");

        let pushed2 = match result2.unwrap() {
            PrimitivePushPullResult::SameType(ext, _rot) => ext,
            _ => panic!("expected SameType"),
        };
        assert!(
            (pushed2.height - 1.8).abs() < 0.01,
            "height should be 1.8 after second push, got {}",
            pushed2.height
        );
        eprintln!("After second push: height={}", pushed2.height);
    }
}

#[cfg(test)]
fn extract_positions(mesh: &Mesh) -> Vec<[f32; 3]> {
    match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
        bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
        _ => panic!("unexpected"),
    }
}

#[cfg(test)]
fn extract_indices(mesh: &Mesh) -> Vec<u32> {
    match mesh.indices().unwrap() {
        bevy::mesh::Indices::U32(v) => v.clone(),
        _ => panic!("unexpected"),
    }
}
