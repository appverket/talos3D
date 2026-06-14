//! Generic, domain-neutral **declarative constraint** substrate (ADR-057).
//!
//! ## Why this module exists
//!
//! Enforcement that encodes *domain* knowledge — "a roof must bear on a wall",
//! "an occurrence must render real geometry" — was being written as compiled
//! Rust validators. Compiled rules cannot be tuned, narrowed, or replaced by the
//! agent that uses the system; changing one needs a human and a recompile, and
//! the lesson never enters the discoverable knowledge corpus. ADR-057 makes the
//! rule the data and the engine the code: a domain rule is a [`ConstraintSpec`]
//! in the knowledge store, and this module is the generic interpreter that turns
//! specs into live findings.
//!
//! It mirrors the [`InterferencePolicy`](super::interference) pattern exactly: a
//! **single** [`ConstraintDescriptor`] reads a [`ConstraintSpecRegistry`]
//! resource each sweep, so adding/editing/hot-reloading a spec changes behaviour
//! with **no code change**. No domain nouns live in this file — applicability is
//! matched on opaque `element_class` / `component_role` strings carried by the
//! spec data, and the predicate vocabulary is geometric and domain-neutral.
//!
//! ## How a spec reads
//!
//! ```jsonc
//! {
//!   "id": "roof_bears_on_wall",
//!   "revision": 1,
//!   "label": "roof must bear on a wall",
//!   "applicability": { "component_roles": ["roof_system"], "required_state_floor": "schematic" },
//!   "predicate": {
//!     "predicate": "vertical_contact",
//!     "support": { "component_roles": ["exterior_wall"] },
//!     "max_gap_m": 0.05
//!   },
//!   "severity": "error",
//!   "message_template": "roof {subject} is floating: no wall within bearing tolerance beneath it",
//!   "grounding_tag": "policy-backed",
//!   "backlink": "VERTICAL_LOAD_PATH_AND_ERECTION_ORDER_2026"
//! }
//! ```
//!
//! A spec emits a [`Finding`] for a subject when the subject is *applicable*
//! (its class/role matches the spec selector and its refinement state is at or
//! above the optional floor) **and** the spec's predicate evaluates to `false`
//! (the asserted condition does not hold).

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::authored_entity::EntityBounds;
use crate::capability_registry::{
    Applicability, CapabilityRegistry, ConstraintDescriptor, ConstraintId, ConstraintRole, Finding,
    FindingId, PassageRef, Severity,
};
use crate::plugins::identity::ElementId;
use crate::plugins::interference::{
    entity_class, entity_component_role, EntitySelector, RuleSeverity,
};
use crate::plugins::refinement::{RefinementState, RefinementStateComponent};

/// Stable identifier of the single generic declarative constraint. Individual
/// findings carry their *spec's* id as their `constraint_id`, so a spec is
/// attributable even though one descriptor evaluates them all.
pub const DECLARATIVE_CONSTRAINT_ID: &str = "DeclarativeConstraint";

// ---------------------------------------------------------------------------
// Data model (all loaded from the knowledge store)
// ---------------------------------------------------------------------------

/// A cartesian axis, deserialized from lowercase data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Y,
    Z,
}

fn default_min_extent_m() -> f32 {
    1e-4
}

fn default_bearing_gap_m() -> f32 {
    0.05
}

/// The closed, domain-neutral v1 predicate vocabulary (ADR-057 §2). New
/// *vocabulary* is a reviewed code change; new *rules* over this vocabulary are
/// data. Internally tagged on the `predicate` key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "predicate", rename_all = "snake_case")]
pub enum Predicate {
    /// The subject's captured geometry has a non-degenerate bounding box: its
    /// largest axis span is at least `min_extent_m`. False when the snapshot is
    /// absent or collapses to a point/line/plane.
    NonDegenerateBounds {
        #[serde(default = "default_min_extent_m")]
        min_extent_m: f32,
    },
    /// The subject's bounding-box extent along `axis` lies within
    /// `[min_m, max_m]` (each optional). False when the subject has no bounds.
    ExtentAxisWithin {
        axis: Axis,
        #[serde(default)]
        min_m: Option<f32>,
        #[serde(default)]
        max_m: Option<f32>,
    },
    /// The subject physically rests on some entity matching `support`: there is
    /// a support entity whose top face is within `max_gap_m` below the subject's
    /// underside and whose footprint overlaps the subject horizontally. False
    /// when the subject floats (no support within tolerance) — the load-path
    /// continuity check, expressed as data.
    VerticalContact {
        support: EntitySelector,
        #[serde(default = "default_bearing_gap_m")]
        max_gap_m: f32,
    },
    /// At least one entity in the model matches `selector`. Useful as an
    /// existence/closure assertion.
    RolePresent { selector: EntitySelector },
    /// All sub-predicates hold.
    All { all: Vec<Predicate> },
    /// At least one sub-predicate holds.
    Any { any: Vec<Predicate> },
    /// The sub-predicate does not hold.
    Not { not: Box<Predicate> },
}

/// Which subjects a spec applies to. An empty selector facet matches anything;
/// the optional `required_state_floor` exempts subjects below a refinement
/// state (e.g. Conceptual massing is exempt from a load-path rule).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpecApplicability {
    #[serde(flatten)]
    pub selector: EntitySelector,
    #[serde(default)]
    pub required_state_floor: Option<RefinementState>,
}

/// Lifecycle status of a spec. Only `Active` specs are evaluated. Externally
/// tagged so the common cases author as bare strings (`"active"`, `"draft"`)
/// and the data-carrying case as `{ "blocked": { "reason": "…" } }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecStatus {
    #[default]
    Active,
    Draft,
    Blocked {
        reason: String,
    },
}

fn default_revision() -> u32 {
    1
}

/// A single declarative, data-authored constraint (ADR-057 §2). Versioned and
/// replaceable: a newer revision may `supersedes` an older spec id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConstraintSpec {
    /// Stable machine-readable id; used to build deterministic finding ids and
    /// as the finding's `constraint_id`.
    pub id: String,
    #[serde(default = "default_revision")]
    pub revision: u32,
    /// Optional id of a spec this one replaces.
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub applicability: SpecApplicability,
    /// The asserted condition. A finding is emitted when this evaluates `false`.
    pub predicate: Predicate,
    #[serde(default)]
    pub severity: RuleSeverity,
    /// Message template. `{subject}` and `{spec_id}` are substituted; empty =
    /// a generated default.
    #[serde(default)]
    pub message_template: String,
    /// ADR-052 epistemic grounding tag (`source-backed` | `policy-backed` |
    /// `user-specified` | `unresolved(CorpusGap)`), surfaced for provenance.
    #[serde(default)]
    pub grounding_tag: Option<String>,
    /// Optional corpus backlink (passage ref or `"doc/section"`).
    #[serde(default)]
    pub backlink: Option<String>,
    #[serde(default)]
    pub status: SpecStatus,
}

impl ConstraintSpec {
    fn is_active(&self) -> bool {
        matches!(self.status, SpecStatus::Active)
    }
}

/// All declarative constraint specs in force, populated from the knowledge
/// store at startup. Empty by default: with no specs the constraint is a no-op,
/// so the platform ships with zero domain opinions baked in.
#[derive(Resource, Debug, Clone, Default)]
pub struct ConstraintSpecRegistry {
    pub specs: Vec<ConstraintSpec>,
}

impl ConstraintSpecRegistry {
    pub fn with_specs(specs: Vec<ConstraintSpec>) -> Self {
        Self { specs }
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Insert a spec, replacing any existing spec with the same id whose
    /// revision is lower or equal (newest revision wins). Returns whether the
    /// spec was stored.
    pub fn upsert(&mut self, spec: ConstraintSpec) -> bool {
        if let Some(existing) = self.specs.iter_mut().find(|s| s.id == spec.id) {
            if spec.revision >= existing.revision {
                *existing = spec;
                return true;
            }
            return false;
        }
        self.specs.push(spec);
        true
    }

    fn active(&self) -> impl Iterator<Item = &ConstraintSpec> {
        self.specs.iter().filter(|s| s.is_active())
    }
}

// ---------------------------------------------------------------------------
// Disk persistence (knowledge store)
// ---------------------------------------------------------------------------

/// `~/.talos3d/knowledge/constraints` (honours `TALOS3D_KNOWLEDGE_DIR`).
pub fn constraints_dir() -> PathBuf {
    crate::plugins::knowledge_persistence::knowledge_dir().join("constraints")
}

/// Load every `*.json` constraint spec from [`constraints_dir`] into `registry`.
/// Malformed files are logged and skipped; loading never panics.
pub fn load_persisted_constraint_specs(registry: &mut ConstraintSpecRegistry) {
    let dir = constraints_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        // Missing dir is normal on a fresh install.
        return;
    };
    let mut loaded = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<ConstraintSpec>(&s).ok())
        {
            Some(spec) => {
                if registry.upsert(spec) {
                    loaded += 1;
                }
            }
            None => {
                bevy::log::warn!(
                    "constraint_spec: skipping unreadable/malformed spec at {:?}",
                    path
                );
            }
        }
    }
    bevy::log::info!(
        "constraint_spec: loaded {loaded} declarative constraint spec(s) from {:?}",
        dir
    );
}

/// Persist a single spec to `constraints_dir()/<id>.json`. The durable-write
/// path the MCP authoring tools (PP-DAEK-2) build on.
pub fn save_constraint_spec(spec: &ConstraintSpec) -> std::io::Result<PathBuf> {
    let dir = constraints_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", spec.id));
    let json = serde_json::to_string_pretty(spec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// World accessors
// ---------------------------------------------------------------------------

/// Captured world-space bounds of an entity via the capability registry, or
/// `None` if it has no resolvable geometry snapshot.
fn entity_bounds(entity: Entity, world: &World) -> Option<EntityBounds> {
    let registry = world.get_resource::<CapabilityRegistry>()?;
    let entity_ref = world.get_entity(entity).ok()?;
    registry.capture_snapshot(&entity_ref, world)?.bounds()
}

/// Largest bounding-box axis span (metres).
fn max_extent(b: &EntityBounds) -> f32 {
    let e = b.max - b.min;
    e.x.abs().max(e.y.abs()).max(e.z.abs())
}

/// True when two bounds overlap in the horizontal (X/Z) plane.
fn xz_overlap(a: &EntityBounds, b: &EntityBounds) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.z <= b.max.z && b.min.z <= a.max.z
}

// ---------------------------------------------------------------------------
// Predicate evaluation
// ---------------------------------------------------------------------------

/// Evaluate a predicate for `subject`. Returns `true` when the asserted
/// condition holds (no finding) and `false` when it is violated (emit finding).
fn eval(pred: &Predicate, subject: Entity, world: &World) -> bool {
    match pred {
        Predicate::NonDegenerateBounds { min_extent_m } => match entity_bounds(subject, world) {
            Some(b) => max_extent(&b) >= *min_extent_m,
            None => false,
        },
        Predicate::ExtentAxisWithin { axis, min_m, max_m } => {
            let Some(b) = entity_bounds(subject, world) else {
                return false;
            };
            let e = b.max - b.min;
            let span = match axis {
                Axis::X => e.x,
                Axis::Y => e.y,
                Axis::Z => e.z,
            }
            .abs();
            if let Some(min) = min_m {
                if span < *min {
                    return false;
                }
            }
            if let Some(max) = max_m {
                if span > *max {
                    return false;
                }
            }
            true
        }
        Predicate::VerticalContact { support, max_gap_m } => {
            let Some(sb) = entity_bounds(subject, world) else {
                // No geometry to test contact for; treat as satisfied so this
                // rule never penalises an entity it cannot measure.
                return true;
            };
            for_each_other_entity(world, |other, world| {
                if other == subject {
                    return false;
                }
                if !support.matches(
                    entity_class(other, world).as_deref(),
                    entity_component_role(other, world).as_deref(),
                ) {
                    return false;
                }
                let Some(ob) = entity_bounds(other, world) else {
                    return false;
                };
                if !xz_overlap(&sb, &ob) {
                    return false;
                }
                // Gap between the subject's underside and the support's top.
                // Negative means they interpenetrate (also acceptable contact).
                let gap = sb.min.y - ob.max.y;
                gap <= *max_gap_m && gap >= -(*max_gap_m).abs() - 1.0
            })
        }
        Predicate::RolePresent { selector } => for_each_other_entity(world, |other, world| {
            selector.matches(
                entity_class(other, world).as_deref(),
                entity_component_role(other, world).as_deref(),
            )
        }),
        Predicate::All { all } => all.iter().all(|p| eval(p, subject, world)),
        Predicate::Any { any } => any.iter().any(|p| eval(p, subject, world)),
        Predicate::Not { not } => !eval(not, subject, world),
    }
}

/// Run `f` over every entity carrying an [`ElementId`], short-circuiting `true`.
fn for_each_other_entity(world: &World, mut f: impl FnMut(Entity, &World) -> bool) -> bool {
    let Some(mut q) = world.try_query::<(Entity, &ElementId)>() else {
        return false;
    };
    for (entity, _) in q.iter(world) {
        if f(entity, world) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Constraint
// ---------------------------------------------------------------------------

/// Build the single generic declarative [`ConstraintDescriptor`]. Its validator
/// reads the [`ConstraintSpecRegistry`] resource each sweep, so editing the spec
/// data (or hot-reloading it) changes behaviour with no code change.
pub fn declarative_constraint() -> ConstraintDescriptor {
    ConstraintDescriptor {
        id: ConstraintId(DECLARATIVE_CONSTRAINT_ID.into()),
        label: "Declarative constraint".into(),
        description: "Evaluates data-authored ConstraintSpecs (ADR-057). Each spec asserts a \
                      geometric/topological condition over entities selected by opaque \
                      class/role data; a violation emits a finding. This check carries no \
                      domain knowledge itself — the rules are data and are amendable without \
                      recompiling."
            .into(),
        applicability: Applicability::any(),
        default_severity: Severity::Error,
        rationale: "Domain enforcement must be knowledge the system can amend, version, and \
                    grow as it is used — not static code. A generic interpreter over a small \
                    domain-neutral predicate vocabulary lets a new rule be added as data."
            .into(),
        source_backlink: None,
        role: ConstraintRole::Validation,
        validator: Arc::new(run_declarative_constraints),
    }
}

fn run_declarative_constraints(subject: Entity, world: &World) -> Vec<Finding> {
    let Some(registry) = world.get_resource::<ConstraintSpecRegistry>() else {
        return Vec::new();
    };
    if registry.is_empty() {
        return Vec::new();
    }
    let Some(subject_eid) = world.get::<ElementId>(subject).map(|e| e.0) else {
        return Vec::new();
    };
    let subject_class = entity_class(subject, world);
    let subject_role = entity_component_role(subject, world);
    let subject_state = world
        .get::<RefinementStateComponent>(subject)
        .map(|c| c.state);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut findings = Vec::new();
    for spec in registry.active() {
        // Applicability: class/role selector.
        if !spec
            .applicability
            .selector
            .matches(subject_class.as_deref(), subject_role.as_deref())
        {
            continue;
        }
        // Applicability: refinement-state floor (subjects below the floor are
        // exempt, e.g. Conceptual massing).
        if let Some(floor) = spec.applicability.required_state_floor {
            match subject_state {
                Some(state) if state >= floor => {}
                Some(_) => continue,
                // No refinement state component: a floor is specified but we
                // cannot place the subject, so do not enforce.
                None => continue,
            }
        }

        // The spec asserts its predicate must hold; a false result is a finding.
        if eval(&spec.predicate, subject, world) {
            continue;
        }

        let label = if spec.label.is_empty() {
            spec.id.as_str()
        } else {
            spec.label.as_str()
        };
        let message = if spec.message_template.is_empty() {
            format!(
                "[{label}] constraint '{}' violated by entity {subject_eid}",
                spec.id
            )
        } else {
            spec.message_template
                .replace("{subject}", &subject_eid.to_string())
                .replace("{spec_id}", &spec.id)
        };
        findings.push(Finding {
            id: FindingId(format!("{}:{subject_eid}", spec.id)),
            constraint_id: ConstraintId(spec.id.clone()),
            subject: subject_eid,
            severity: spec.severity.into(),
            message,
            rationale: spec.rationale.clone(),
            backlink: spec.backlink.as_ref().map(|b| PassageRef(b.clone())),
            emitted_at: now,
            role: ConstraintRole::Validation,
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::{ElementClassAssignment, ElementClassId};
    use crate::plugins::modeling::generic_factory::PrimitiveFactory;
    use crate::plugins::modeling::primitives::{BoxPrimitive, ShapeRotation};
    use crate::plugins::refinement::SemanticIntent;

    fn registry_with_box() -> CapabilityRegistry {
        let mut r = CapabilityRegistry::default();
        r.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        r
    }

    fn spawn_box(
        world: &mut World,
        eid: u64,
        role: &str,
        centre: Vec3,
        half: Vec3,
        state: Option<RefinementState>,
    ) -> Entity {
        let mut e = world.spawn((
            ElementId(eid),
            BoxPrimitive {
                centre,
                half_extents: half,
            },
            ShapeRotation(Quat::IDENTITY),
            ElementClassAssignment {
                element_class: ElementClassId("test".into()),
                active_recipe: None,
            },
            SemanticIntent {
                parameters: serde_json::json!({ "component_role": role }),
                unresolved_decisions: Vec::new(),
                source_refs: Vec::new(),
            },
        ));
        if let Some(state) = state {
            e.insert(RefinementStateComponent { state });
        }
        e.id()
    }

    fn find_entity(world: &mut World, eid: u64) -> Entity {
        world
            .query::<(Entity, &ElementId)>()
            .iter(world)
            .find_map(|(e, id)| (id.0 == eid).then_some(e))
            .unwrap()
    }

    #[test]
    fn spec_deserializes_with_defaults() {
        let json = r#"{
            "id": "roof_bears_on_wall",
            "applicability": { "component_roles": ["roof_system"] },
            "predicate": { "predicate": "vertical_contact",
                           "support": { "component_roles": ["exterior_wall"] } }
        }"#;
        let spec: ConstraintSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.revision, 1);
        assert_eq!(spec.severity, RuleSeverity::Error);
        assert!(spec.is_active());
        match spec.predicate {
            Predicate::VerticalContact { max_gap_m, .. } => {
                assert!((max_gap_m - 0.05).abs() < 1e-9)
            }
            _ => panic!("wrong predicate"),
        }
    }

    #[test]
    fn status_deserializes_from_bare_string_and_blocked_object() {
        // Bare-string forms author the common cases.
        let active: ConstraintSpec = serde_json::from_str(
            r#"{ "id":"a", "predicate":{"predicate":"non_degenerate_bounds"}, "status":"active" }"#,
        )
        .unwrap();
        assert_eq!(active.status, SpecStatus::Active);
        let draft: ConstraintSpec = serde_json::from_str(
            r#"{ "id":"a", "predicate":{"predicate":"non_degenerate_bounds"}, "status":"draft" }"#,
        )
        .unwrap();
        assert_eq!(draft.status, SpecStatus::Draft);
        // Data-carrying variant authors as an object.
        let blocked: ConstraintSpec = serde_json::from_str(
            r#"{ "id":"a", "predicate":{"predicate":"non_degenerate_bounds"},
                 "status":{"blocked":{"reason":"awaiting review"}} }"#,
        )
        .unwrap();
        assert_eq!(
            blocked.status,
            SpecStatus::Blocked {
                reason: "awaiting review".into()
            }
        );
    }

    #[test]
    fn extent_axis_within_deserializes() {
        let spec: ConstraintSpec = serde_json::from_str(
            r#"{
                "id": "exterior_wall_height_cap",
                "applicability": { "component_roles": ["exterior_wall"],
                                   "required_state_floor": "Constructible" },
                "predicate": { "predicate": "extent_axis_within", "axis": "y", "max_m": 3.0 },
                "severity": "error",
                "status": "active"
            }"#,
        )
        .unwrap();
        assert_eq!(
            spec.applicability.required_state_floor,
            Some(RefinementState::Constructible)
        );
        match spec.predicate {
            Predicate::ExtentAxisWithin { axis, max_m, min_m } => {
                assert_eq!(axis, Axis::Y);
                assert_eq!(max_m, Some(3.0));
                assert_eq!(min_m, None);
            }
            _ => panic!("wrong predicate"),
        }
    }

    #[test]
    fn non_degenerate_bounds_flags_degenerate_subject() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![ConstraintSpec {
            id: "must_render".into(),
            revision: 1,
            supersedes: None,
            label: "must render".into(),
            description: String::new(),
            rationale: String::new(),
            applicability: SpecApplicability::default(),
            predicate: Predicate::NonDegenerateBounds { min_extent_m: 1e-4 },
            severity: RuleSeverity::Error,
            message_template: String::new(),
            grounding_tag: None,
            backlink: None,
            status: SpecStatus::Active,
        }]));
        // Zero-extent box → degenerate.
        let e = spawn_box(&mut world, 1, "anything", Vec3::ZERO, Vec3::ZERO, None);
        let findings = run_declarative_constraints(e, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].constraint_id.0, "must_render");
    }

    #[test]
    fn non_degenerate_bounds_accepts_real_subject() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![ConstraintSpec {
            id: "must_render".into(),
            revision: 1,
            supersedes: None,
            label: String::new(),
            description: String::new(),
            rationale: String::new(),
            applicability: SpecApplicability::default(),
            predicate: Predicate::NonDegenerateBounds { min_extent_m: 1e-4 },
            severity: RuleSeverity::Error,
            message_template: String::new(),
            grounding_tag: None,
            backlink: None,
            status: SpecStatus::Active,
        }]));
        let e = spawn_box(
            &mut world,
            1,
            "anything",
            Vec3::ZERO,
            Vec3::splat(0.5),
            None,
        );
        assert!(run_declarative_constraints(e, &world).is_empty());
    }

    fn load_path_spec() -> ConstraintSpec {
        ConstraintSpec {
            id: "roof_bears_on_wall".into(),
            revision: 1,
            supersedes: None,
            label: "roof must bear on a wall".into(),
            description: String::new(),
            rationale: "erection order: roof rests on walls".into(),
            applicability: SpecApplicability {
                selector: EntitySelector {
                    element_classes: vec![],
                    component_roles: vec!["roof_system".into()],
                },
                required_state_floor: Some(RefinementState::Schematic),
            },
            predicate: Predicate::VerticalContact {
                support: EntitySelector {
                    element_classes: vec![],
                    component_roles: vec!["exterior_wall".into()],
                },
                max_gap_m: 0.05,
            },
            severity: RuleSeverity::Error,
            message_template: "roof {subject} floats above the walls".into(),
            grounding_tag: Some("policy-backed".into()),
            backlink: Some("VERTICAL_LOAD_PATH_AND_ERECTION_ORDER_2026".into()),
            status: SpecStatus::Active,
        }
    }

    #[test]
    fn vertical_contact_flags_floating_roof() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![load_path_spec()]));
        // Wall: top at y=2.4.
        spawn_box(
            &mut world,
            1,
            "exterior_wall",
            Vec3::new(0.0, 1.2, 0.0),
            Vec3::new(3.0, 1.2, 3.0),
            Some(RefinementState::Schematic),
        );
        // Roof: underside at y=2.9 → floats 0.5 m above the wall top.
        let roof = spawn_box(
            &mut world,
            2,
            "roof_system",
            Vec3::new(0.0, 3.15, 0.0),
            Vec3::new(3.0, 0.25, 3.0),
            Some(RefinementState::Schematic),
        );
        let findings = run_declarative_constraints(roof, &world);
        assert_eq!(findings.len(), 1, "floating roof must be flagged");
        assert_eq!(findings[0].subject, 2);
        assert!(findings[0].message.contains("floats"));
    }

    #[test]
    fn vertical_contact_accepts_seated_roof() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![load_path_spec()]));
        spawn_box(
            &mut world,
            1,
            "exterior_wall",
            Vec3::new(0.0, 1.2, 0.0),
            Vec3::new(3.0, 1.2, 3.0),
            Some(RefinementState::Schematic),
        );
        // Roof underside flush at the wall top (y=2.4): centre 2.65, half 0.25.
        let roof = spawn_box(
            &mut world,
            2,
            "roof_system",
            Vec3::new(0.0, 2.65, 0.0),
            Vec3::new(3.0, 0.25, 3.0),
            Some(RefinementState::Schematic),
        );
        assert!(
            run_declarative_constraints(roof, &world).is_empty(),
            "seated roof must not be flagged"
        );
    }

    #[test]
    fn conceptual_subject_is_exempt_from_state_floored_spec() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![load_path_spec()]));
        // A lone Conceptual roof with no wall: would float, but the floor exempts it.
        let roof = spawn_box(
            &mut world,
            2,
            "roof_system",
            Vec3::new(0.0, 3.15, 0.0),
            Vec3::new(3.0, 0.25, 3.0),
            Some(RefinementState::Conceptual),
        );
        assert!(run_declarative_constraints(roof, &world).is_empty());
    }

    #[test]
    fn non_applicable_subject_is_ignored() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![load_path_spec()]));
        // A wall (not a roof) is not the subject of the roof rule.
        let wall = spawn_box(
            &mut world,
            1,
            "exterior_wall",
            Vec3::new(0.0, 1.2, 0.0),
            Vec3::new(3.0, 1.2, 3.0),
            Some(RefinementState::Schematic),
        );
        assert!(run_declarative_constraints(wall, &world).is_empty());
    }

    #[test]
    fn draft_spec_is_not_evaluated() {
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        let mut spec = load_path_spec();
        spec.status = SpecStatus::Draft;
        world.insert_resource(ConstraintSpecRegistry::with_specs(vec![spec]));
        let roof = spawn_box(
            &mut world,
            2,
            "roof_system",
            Vec3::new(0.0, 3.15, 0.0),
            Vec3::new(3.0, 0.25, 3.0),
            Some(RefinementState::Schematic),
        );
        assert!(run_declarative_constraints(roof, &world).is_empty());
    }

    #[test]
    fn upsert_keeps_newest_revision() {
        let mut reg = ConstraintSpecRegistry::default();
        let mut v1 = load_path_spec();
        v1.revision = 1;
        assert!(reg.upsert(v1));
        let mut v2 = load_path_spec();
        v2.revision = 2;
        v2.label = "tightened".into();
        assert!(reg.upsert(v2));
        assert_eq!(reg.specs.len(), 1);
        assert_eq!(reg.specs[0].revision, 2);
        assert_eq!(reg.specs[0].label, "tightened");
        // An older revision is rejected.
        let mut v1b = load_path_spec();
        v1b.revision = 1;
        assert!(!reg.upsert(v1b));
        assert_eq!(reg.specs[0].revision, 2);
    }

    #[test]
    fn find_entity_helper_compiles() {
        // Guards against unused-helper warnings if other tests change.
        let mut world = World::new();
        world.insert_resource(registry_with_box());
        let e = spawn_box(&mut world, 7, "x", Vec3::ZERO, Vec3::splat(0.5), None);
        assert_eq!(find_entity(&mut world, 7), e);
    }
}
