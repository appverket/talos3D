//! Ghost geometry primitives (ADR-042 §10).
//!
//! Two first-class core primitives for modelling negative-space and
//! load-path expectations:
//!
//! - [`ClearanceEnvelope`] — a volume of declared "must stay clear"
//!   negative space (usable storage volume, headroom, an opening
//!   reserved for a window, an egress path, a service chase).
//! - [`SupportCorridor`] — a volume that bears load between two faces
//!   and must contain a continuous (or staggered) chain of structural
//!   contributors.
//!
//! Both are Bevy components carrying authored data, plus a uniform
//! AABB-based intersection / load-path validator. The validator
//! produces [`IntersectionFinding`]s that the rest of the system can
//! surface as findings, recipe postcondition failures, or promotion
//! gates.
//!
//! This is the **first viable** validator — AABB-only obstacle
//! intersection plus a contributor-chain check for support corridors.
//! Mesh-mesh intersection and full structural load-path solving are
//! deferred per the handoff scope guidance ("AABB intersection is the
//! minimum viable check"). Viewport rendering of the envelopes /
//! corridors is also deferred — recipes consume the data and the
//! validator reports findings; gizmo-line rendering can land in a
//! follow-up.
//!
//! Ghost primitives are **emitted by recipes from their owning
//! concept**, not directly authored by users (per ADR-042 §10).
//! Consequently this module ships components only — no
//! `AuthoredEntity` / `*Factory` surface. Recipes spawn a child entity
//! and insert `ClearanceEnvelope` or `SupportCorridor` as the
//! component; the validator picks them up by the standard query
//! pattern.

use std::collections::HashSet;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::authored_entity::EntityBounds;
use crate::plugins::identity::ElementId;
use crate::plugins::modeling::primitives::{
    BoxPrimitive, CylinderPrimitive, PlanePrimitive, SpherePrimitive, TriangleMesh,
};

// ---------------------------------------------------------------------------
// Geometry shapes
// ---------------------------------------------------------------------------

/// A geometric volume used by either ghost primitive.
///
/// Three shapes cover the cases the first-vertical-slice recipes
/// need:
///
/// - [`ClearanceShape::Box`] — oriented bounding box (centre + half
///   extents + rotation). The most common shape for usable / headroom
///   envelopes.
/// - [`ClearanceShape::Cylinder`] — vertical cylinder for round
///   service shafts and stairwell egress envelopes. The axis is
///   stored explicitly so non-vertical cylinders are also supported.
/// - [`ClearanceShape::Polyhedron`] — explicit triangle mesh for
///   arbitrary shapes (an L-shaped storage void, a staircase
///   envelope, a complex egress corridor).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClearanceShape {
    Box {
        center: Vec3,
        half_extents: Vec3,
        rotation: Quat,
    },
    Cylinder {
        center: Vec3,
        /// Axis along which the cylinder extends. Length-normalised
        /// internally for AABB computation.
        axis: Vec3,
        radius: f32,
        height: f32,
    },
    Polyhedron {
        vertices: Vec<Vec3>,
        /// Optional triangulation for downstream mesh-mesh tests. The
        /// AABB validator only uses `vertices`.
        faces: Vec<[u32; 3]>,
    },
}

impl ClearanceShape {
    /// Axis-aligned bounding box of this shape in world space.
    pub fn aabb(&self) -> EntityBounds {
        match self {
            Self::Box {
                center,
                half_extents,
                rotation,
            } => oriented_box_aabb(*center, *half_extents, *rotation),
            Self::Cylinder {
                center,
                axis,
                radius,
                height,
            } => cylinder_aabb(*center, *axis, *radius, *height),
            Self::Polyhedron { vertices, .. } => points_aabb(vertices),
        }
    }
}

fn oriented_box_aabb(center: Vec3, half_extents: Vec3, rotation: Quat) -> EntityBounds {
    let local = [
        Vec3::new(-half_extents.x, -half_extents.y, -half_extents.z),
        Vec3::new(half_extents.x, -half_extents.y, -half_extents.z),
        Vec3::new(half_extents.x, half_extents.y, -half_extents.z),
        Vec3::new(-half_extents.x, half_extents.y, -half_extents.z),
        Vec3::new(-half_extents.x, -half_extents.y, half_extents.z),
        Vec3::new(half_extents.x, -half_extents.y, half_extents.z),
        Vec3::new(half_extents.x, half_extents.y, half_extents.z),
        Vec3::new(-half_extents.x, half_extents.y, half_extents.z),
    ];
    let world: Vec<Vec3> = local.iter().map(|v| center + rotation * *v).collect();
    points_aabb(&world)
}

fn cylinder_aabb(center: Vec3, axis: Vec3, radius: f32, height: f32) -> EntityBounds {
    // Build an OBB enclosing the cylinder (axis · height + a square
    // cross-section of edge 2 * radius) and take its AABB. This
    // over-estimates the true cylinder AABB by at most √2, which is
    // acceptable for a ghost-geometry first cut.
    let axis = if axis.length_squared() > f32::EPSILON {
        axis.normalize()
    } else {
        Vec3::Y
    };
    let any_perp = if axis.dot(Vec3::X).abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let u = axis.cross(any_perp).normalize();
    let v = axis.cross(u).normalize();
    let half_h = height * 0.5;
    let mut points = Vec::with_capacity(8);
    for s_axis in [-half_h, half_h] {
        for s_u in [-radius, radius] {
            for s_v in [-radius, radius] {
                points.push(center + axis * s_axis + u * s_u + v * s_v);
            }
        }
    }
    points_aabb(&points)
}

fn points_aabb(points: &[Vec3]) -> EntityBounds {
    if points.is_empty() {
        return EntityBounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        };
    }
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in points {
        min = min.min(*p);
        max = max.max(*p);
    }
    EntityBounds { min, max }
}

/// Strict AABB overlap test. Touching boxes (shared face / edge /
/// vertex) are **not** considered overlapping — a clearance envelope
/// that exactly meets a wall plane is intentional, not a violation.
pub fn aabb_overlaps(a: &EntityBounds, b: &EntityBounds) -> bool {
    a.min.x < b.max.x
        && a.max.x > b.min.x
        && a.min.y < b.max.y
        && a.max.y > b.min.y
        && a.min.z < b.max.z
        && a.max.z > b.min.z
}

// ---------------------------------------------------------------------------
// Clearance envelope
// ---------------------------------------------------------------------------

/// What the clearance envelope is reserving negative space *for*.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClearanceKind {
    /// Useful but uninhabitable volume (storage, attic pocket).
    Usable,
    /// Headroom for a habitable space.
    Headroom,
    /// Reserved opening (window / door cut).
    Opening,
    /// Service chase or duct run.
    Service,
    /// Egress path (stair, exit corridor).
    Egress,
    /// Domain-defined custom kind.
    Custom(String),
}

/// Whether the envelope tolerates declared intrusions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClearanceConstraint {
    /// Nothing may intrude on the envelope.
    NoIntersect,
    /// Specific elements may intrude (the
    /// [`ClearanceEnvelope::declared_intrusions`] whitelist).
    NoIntersectExceptDeclared,
}

/// A declared volume of negative space (ADR-042 §10).
///
/// Carried as a Bevy component. Spawned by the recipe that emits the
/// owner concept (e.g. the attic-truss schematic recipe spawns a
/// `Usable` envelope at the attic's central pocket so downstream
/// recipes that place rafters / stair runs can validate they don't
/// intrude on the storage void).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClearanceEnvelope {
    /// The element this envelope belongs to (a truss occurrence, a
    /// room, a stair). Recipes typically attach the envelope as a
    /// child entity of this owner; storing the id explicitly keeps
    /// the envelope queryable without parent traversal.
    pub owner: ElementId,
    /// Concept asset path the envelope's existence is grounded in
    /// (e.g. `"roof.truss.attic.storage_void"`).
    pub owner_concept: String,
    pub kind: ClearanceKind,
    pub geometry: ClearanceShape,
    pub constraint: ClearanceConstraint,
    /// Whitelist of elements that are *allowed* to intrude. Only
    /// consulted when `constraint` is
    /// [`ClearanceConstraint::NoIntersectExceptDeclared`].
    pub declared_intrusions: Vec<ElementId>,
    /// Optional human-readable label used in findings.
    pub label: Option<String>,
}

impl ClearanceEnvelope {
    /// Construct a minimal `Usable` envelope.
    pub fn usable(owner: ElementId, owner_concept: impl Into<String>, geometry: ClearanceShape) -> Self {
        Self {
            owner,
            owner_concept: owner_concept.into(),
            kind: ClearanceKind::Usable,
            geometry,
            constraint: ClearanceConstraint::NoIntersect,
            declared_intrusions: Vec::new(),
            label: None,
        }
    }

    /// Builder: change the kind.
    pub fn with_kind(mut self, kind: ClearanceKind) -> Self {
        self.kind = kind;
        self
    }

    /// Builder: change the constraint.
    pub fn with_constraint(mut self, constraint: ClearanceConstraint) -> Self {
        self.constraint = constraint;
        self
    }

    /// Builder: replace the declared-intrusions whitelist.
    pub fn with_declared_intrusions(mut self, ids: Vec<ElementId>) -> Self {
        self.declared_intrusions = ids;
        self
    }

    /// Builder: add a label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Support corridor
// ---------------------------------------------------------------------------

/// Anchor for the start / end of a support corridor.
///
/// `face_label` is optional and free-form (e.g. `"top"`, `"bottom"`,
/// `"north_face"`, or a face id from the host's domain model). The
/// AABB validator does not consume the face label; it is preserved
/// for downstream reporting and for richer load-path solvers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FaceAnchor {
    pub element_id: ElementId,
    pub face_label: Option<String>,
}

impl FaceAnchor {
    pub fn new(element_id: ElementId) -> Self {
        Self {
            element_id,
            face_label: None,
        }
    }

    pub fn labeled(element_id: ElementId, face_label: impl Into<String>) -> Self {
        Self {
            element_id,
            face_label: Some(face_label.into()),
        }
    }
}

/// Whether the corridor's contributors must form a continuous chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoadPathRequirement {
    /// Every contributor must touch (AABB-overlap) its neighbours so
    /// the load path is unbroken from `start_face` to `end_face`.
    Continuous,
    /// Gaps between contributors are acceptable (a transfer beam
    /// will bridge them).
    StaggeredAllowed,
}

/// A volume that bears load between two faces (ADR-042 §10).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupportCorridor {
    pub owner: ElementId,
    pub owner_concept: String,
    pub start_face: FaceAnchor,
    pub end_face: FaceAnchor,
    pub geometry: ClearanceShape,
    pub required_load_path: LoadPathRequirement,
    /// Ordered list of structural contributors that bear load between
    /// `start_face` and `end_face`. Order matters for `Continuous`
    /// load paths (each contributor must touch the previous one).
    pub contributors: Vec<ElementId>,
    pub label: Option<String>,
}

impl SupportCorridor {
    pub fn new(
        owner: ElementId,
        owner_concept: impl Into<String>,
        start_face: FaceAnchor,
        end_face: FaceAnchor,
        geometry: ClearanceShape,
    ) -> Self {
        Self {
            owner,
            owner_concept: owner_concept.into(),
            start_face,
            end_face,
            geometry,
            required_load_path: LoadPathRequirement::Continuous,
            contributors: Vec::new(),
            label: None,
        }
    }

    pub fn with_contributors(mut self, contributors: Vec<ElementId>) -> Self {
        self.contributors = contributors;
        self
    }

    pub fn with_load_path(mut self, requirement: LoadPathRequirement) -> Self {
        self.required_load_path = requirement;
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// Which kind of ghost emitted a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GhostKind {
    ClearanceEnvelope,
    SupportCorridor,
}

/// Why the finding was emitted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FindingReason {
    /// The element's AABB overlaps the envelope's AABB and is not
    /// whitelisted via `declared_intrusions`.
    Intrusion,
    /// `Continuous` load path required and a gap was detected
    /// between two adjacent contributors.
    BrokenLoadPath,
    /// `Continuous` load path required but the contributor list
    /// is empty.
    MissingContributors,
}

/// A single validator finding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IntersectionFinding {
    pub ghost_kind: GhostKind,
    pub ghost_owner: ElementId,
    pub ghost_label: Option<String>,
    pub reason: FindingReason,
    /// Element that triggered the finding. For
    /// [`FindingReason::MissingContributors`] this equals the ghost
    /// owner.
    pub offending_element: ElementId,
    pub message: String,
}

/// An obstacle considered by the AABB intersection validator.
///
/// The validator is pure with respect to `Vec<GhostObstacle>` so it
/// can be unit-tested without a Bevy world.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostObstacle {
    pub element_id: ElementId,
    pub bounds: EntityBounds,
    pub label: Option<String>,
}

/// Pure AABB-overlap check against a list of obstacles. Used by the
/// envelope validator and as the building block for the corridor
/// load-path check.
fn aabb_obstacles_overlapping(
    shape_aabb: &EntityBounds,
    obstacles: &[GhostObstacle],
    skip: &HashSet<ElementId>,
) -> Vec<ElementId> {
    let mut hits = Vec::new();
    for obs in obstacles {
        if skip.contains(&obs.element_id) {
            continue;
        }
        if aabb_overlaps(shape_aabb, &obs.bounds) {
            hits.push(obs.element_id);
        }
    }
    hits
}

/// Validate a [`ClearanceEnvelope`] against an obstacle list.
///
/// Returns one [`IntersectionFinding`] per non-whitelisted obstacle
/// whose AABB overlaps the envelope's AABB. The envelope's owner is
/// always whitelisted (an envelope cannot intrude on itself).
pub fn check_clearance_envelope(
    envelope: &ClearanceEnvelope,
    obstacles: &[GhostObstacle],
) -> Vec<IntersectionFinding> {
    let mut skip: HashSet<ElementId> = HashSet::new();
    skip.insert(envelope.owner);
    if envelope.constraint == ClearanceConstraint::NoIntersectExceptDeclared {
        skip.extend(envelope.declared_intrusions.iter().copied());
    }
    let aabb = envelope.geometry.aabb();
    aabb_obstacles_overlapping(&aabb, obstacles, &skip)
        .into_iter()
        .map(|id| IntersectionFinding {
            ghost_kind: GhostKind::ClearanceEnvelope,
            ghost_owner: envelope.owner,
            ghost_label: envelope.label.clone(),
            reason: FindingReason::Intrusion,
            offending_element: id,
            message: format!(
                "element {} intrudes on clearance envelope for {}",
                id.0, envelope.owner_concept,
            ),
        })
        .collect()
}

/// Validate a [`SupportCorridor`] against an obstacle list.
///
/// For `Continuous` load paths, the contributor list must be
/// non-empty and each adjacent pair must AABB-overlap. For
/// `StaggeredAllowed`, only the non-empty check applies (the
/// validator trusts the recipe to attach a transfer member elsewhere
/// in the model).
///
/// `obstacles` is the world-side AABB index of every contributor and
/// the start / end face hosts. The validator looks up each
/// contributor by id; contributors not present in `obstacles` are
/// reported as missing.
pub fn check_support_corridor(
    corridor: &SupportCorridor,
    obstacles: &[GhostObstacle],
) -> Vec<IntersectionFinding> {
    if corridor.contributors.is_empty() {
        return vec![IntersectionFinding {
            ghost_kind: GhostKind::SupportCorridor,
            ghost_owner: corridor.owner,
            ghost_label: corridor.label.clone(),
            reason: FindingReason::MissingContributors,
            offending_element: corridor.owner,
            message: format!(
                "support corridor for {} declares no contributors",
                corridor.owner_concept,
            ),
        }];
    }
    if corridor.required_load_path == LoadPathRequirement::StaggeredAllowed {
        return Vec::new();
    }
    // Continuous: build a quick id → bounds map and walk the
    // contributor list.
    let mut findings = Vec::new();
    let mut prev_bounds: Option<&EntityBounds> = None;
    let mut prev_id: Option<ElementId> = None;
    for &id in &corridor.contributors {
        let entry = obstacles.iter().find(|o| o.element_id == id);
        let bounds = match entry {
            Some(e) => &e.bounds,
            None => {
                findings.push(IntersectionFinding {
                    ghost_kind: GhostKind::SupportCorridor,
                    ghost_owner: corridor.owner,
                    ghost_label: corridor.label.clone(),
                    reason: FindingReason::MissingContributors,
                    offending_element: id,
                    message: format!(
                        "contributor {} is missing from the obstacle index for support corridor {}",
                        id.0, corridor.owner_concept,
                    ),
                });
                prev_bounds = None;
                prev_id = None;
                continue;
            }
        };
        if let (Some(prev), Some(prev_id_v)) = (prev_bounds, prev_id) {
            if !aabb_overlaps(prev, bounds) {
                findings.push(IntersectionFinding {
                    ghost_kind: GhostKind::SupportCorridor,
                    ghost_owner: corridor.owner,
                    ghost_label: corridor.label.clone(),
                    reason: FindingReason::BrokenLoadPath,
                    offending_element: id,
                    message: format!(
                        "support corridor for {} has a gap between contributors {} and {}",
                        corridor.owner_concept, prev_id_v.0, id.0,
                    ),
                });
            }
        }
        prev_bounds = Some(bounds);
        prev_id = Some(id);
    }
    findings
}

// ---------------------------------------------------------------------------
// Bevy-side convenience helpers
// ---------------------------------------------------------------------------

/// Walk the world and emit a [`GhostObstacle`] for every entity that
/// carries one of the geometry-bearing components the validator
/// understands. This is the bridge that turns recipe-emitted geometry
/// into validator inputs.
///
/// Today this picks up: `TriangleMesh`, `BoxPrimitive`,
/// `CylinderPrimitive`, `SpherePrimitive`, `PlanePrimitive`. If new
/// authored kinds appear, extend this function.
pub fn collect_world_obstacles(world: &World) -> Vec<GhostObstacle> {
    let mut out = Vec::new();
    let mut seen: HashSet<ElementId> = HashSet::new();
    let push = |id: ElementId, bounds: EntityBounds, out: &mut Vec<GhostObstacle>, seen: &mut HashSet<ElementId>| {
        if seen.insert(id) {
            out.push(GhostObstacle {
                element_id: id,
                bounds,
                label: None,
            });
        }
    };
    // `try_query` returns `None` when no entity in the world has the
    // component type yet (the type is not registered). That is fine
    // for our purposes — no entities means no obstacles of that
    // kind, so just skip the query.
    if let Some(mut q) = world.try_query::<(&ElementId, &TriangleMesh)>() {
        for (id, mesh) in q.iter(world) {
            let bounds = points_aabb(&mesh.vertices);
            push(*id, bounds, &mut out, &mut seen);
        }
    }
    if let Some(mut q) = world.try_query::<(&ElementId, &BoxPrimitive)>() {
        for (id, p) in q.iter(world) {
            let bounds = EntityBounds {
                min: p.centre - p.half_extents,
                max: p.centre + p.half_extents,
            };
            push(*id, bounds, &mut out, &mut seen);
        }
    }
    if let Some(mut q) = world.try_query::<(&ElementId, &CylinderPrimitive)>() {
        for (id, p) in q.iter(world) {
            let half_h = p.height * 0.5;
            let r = p.radius;
            let bounds = EntityBounds {
                min: p.centre - Vec3::new(r, half_h, r),
                max: p.centre + Vec3::new(r, half_h, r),
            };
            push(*id, bounds, &mut out, &mut seen);
        }
    }
    if let Some(mut q) = world.try_query::<(&ElementId, &SpherePrimitive)>() {
        for (id, p) in q.iter(world) {
            let r = p.radius;
            let bounds = EntityBounds {
                min: p.centre - Vec3::splat(r),
                max: p.centre + Vec3::splat(r),
            };
            push(*id, bounds, &mut out, &mut seen);
        }
    }
    if let Some(mut q) = world.try_query::<(&ElementId, &PlanePrimitive)>() {
        for (id, p) in q.iter(world) {
            let min2 = p.corner_a.min(p.corner_b);
            let max2 = p.corner_a.max(p.corner_b);
            let bounds = EntityBounds {
                min: Vec3::new(min2.x, p.elevation, min2.y),
                max: Vec3::new(max2.x, p.elevation, max2.y),
            };
            push(*id, bounds, &mut out, &mut seen);
        }
    }
    out
}

/// Run [`check_clearance_envelope`] against the live world's
/// obstacle index.
pub fn check_envelope_in_world(
    world: &World,
    envelope: &ClearanceEnvelope,
) -> Vec<IntersectionFinding> {
    let obstacles = collect_world_obstacles(world);
    check_clearance_envelope(envelope, &obstacles)
}

/// Run [`check_support_corridor`] against the live world's obstacle
/// index.
pub fn check_corridor_in_world(
    world: &World,
    corridor: &SupportCorridor,
) -> Vec<IntersectionFinding> {
    let obstacles = collect_world_obstacles(world);
    check_support_corridor(corridor, &obstacles)
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin: today this is a no-op marker so apps can `add_plugins`
/// it for forward compatibility (a viewport-rendering system for
/// envelopes / corridors will live here when PP-B's rendering slice
/// lands). The components themselves are auto-registered by Bevy
/// when first queried.
pub struct GhostGeometryPlugin;

impl Plugin for GhostGeometryPlugin {
    fn build(&self, _app: &mut App) {
        // No systems yet. Validators are invoked from recipe replays
        // / postcondition oracles; rendering is deferred.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn box_shape(center: Vec3, half_extents: Vec3) -> ClearanceShape {
        ClearanceShape::Box {
            center,
            half_extents,
            rotation: Quat::IDENTITY,
        }
    }

    fn unit_envelope(owner: ElementId, center: Vec3) -> ClearanceEnvelope {
        ClearanceEnvelope::usable(
            owner,
            "test.concept",
            box_shape(center, Vec3::splat(0.5)),
        )
    }

    fn obstacle(id: u64, center: Vec3, half: Vec3) -> GhostObstacle {
        GhostObstacle {
            element_id: ElementId(id),
            bounds: EntityBounds {
                min: center - half,
                max: center + half,
            },
            label: None,
        }
    }

    #[test]
    fn box_aabb_axis_aligned_unit_box() {
        let s = box_shape(Vec3::ZERO, Vec3::splat(1.0));
        let a = s.aabb();
        assert_eq!(a.min, Vec3::splat(-1.0));
        assert_eq!(a.max, Vec3::splat(1.0));
    }

    #[test]
    fn box_aabb_translates_with_center() {
        let s = box_shape(Vec3::new(10.0, -3.0, 4.0), Vec3::splat(0.5));
        let a = s.aabb();
        assert_eq!(a.min, Vec3::new(9.5, -3.5, 3.5));
        assert_eq!(a.max, Vec3::new(10.5, -2.5, 4.5));
    }

    #[test]
    fn box_aabb_expands_under_45deg_rotation() {
        // 1×1×1 box rotated 45° about Y has an AABB of ~√2 in xz.
        let half = Vec3::splat(0.5);
        let rot = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
        let s = ClearanceShape::Box {
            center: Vec3::ZERO,
            half_extents: half,
            rotation: rot,
        };
        let a = s.aabb();
        let expected_xz = (2.0_f32).sqrt() * 0.5;
        assert!((a.max.x - expected_xz).abs() < 1e-5);
        assert!((a.max.z - expected_xz).abs() < 1e-5);
        // Y-extent unchanged.
        assert!((a.max.y - 0.5).abs() < 1e-5);
    }

    #[test]
    fn cylinder_aabb_axis_aligned_y() {
        let s = ClearanceShape::Cylinder {
            center: Vec3::ZERO,
            axis: Vec3::Y,
            radius: 1.0,
            height: 4.0,
        };
        let a = s.aabb();
        // Y extent = ±2.
        assert!((a.min.y + 2.0).abs() < 1e-5);
        assert!((a.max.y - 2.0).abs() < 1e-5);
        // XZ at least ±radius.
        assert!(a.min.x <= -1.0 + 1e-5);
        assert!(a.max.x >= 1.0 - 1e-5);
    }

    #[test]
    fn polyhedron_aabb_picks_extremes() {
        let s = ClearanceShape::Polyhedron {
            vertices: vec![
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 2.0, 0.0),
                Vec3::new(0.0, 0.0, 3.0),
            ],
            faces: vec![],
        };
        let a = s.aabb();
        assert_eq!(a.min, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(a.max, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn aabb_overlaps_positive() {
        let a = EntityBounds {
            min: Vec3::ZERO,
            max: Vec3::splat(1.0),
        };
        let b = EntityBounds {
            min: Vec3::splat(0.5),
            max: Vec3::splat(1.5),
        };
        assert!(aabb_overlaps(&a, &b));
    }

    #[test]
    fn aabb_overlaps_separated_returns_false() {
        let a = EntityBounds {
            min: Vec3::ZERO,
            max: Vec3::splat(1.0),
        };
        let b = EntityBounds {
            min: Vec3::splat(2.0),
            max: Vec3::splat(3.0),
        };
        assert!(!aabb_overlaps(&a, &b));
    }

    #[test]
    fn aabb_overlaps_touching_returns_false() {
        // Touching faces should not be considered an overlap.
        let a = EntityBounds {
            min: Vec3::ZERO,
            max: Vec3::splat(1.0),
        };
        let b = EntityBounds {
            min: Vec3::new(1.0, 0.0, 0.0),
            max: Vec3::new(2.0, 1.0, 1.0),
        };
        assert!(!aabb_overlaps(&a, &b));
    }

    #[test]
    fn envelope_no_obstacles_no_findings() {
        let env = unit_envelope(ElementId(1), Vec3::ZERO);
        let findings = check_clearance_envelope(&env, &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn envelope_no_intersect_flags_overlapping_obstacle() {
        let env = unit_envelope(ElementId(1), Vec3::ZERO);
        let obs = [obstacle(99, Vec3::ZERO, Vec3::splat(0.5))];
        let findings = check_clearance_envelope(&env, &obs);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offending_element, ElementId(99));
        assert_eq!(findings[0].reason, FindingReason::Intrusion);
        assert_eq!(findings[0].ghost_kind, GhostKind::ClearanceEnvelope);
    }

    #[test]
    fn envelope_skips_owner_self_intrusion() {
        let env = unit_envelope(ElementId(1), Vec3::ZERO);
        // Owner overlaps its own envelope — should NOT be flagged.
        let obs = [obstacle(1, Vec3::ZERO, Vec3::splat(0.5))];
        let findings = check_clearance_envelope(&env, &obs);
        assert!(findings.is_empty());
    }

    #[test]
    fn envelope_separated_obstacle_no_finding() {
        let env = unit_envelope(ElementId(1), Vec3::ZERO);
        let obs = [obstacle(99, Vec3::new(5.0, 0.0, 0.0), Vec3::splat(0.5))];
        let findings = check_clearance_envelope(&env, &obs);
        assert!(findings.is_empty());
    }

    #[test]
    fn envelope_no_intersect_except_declared_whitelists() {
        let env = unit_envelope(ElementId(1), Vec3::ZERO)
            .with_constraint(ClearanceConstraint::NoIntersectExceptDeclared)
            .with_declared_intrusions(vec![ElementId(99)]);
        let obs = [obstacle(99, Vec3::ZERO, Vec3::splat(0.5))];
        let findings = check_clearance_envelope(&env, &obs);
        assert!(findings.is_empty(), "declared intrusion should be allowed");
    }

    #[test]
    fn envelope_no_intersect_except_declared_still_flags_others() {
        let env = unit_envelope(ElementId(1), Vec3::ZERO)
            .with_constraint(ClearanceConstraint::NoIntersectExceptDeclared)
            .with_declared_intrusions(vec![ElementId(99)]);
        let obs = [
            obstacle(99, Vec3::ZERO, Vec3::splat(0.5)),
            obstacle(100, Vec3::ZERO, Vec3::splat(0.5)),
        ];
        let findings = check_clearance_envelope(&env, &obs);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offending_element, ElementId(100));
    }

    #[test]
    fn envelope_kinds_round_trip_through_json() {
        let env = ClearanceEnvelope::usable(
            ElementId(7),
            "roof.truss.attic.storage_void",
            box_shape(Vec3::new(1.0, 1.5, 0.0), Vec3::new(2.0, 1.0, 1.5)),
        )
        .with_kind(ClearanceKind::Headroom)
        .with_label("attic storage headroom");
        let s = serde_json::to_string(&env).unwrap();
        let back: ClearanceEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn corridor_no_contributors_emits_missing() {
        let corr = SupportCorridor::new(
            ElementId(1),
            "wall.support_path",
            FaceAnchor::new(ElementId(2)),
            FaceAnchor::new(ElementId(3)),
            box_shape(Vec3::ZERO, Vec3::splat(1.0)),
        );
        let findings = check_support_corridor(&corr, &[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].reason, FindingReason::MissingContributors);
        assert_eq!(findings[0].ghost_kind, GhostKind::SupportCorridor);
    }

    #[test]
    fn corridor_continuous_passes_when_chain_overlaps() {
        // Three contributors, each touching the next at a face.
        let corr = SupportCorridor::new(
            ElementId(1),
            "wall.support_path",
            FaceAnchor::new(ElementId(10)),
            FaceAnchor::new(ElementId(12)),
            box_shape(Vec3::ZERO, Vec3::splat(10.0)),
        )
        .with_contributors(vec![ElementId(20), ElementId(21), ElementId(22)]);
        let obs = vec![
            obstacle(20, Vec3::new(0.0, 0.0, 0.0), Vec3::splat(0.6)),
            obstacle(21, Vec3::new(1.0, 0.0, 0.0), Vec3::splat(0.6)),
            obstacle(22, Vec3::new(2.0, 0.0, 0.0), Vec3::splat(0.6)),
        ];
        let findings = check_support_corridor(&corr, &obs);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn corridor_continuous_flags_gap() {
        let corr = SupportCorridor::new(
            ElementId(1),
            "wall.support_path",
            FaceAnchor::new(ElementId(10)),
            FaceAnchor::new(ElementId(12)),
            box_shape(Vec3::ZERO, Vec3::splat(10.0)),
        )
        .with_contributors(vec![ElementId(20), ElementId(21)]);
        // Far-apart contributors → broken load path.
        let obs = vec![
            obstacle(20, Vec3::new(0.0, 0.0, 0.0), Vec3::splat(0.4)),
            obstacle(21, Vec3::new(5.0, 0.0, 0.0), Vec3::splat(0.4)),
        ];
        let findings = check_support_corridor(&corr, &obs);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].reason, FindingReason::BrokenLoadPath);
        assert_eq!(findings[0].offending_element, ElementId(21));
    }

    #[test]
    fn corridor_staggered_allowed_passes_with_gap() {
        let corr = SupportCorridor::new(
            ElementId(1),
            "wall.support_path",
            FaceAnchor::new(ElementId(10)),
            FaceAnchor::new(ElementId(12)),
            box_shape(Vec3::ZERO, Vec3::splat(10.0)),
        )
        .with_contributors(vec![ElementId(20), ElementId(21)])
        .with_load_path(LoadPathRequirement::StaggeredAllowed);
        let obs = vec![
            obstacle(20, Vec3::new(0.0, 0.0, 0.0), Vec3::splat(0.4)),
            obstacle(21, Vec3::new(5.0, 0.0, 0.0), Vec3::splat(0.4)),
        ];
        let findings = check_support_corridor(&corr, &obs);
        assert!(findings.is_empty());
    }

    #[test]
    fn corridor_continuous_reports_missing_contributor() {
        let corr = SupportCorridor::new(
            ElementId(1),
            "wall.support_path",
            FaceAnchor::new(ElementId(10)),
            FaceAnchor::new(ElementId(12)),
            box_shape(Vec3::ZERO, Vec3::splat(10.0)),
        )
        .with_contributors(vec![ElementId(20), ElementId(21)]);
        // 21 absent from obstacle index → MissingContributors finding.
        let obs = vec![obstacle(20, Vec3::ZERO, Vec3::splat(0.5))];
        let findings = check_support_corridor(&corr, &obs);
        assert!(
            findings
                .iter()
                .any(|f| f.reason == FindingReason::MissingContributors
                    && f.offending_element == ElementId(21))
        );
    }

    #[test]
    fn corridor_round_trips_through_json() {
        let corr = SupportCorridor::new(
            ElementId(1),
            "wall.support_path",
            FaceAnchor::labeled(ElementId(10), "top"),
            FaceAnchor::labeled(ElementId(12), "bottom"),
            box_shape(Vec3::new(2.0, 1.0, 0.0), Vec3::new(0.5, 1.0, 0.5)),
        )
        .with_contributors(vec![ElementId(20), ElementId(21)])
        .with_label("primary load path");
        let s = serde_json::to_string(&corr).unwrap();
        let back: SupportCorridor = serde_json::from_str(&s).unwrap();
        assert_eq!(back, corr);
    }

    #[test]
    fn collect_world_obstacles_picks_up_triangle_mesh() {
        let mut world = World::new();
        world.spawn((
            ElementId(7),
            TriangleMesh {
                vertices: vec![
                    Vec3::new(-1.0, 0.0, -1.0),
                    Vec3::new(1.0, 0.0, -1.0),
                    Vec3::new(1.0, 0.0, 1.0),
                ],
                faces: vec![[0, 1, 2]],
                normals: None,
                name: Some("test".into()),
            },
        ));
        let obstacles = collect_world_obstacles(&world);
        assert_eq!(obstacles.len(), 1);
        assert_eq!(obstacles[0].element_id, ElementId(7));
        assert_eq!(obstacles[0].bounds.min, Vec3::new(-1.0, 0.0, -1.0));
        assert_eq!(obstacles[0].bounds.max, Vec3::new(1.0, 0.0, 1.0));
    }

    #[test]
    fn check_envelope_in_world_finds_intrusion() {
        let mut world = World::new();
        world.spawn((
            ElementId(50),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::splat(0.5),
            },
        ));
        let env = ClearanceEnvelope::usable(
            ElementId(1),
            "test.envelope",
            box_shape(Vec3::ZERO, Vec3::splat(1.0)),
        );
        let findings = check_envelope_in_world(&world, &env);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offending_element, ElementId(50));
    }

    #[test]
    fn check_envelope_in_world_passes_when_clear() {
        let mut world = World::new();
        // Obstacle far away.
        world.spawn((
            ElementId(50),
            BoxPrimitive {
                centre: Vec3::new(10.0, 0.0, 0.0),
                half_extents: Vec3::splat(0.5),
            },
        ));
        let env = ClearanceEnvelope::usable(
            ElementId(1),
            "test.envelope",
            box_shape(Vec3::ZERO, Vec3::splat(1.0)),
        );
        let findings = check_envelope_in_world(&world, &env);
        assert!(findings.is_empty());
    }
}
