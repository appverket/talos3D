//! Generic, domain-neutral geometric interference (clash) validation.
//!
//! ## Why this module exists
//!
//! A structural member that pokes *through* its weather covering (e.g. roof
//! trusses penetrating the roof skin) is a hard geometric malformation. Yet
//! such a model can still satisfy every *semantic* check — correct entity
//! count, a bounding box that spans the footprint, zero obligation findings —
//! because none of those checks look at whether solids actually interpenetrate.
//! That blind spot let an obviously broken house pass as "excellent".
//!
//! This module closes the blind spot with a generic primitive: **two solids
//! that are not supposed to share volume must not interpenetrate beyond a small
//! tolerance.** The geometry test (oriented-bounding-box separating-axis
//! overlap) is pure, domain-neutral Rust. *Which* pairs of things must not
//! interpenetrate is **data**: an [`InterferencePolicy`] of [`InterferenceRule`]s
//! loaded from the knowledge store. No roof/wall/house facts live in this file.
//!
//! ## How a rule reads
//!
//! ```jsonc
//! {
//!   "id": "framing_must_not_penetrate_covering",
//!   "subject": { "element_classes": [], "component_roles": ["structural_framing"] },
//!   "barrier": { "element_classes": [], "component_roles": ["weather_covering"] },
//!   "relation": "must_not_penetrate",
//!   "tolerance_m": 0.003,
//!   "severity": "error",
//!   "rationale": "Covering layers sit on the outside of the framing; framing that
//!                 protrudes through the covering is a weather-tightness failure."
//! }
//! ```
//!
//! A *selector* matches an entity when every non-empty facet matches: the
//! entity's `element_class` is in `element_classes` (empty = any), and its
//! `component_role` (read from `SemanticIntent.parameters.component_role`) is in
//! `component_roles` (empty = any). The validator emits a [`Finding`] for each
//! subject entity that interpenetrates a barrier entity by more than
//! `tolerance_m`.
//!
//! Correctly built layered assemblies are *face-flush*: along the contact normal
//! the projected overlap is zero, so they are reported as separated and produce
//! no finding. Only genuine interpenetration is flagged.

use std::sync::Arc;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::capability_registry::{
    Applicability, ConstraintDescriptor, ConstraintId, ConstraintRole, ElementClassAssignment,
    Finding, FindingId, PassageRef, Severity,
};
use crate::plugins::identity::ElementId;
use crate::plugins::modeling::primitives::{BoxPrimitive, ShapeRotation};
use crate::plugins::refinement::SemanticIntent;

/// Stable identifier of the single generic interference constraint.
pub const INTERFERENCE_CONSTRAINT_ID: &str = "GeometricInterference";

/// The free-form semantic parameter key an entity carries to declare which
/// layer/role it plays inside an assembly (e.g. `"structural_framing"`,
/// `"weather_covering"`). Read from `SemanticIntent.parameters[COMPONENT_ROLE_KEY]`.
///
/// This is a *convention*, not a hardcoded vocabulary: the set of valid role
/// strings lives entirely in the data (recipes that tag boxes and rules that
/// reference them), never in this file.
pub const COMPONENT_ROLE_KEY: &str = "component_role";

// ---------------------------------------------------------------------------
// Data model (all loaded from the knowledge store)
// ---------------------------------------------------------------------------

/// Selects a set of entities by semantic facets. An empty facet matches any
/// value; a non-empty facet matches when the entity's value is contained in it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EntitySelector {
    /// Entity `element_class` must be one of these (empty = any class).
    #[serde(default)]
    pub element_classes: Vec<String>,
    /// Entity `component_role` must be one of these (empty = any role).
    #[serde(default)]
    pub component_roles: Vec<String>,
}

impl EntitySelector {
    /// True when `class`/`role` satisfy this selector. An empty facet matches
    /// anything. `pub` so the declarative constraint substrate (ADR-057,
    /// `super::constraint_spec`) can reuse the same selector semantics without
    /// duplicating them.
    pub fn matches(&self, class: Option<&str>, role: Option<&str>) -> bool {
        if !self.element_classes.is_empty() {
            let Some(class) = class else { return false };
            if !self.element_classes.iter().any(|c| c == class) {
                return false;
            }
        }
        if !self.component_roles.is_empty() {
            let Some(role) = role else { return false };
            if !self.component_roles.iter().any(|r| r == role) {
                return false;
            }
        }
        true
    }
}

/// The geometric relation a rule asserts must hold between subject and barrier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRelation {
    /// Subject solids must not share volume with barrier solids beyond the
    /// rule tolerance. Face-flush contact is allowed.
    #[default]
    MustNotPenetrate,
}

/// How a rule's findings map onto the severity ladder. Mirrors
/// [`Severity`] but is deserialized from lowercase data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuleSeverity {
    Advice,
    Warning,
    #[default]
    Error,
}

impl From<RuleSeverity> for Severity {
    fn from(value: RuleSeverity) -> Self {
        match value {
            RuleSeverity::Advice => Severity::Advice,
            RuleSeverity::Warning => Severity::Warning,
            RuleSeverity::Error => Severity::Error,
        }
    }
}

fn default_tolerance_m() -> f32 {
    0.003
}

/// A single data-driven interference rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InterferenceRule {
    /// Stable machine-readable id; used to build deterministic finding ids.
    pub id: String,
    /// Short human-readable label.
    #[serde(default)]
    pub label: String,
    /// Entities playing the "subject" side (the thing that must not intrude).
    pub subject: EntitySelector,
    /// Entities playing the "barrier" side (the thing that must enclose / not
    /// be penetrated).
    pub barrier: EntitySelector,
    /// The asserted relation.
    #[serde(default)]
    pub relation: InterferenceRelation,
    /// Penetration depth (metres) below which contact is treated as flush and
    /// not reported. Absorbs floating-point noise on coincident faces.
    #[serde(default = "default_tolerance_m")]
    pub tolerance_m: f32,
    /// Severity of an emitted finding.
    #[serde(default)]
    pub severity: RuleSeverity,
    /// Why this rule exists (surfaced verbatim in findings).
    #[serde(default)]
    pub rationale: String,
    /// Optional corpus backlink (`"doc/section"`).
    #[serde(default)]
    pub backlink: Option<String>,
}

/// The full set of interference rules in force, populated from the knowledge
/// store at startup. Empty by default: with no rules the validator is a no-op,
/// so the platform ships with zero domain opinions baked in.
#[derive(Resource, Debug, Clone, Default)]
pub struct InterferencePolicy {
    pub rules: Vec<InterferenceRule>,
}

impl InterferencePolicy {
    pub fn with_rules(rules: Vec<InterferenceRule>) -> Self {
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Oriented bounding box + separating-axis overlap
// ---------------------------------------------------------------------------

/// An oriented bounding box: centre, three orthonormal axes, and the
/// half-extent along each axis.
#[derive(Debug, Clone, Copy)]
struct Obb {
    centre: Vec3,
    axes: [Vec3; 3],
    half: [f32; 3],
}

impl Obb {
    fn from_box(prim: &BoxPrimitive, rotation: Quat) -> Self {
        Self {
            centre: prim.centre,
            axes: [
                rotation * Vec3::X,
                rotation * Vec3::Y,
                rotation * Vec3::Z,
            ],
            half: [
                prim.half_extents.x.abs(),
                prim.half_extents.y.abs(),
                prim.half_extents.z.abs(),
            ],
        }
    }

    /// Conservative world-space AABB used for cheap broad-phase rejection.
    fn aabb(&self) -> (Vec3, Vec3) {
        let extent = self.axes[0].abs() * self.half[0]
            + self.axes[1].abs() * self.half[1]
            + self.axes[2].abs() * self.half[2];
        (self.centre - extent, self.centre + extent)
    }

    /// Projected radius of the box onto a (not necessarily unit) axis.
    fn projected_radius(&self, axis: Vec3) -> f32 {
        self.half[0] * self.axes[0].dot(axis).abs()
            + self.half[1] * self.axes[1].dot(axis).abs()
            + self.half[2] * self.axes[2].dot(axis).abs()
    }
}

fn aabb_separated((amin, amax): (Vec3, Vec3), (bmin, bmax): (Vec3, Vec3), pad: f32) -> bool {
    amax.x + pad < bmin.x
        || bmax.x + pad < amin.x
        || amax.y + pad < bmin.y
        || bmax.y + pad < amin.y
        || amax.z + pad < bmin.z
        || bmax.z + pad < amin.z
}

/// Penetration depth (metres) between two oriented boxes, or `None` if they are
/// separated along any axis.
///
/// Uses the separating-axis theorem across all 15 candidate axes (3 face
/// normals per box plus 9 edge-edge cross products) to decide separation. If no
/// axis separates them, the returned depth is the minimum projected overlap
/// across the six face axes — a robust lower bound on how far the boxes
/// interpenetrate. Degenerate (near-parallel) cross-product axes are skipped.
fn obb_penetration_depth(a: &Obb, b: &Obb) -> Option<f32> {
    const EPS: f32 = 1e-6;
    let t = b.centre - a.centre;

    let mut min_face_overlap = f32::INFINITY;

    // Face axes of A (0..2) and B (3..5), then 9 cross products (6..14).
    let mut axes: Vec<(Vec3, bool)> = Vec::with_capacity(15);
    for ax in a.axes.iter() {
        axes.push((*ax, true));
    }
    for ax in b.axes.iter() {
        axes.push((*ax, true));
    }
    for ai in 0..3 {
        for bi in 0..3 {
            axes.push((a.axes[ai].cross(b.axes[bi]), false));
        }
    }

    for (axis, is_face) in axes {
        let len = axis.length();
        if len < EPS {
            // Degenerate cross product (parallel edges); not a useful axis.
            continue;
        }
        let n = axis / len;
        let dist = t.dot(n).abs();
        let overlap = a.projected_radius(n) + b.projected_radius(n) - dist;
        if overlap <= 0.0 {
            // A separating axis exists → boxes do not interpenetrate.
            return None;
        }
        if is_face && overlap < min_face_overlap {
            min_face_overlap = overlap;
        }
    }

    if min_face_overlap.is_finite() {
        Some(min_face_overlap)
    } else {
        // Should not happen (face axes are never degenerate for a real box),
        // but stay safe rather than report a bogus zero.
        None
    }
}

// ---------------------------------------------------------------------------
// World accessors
// ---------------------------------------------------------------------------

fn entity_obb(entity: Entity, world: &World) -> Option<Obb> {
    let prim = world.get::<BoxPrimitive>(entity)?;
    let rotation = world
        .get::<ShapeRotation>(entity)
        .map(|r| r.0)
        .unwrap_or(Quat::IDENTITY);
    Some(Obb::from_box(prim, rotation))
}

/// Read an entity's free-form `component_role` (e.g. `"structural_framing"`)
/// from `SemanticIntent.parameters`. Shared with the declarative-constraint
/// substrate (ADR-057) so role matching is defined in exactly one place.
pub fn entity_component_role(entity: Entity, world: &World) -> Option<String> {
    let intent = world.get::<SemanticIntent>(entity)?;
    intent
        .parameters
        .get(COMPONENT_ROLE_KEY)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Read an entity's `element_class` id, if assigned. Shared with the
/// declarative-constraint substrate (ADR-057).
pub fn entity_class(entity: Entity, world: &World) -> Option<String> {
    world
        .get::<ElementClassAssignment>(entity)
        .map(|c| c.element_class.0.clone())
}

// ---------------------------------------------------------------------------
// Constraint
// ---------------------------------------------------------------------------

/// Build the generic interference [`ConstraintDescriptor`]. Its validator reads
/// the [`InterferencePolicy`] resource each sweep, so editing the policy data
/// (or hot-reloading it) changes behaviour with no code change.
pub fn interference_constraint() -> ConstraintDescriptor {
    ConstraintDescriptor {
        id: ConstraintId(INTERFERENCE_CONSTRAINT_ID.into()),
        label: "Geometric interference".into(),
        description: "Solids that a rule says must not share volume must not interpenetrate \
                      beyond tolerance. Face-flush contact is allowed. Rules are data \
                      (InterferencePolicy); this check carries no domain knowledge itself."
            .into(),
        applicability: Applicability::any(),
        default_severity: Severity::Error,
        rationale: "Semantic checks (entity count, bounding box, obligations) cannot see when \
                    one solid pokes through another. Member-through-covering penetration is a \
                    hard geometric failure that must be caught mechanically, not by eye."
            .into(),
        source_backlink: None,
        role: ConstraintRole::Validation,
        validator: Arc::new(run_interference),
    }
}

fn run_interference(subject: Entity, world: &World) -> Vec<Finding> {
    let Some(policy) = world.get_resource::<InterferencePolicy>() else {
        return Vec::new();
    };
    if policy.is_empty() {
        return Vec::new();
    }

    let Some(subject_obb) = entity_obb(subject, world) else {
        return Vec::new();
    };
    let Some(subject_eid) = world.get::<ElementId>(subject).map(|e| e.0) else {
        return Vec::new();
    };
    let subject_class = entity_class(subject, world);
    let subject_role = entity_component_role(subject, world);
    let subject_class_ref = subject_class.as_deref();
    let subject_role_ref = subject_role.as_deref();

    // Which rules does this entity participate in (as subject)?
    let active: Vec<&InterferenceRule> = policy
        .rules
        .iter()
        .filter(|r| r.subject.matches(subject_class_ref, subject_role_ref))
        .collect();
    if active.is_empty() {
        return Vec::new();
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let subject_aabb = subject_obb.aabb();

    // Scan all other box entities once.
    let Some(mut candidates) = world.try_query::<(Entity, &ElementId, &BoxPrimitive)>() else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    for (other, other_eid, _) in candidates.iter(world) {
        if other == subject {
            continue;
        }
        let other_eid = other_eid.0;
        let other_class = entity_class(other, world);
        let other_role = entity_component_role(other, world);
        let other_class_ref = other_class.as_deref();
        let other_role_ref = other_role.as_deref();

        for rule in &active {
            if !rule.barrier.matches(other_class_ref, other_role_ref) {
                continue;
            }
            // Symmetric-rule dedup: if both directions match, only the lower
            // element id emits, so each interpenetrating pair is reported once.
            let symmetric = rule.subject.matches(other_class_ref, other_role_ref)
                && rule.barrier.matches(subject_class_ref, subject_role_ref);
            if symmetric && subject_eid > other_eid {
                continue;
            }

            let Some(other_obb) = entity_obb(other, world) else {
                continue;
            };
            // Cheap broad-phase: skip pairs whose padded AABBs do not touch.
            if aabb_separated(subject_aabb, other_obb.aabb(), rule.tolerance_m) {
                continue;
            }
            let Some(depth) = obb_penetration_depth(&subject_obb, &other_obb) else {
                continue;
            };
            if depth <= rule.tolerance_m {
                continue;
            }

            let label = if rule.label.is_empty() {
                rule.id.as_str()
            } else {
                rule.label.as_str()
            };
            findings.push(Finding {
                id: FindingId(format!(
                    "{INTERFERENCE_CONSTRAINT_ID}:{}:{}:{}",
                    rule.id, subject_eid, other_eid
                )),
                constraint_id: ConstraintId(INTERFERENCE_CONSTRAINT_ID.into()),
                subject: subject_eid,
                severity: rule.severity.into(),
                message: format!(
                    "[{label}] entity {subject_eid} interpenetrates entity {other_eid} by \
                     {:.0} mm (tolerance {:.0} mm)",
                    depth * 1000.0,
                    rule.tolerance_m * 1000.0
                ),
                rationale: rule.rationale.clone(),
                backlink: rule
                    .backlink
                    .as_ref()
                    .map(|b| PassageRef(b.clone())),
                emitted_at: now,
                role: ConstraintRole::Validation,
            });
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obb(centre: Vec3, half: Vec3, rot: Quat) -> Obb {
        Obb::from_box(
            &BoxPrimitive {
                centre,
                half_extents: half,
            },
            rot,
        )
    }

    #[test]
    fn flush_faces_are_not_penetrating() {
        // Two unit cubes stacked face-to-face along Y: centres 0 and 1, half 0.5.
        let a = obb(Vec3::ZERO, Vec3::splat(0.5), Quat::IDENTITY);
        let b = obb(Vec3::new(0.0, 1.0, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        assert!(obb_penetration_depth(&a, &b).is_none());
    }

    #[test]
    fn clearly_separated_boxes_are_not_penetrating() {
        let a = obb(Vec3::ZERO, Vec3::splat(0.5), Quat::IDENTITY);
        let b = obb(Vec3::new(0.0, 5.0, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        assert!(obb_penetration_depth(&a, &b).is_none());
    }

    #[test]
    fn overlapping_boxes_report_depth() {
        // Overlap by 0.2 m along Y.
        let a = obb(Vec3::ZERO, Vec3::splat(0.5), Quat::IDENTITY);
        let b = obb(Vec3::new(0.0, 0.8, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        let depth = obb_penetration_depth(&a, &b).expect("should penetrate");
        assert!((depth - 0.2).abs() < 1e-4, "depth was {depth}");
    }

    #[test]
    fn rotated_member_poking_through_thin_slab_is_detected() {
        // A thin "covering" slab in the XZ plane at y in [0.0,0.03], and a
        // 45°-rotated "framing" bar whose tip pushes past the slab's top face.
        let slab = obb(
            Vec3::new(0.0, 0.015, 0.0),
            Vec3::new(2.0, 0.015, 2.0),
            Quat::IDENTITY,
        );
        let bar = obb(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.05, 0.2, 0.05),
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
        );
        assert!(obb_penetration_depth(&slab, &bar).is_some());
    }

    #[test]
    fn selector_matches_on_role_and_class() {
        let sel = EntitySelector {
            element_classes: vec![],
            component_roles: vec!["structural_framing".into()],
        };
        assert!(sel.matches(Some("roof_system"), Some("structural_framing")));
        assert!(!sel.matches(Some("roof_system"), Some("weather_covering")));
        assert!(!sel.matches(Some("roof_system"), None));

        let any = EntitySelector::default();
        assert!(any.matches(None, None));
        assert!(any.matches(Some("anything"), Some("whatever")));
    }

    fn spawn_member(
        world: &mut World,
        eid: u64,
        class: &str,
        role: &str,
        centre: Vec3,
        half: Vec3,
    ) {
        use crate::capability_registry::{ElementClassAssignment, ElementClassId};
        world.spawn((
            ElementId(eid),
            BoxPrimitive {
                centre,
                half_extents: half,
            },
            ShapeRotation(Quat::IDENTITY),
            ElementClassAssignment {
                element_class: ElementClassId(class.into()),
                active_recipe: None,
            },
            SemanticIntent {
                parameters: serde_json::json!({ COMPONENT_ROLE_KEY: role }),
                unresolved_decisions: Vec::new(),
                source_refs: Vec::new(),
            },
        ));
    }

    fn framing_through_covering_rule() -> InterferenceRule {
        InterferenceRule {
            id: "framing_not_through_covering".into(),
            label: "framing must not protrude through covering".into(),
            subject: EntitySelector {
                element_classes: vec![],
                component_roles: vec!["structural_framing".into()],
            },
            barrier: EntitySelector {
                element_classes: vec![],
                component_roles: vec!["weather_covering".into()],
            },
            relation: InterferenceRelation::MustNotPenetrate,
            tolerance_m: 0.003,
            severity: RuleSeverity::Error,
            rationale: "roofing goes on the outside of the framing".into(),
            backlink: None,
        }
    }

    #[test]
    fn framing_poking_through_covering_emits_finding() {
        let mut world = World::new();
        world.insert_resource(InterferencePolicy::with_rules(vec![
            framing_through_covering_rule(),
        ]));
        // Framing bar centred at y=0, top at y=0.2.
        spawn_member(
            &mut world,
            1,
            "roof_system",
            "structural_framing",
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.05, 0.2, 0.05),
        );
        // Covering slab whose mid-plane sits at y=0.1 → it straddles the bar,
        // so the bar pierces it (interpenetration well over tolerance).
        spawn_member(
            &mut world,
            2,
            "roof_system",
            "weather_covering",
            Vec3::new(0.0, 0.1, 0.0),
            Vec3::new(1.0, 0.015, 1.0),
        );

        let subject = world
            .query::<(Entity, &ElementId)>()
            .iter(&world)
            .find_map(|(e, id)| (id.0 == 1).then_some(e))
            .unwrap();
        let findings = run_interference(subject, &world);
        assert_eq!(findings.len(), 1, "expected one interference finding");
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].subject, 1);
    }

    #[test]
    fn covering_outboard_of_framing_is_clean() {
        let mut world = World::new();
        world.insert_resource(InterferencePolicy::with_rules(vec![
            framing_through_covering_rule(),
        ]));
        // Framing bar: top face at y = 0.2.
        spawn_member(
            &mut world,
            1,
            "roof_system",
            "structural_framing",
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.05, 0.2, 0.05),
        );
        // Covering slab sits flush *outboard* of the framing top face: bottom
        // face at y = 0.2, 30 mm thick → centre y = 0.215.
        spawn_member(
            &mut world,
            2,
            "roof_system",
            "weather_covering",
            Vec3::new(0.0, 0.215, 0.0),
            Vec3::new(1.0, 0.015, 1.0),
        );

        let subject = world
            .query::<(Entity, &ElementId)>()
            .iter(&world)
            .find_map(|(e, id)| (id.0 == 1).then_some(e))
            .unwrap();
        let findings = run_interference(subject, &world);
        assert!(
            findings.is_empty(),
            "flush covering should not be flagged, got {findings:?}"
        );
    }

    #[test]
    fn rule_deserializes_with_defaults() {
        let json = r#"{
            "id": "framing_not_through_covering",
            "subject": { "component_roles": ["structural_framing"] },
            "barrier": { "component_roles": ["weather_covering"] }
        }"#;
        let rule: InterferenceRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.relation, InterferenceRelation::MustNotPenetrate);
        assert_eq!(rule.severity, RuleSeverity::Error);
        assert!((rule.tolerance_m - 0.003).abs() < 1e-9);
    }
}
