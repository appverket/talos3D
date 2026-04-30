//! Proof Point 74 — Constraint orchestration engine.
//!
//! This module provides:
//! - The `Findings` resource: the cached, index-keyed store of all validator
//!   output.
//! - `validation_sweep_system`: a Bevy `PostUpdate` system that iterates
//!   registered `ConstraintDescriptor`s, applies each to matching entities,
//!   and writes results into `Findings`.
//!
//! The PP70 starter validator (`DeclaredStateRequiresResolvedObligations`) is
//! migrated here as a `ConstraintDescriptor` and registered via the standard
//! path. Any code that previously called `validate_declared_state_obligations`
//! directly should now read from `Findings` after the sweep runs.
//!
//! ## Dirty propagation
//!
//! The first implementation does a **full sweep** on every `PostUpdate` tick.
//! Change-detection via `Changed<T>` filters is a follow-on.
//! TODO(F4 follow-on): replace full sweep with per-(constraint × entity) dirty
//! tracking driven by `Changed<ObligationSet>` / `Changed<RefinementStateComponent>`.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::capability_registry::{
    Applicability, CapabilityRegistry, ConstraintDescriptor, ConstraintId, ConstraintRole,
    ElementClassAssignment, ElementClassId, Finding, FindingId,
};
use crate::plugins::{
    identity::ElementId,
    refinement::{
        ObligationSet, ObligationStatus, RefinementState, RefinementStateComponent, SemanticRole,
    },
};

// ---------------------------------------------------------------------------
// Findings resource
// ---------------------------------------------------------------------------

/// Cached validator output for the whole model.
///
/// Keys: `(ConstraintId, subject_element_id as u64)` → `Vec<Finding>`.
/// A flat `index: HashMap<FindingId, Finding>` supports O(1) `explain_finding`
/// lookup by `FindingId`.
///
/// The sweep system replaces the entire per-`(constraint, entity)` entry on
/// every run — it does not merge.
#[derive(Resource, Default)]
pub struct Findings {
    /// Primary cache keyed by `(constraint_id, element_id)`.
    pub cache: HashMap<(ConstraintId, u64), Vec<Finding>>,
    /// Flat index by `FindingId` for `explain_finding`.
    pub index: HashMap<FindingId, Finding>,
    /// Incremented each time the sweep finishes. Callers can use this to
    /// detect staleness.
    pub sweep_generation: u64,
}

impl Findings {
    /// Return all findings for a specific entity (across all constraints).
    pub fn for_entity(&self, element_id: u64) -> Vec<&Finding> {
        self.cache
            .iter()
            .filter(|((_, eid), _)| *eid == element_id)
            .flat_map(|(_, findings)| findings.iter())
            .collect()
    }

    /// Return all findings (across all entities and constraints).
    pub fn all(&self) -> impl Iterator<Item = &Finding> {
        self.cache.values().flat_map(|v| v.iter())
    }

    /// Return findings of a specific role across all entities.
    pub fn by_role(&self, role: ConstraintRole) -> impl Iterator<Item = &Finding> {
        self.cache
            .values()
            .flat_map(|v| v.iter())
            .filter(move |f| f.role == role)
    }

    /// Return findings of a specific role for a specific entity.
    pub fn for_entity_by_role(&self, element_id: u64, role: ConstraintRole) -> Vec<&Finding> {
        self.cache
            .iter()
            .filter(|((_, eid), _)| *eid == element_id)
            .flat_map(|(_, findings)| findings.iter())
            .filter(|f| f.role == role)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Discovery findings session budget (ADR-042 §13)
// ---------------------------------------------------------------------------

/// Per-session budget for `Discovery`-role findings.
///
/// Discovery findings are user-facing prompts. Without a cap they would
/// flood the assistant lane every sweep. The validation sweep system
/// resets `emitted_this_sweep` at the top of each sweep, then increments
/// it as Discovery findings are admitted; emissions beyond
/// `max_per_sweep` are dropped from the cache (and tallied in
/// `suppressed_this_sweep` for diagnostics).
///
/// `max_per_sweep` defaults to 8 — picked as a tractable batch for an
/// agent to address before re-running. Tooling may override at startup.
#[derive(Resource, Debug, Clone)]
pub struct DiscoveryFindingsBudget {
    pub max_per_sweep: u32,
    pub emitted_this_sweep: u32,
    pub suppressed_this_sweep: u32,
}

impl Default for DiscoveryFindingsBudget {
    fn default() -> Self {
        Self {
            max_per_sweep: 8,
            emitted_this_sweep: 0,
            suppressed_this_sweep: 0,
        }
    }
}

impl DiscoveryFindingsBudget {
    pub fn with_capacity(max_per_sweep: u32) -> Self {
        Self {
            max_per_sweep,
            emitted_this_sweep: 0,
            suppressed_this_sweep: 0,
        }
    }

    fn reset_sweep(&mut self) {
        self.emitted_this_sweep = 0;
        self.suppressed_this_sweep = 0;
    }

    /// Check-and-take one budget unit. Returns `true` if a finding can
    /// be admitted, `false` if the budget is exhausted.
    fn take(&mut self) -> bool {
        if self.emitted_this_sweep < self.max_per_sweep {
            self.emitted_this_sweep += 1;
            true
        } else {
            self.suppressed_this_sweep += 1;
            false
        }
    }

    pub fn remaining(&self) -> u32 {
        self.max_per_sweep
            .saturating_sub(self.emitted_this_sweep)
    }
}

// ---------------------------------------------------------------------------
// Promotion-finding query helper
// ---------------------------------------------------------------------------

/// Returns true when the entity has any `Promotion`-role finding with
/// `severity >= Warning`. Used by the refinement-state advance command
/// (ADR-042 §13) to gate movement into a higher state.
///
/// Callers must run `validation_sweep_system` before consulting this so
/// the cache is current.
pub fn entity_has_unresolved_promotion_findings(world: &World, element_id: u64) -> bool {
    use crate::capability_registry::Severity;
    let Some(findings) = world.get_resource::<Findings>() else {
        return false;
    };
    findings
        .for_entity_by_role(element_id, ConstraintRole::Promotion)
        .into_iter()
        .any(|f| matches!(f.severity, Severity::Warning | Severity::Error))
}

// ---------------------------------------------------------------------------
// Validation sweep system
// ---------------------------------------------------------------------------

/// Full-sweep validator system. Runs in `PostUpdate` after command application.
///
/// For each registered `ConstraintDescriptor`:
/// 1. Iterate all entities with an `ElementId`.
/// 2. Check applicability (element class + required refinement state).
/// 3. Run the validator function.
/// 4. Write results into `Findings`.
///
/// TODO(F4 follow-on): replace with dirty-tracking sweep using
/// `Changed<ObligationSet>` and `Changed<RefinementStateComponent>`.
pub fn validation_sweep_system(world: &mut World) {
    // Collect the constraint descriptors first (cloning the Arcs) so we can
    // release the `CapabilityRegistry` borrow before running validators that
    // read arbitrary world components. Role is captured here so the sweep
    // can canonicalize each finding's role and apply the Discovery budget.
    let constraints: Vec<(
        ConstraintId,
        Applicability,
        ConstraintRole,
        crate::capability_registry::ValidatorFn,
    )> = {
        let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
            return;
        };
        registry
            .constraint_descriptors()
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.applicability.clone(),
                    c.role,
                    c.validator.clone(),
                )
            })
            .collect()
    };

    // Reset the per-sweep Discovery budget. If the resource isn't
    // present, install the default (cap = 8). Done before running
    // validators so individual validators can also peek at the
    // resource if needed.
    if !world.contains_resource::<DiscoveryFindingsBudget>() {
        world.insert_resource(DiscoveryFindingsBudget::default());
    }
    {
        let mut budget = world.resource_mut::<DiscoveryFindingsBudget>();
        budget.reset_sweep();
    }

    // Collect (entity, element_id, element_class_id, refinement_state) tuples.
    // Use try_query with a required ElementId and optional secondary components.
    // try_query returns None only when a required component is entirely absent
    // from the world's component registry; using with_id(EntityId) avoids that.
    //
    // Strategy: iterate all live entities via the sparse-set EntityRef approach,
    // then call world.get::<T>(entity) for optional components. This is safe
    // because world.get() returns None when the component is unregistered.
    let all_entities: Vec<Entity> = {
        // Collect live entity ids by querying with EntityRef — which is always
        // available — and checking for ElementId presence explicitly.
        let mut q = world.query::<Entity>();
        q.iter(world).collect()
    };

    let entity_info: Vec<(Entity, u64, Option<ElementClassId>, RefinementState)> = all_entities
        .into_iter()
        .filter_map(|entity| {
            // Skip entities without an ElementId component.
            let eid = world.get::<ElementId>(entity)?;
            let class = world
                .get::<ElementClassAssignment>(entity)
                .map(|c| c.element_class.clone());
            let state = world
                .get::<RefinementStateComponent>(entity)
                .map(|s| s.state)
                .unwrap_or_default();
            Some((entity, eid.0, class, state))
        })
        .collect();

    // Run each constraint over applicable entities and accumulate findings.
    let mut new_cache: HashMap<(ConstraintId, u64), Vec<Finding>> = HashMap::new();
    let mut new_index: HashMap<FindingId, Finding> = HashMap::new();

    for (constraint_id, applicability, role, validator_fn) in &constraints {
        for (entity, element_id, class_id, state) in &entity_info {
            if !is_applicable(applicability, class_id.as_ref(), *state) {
                continue;
            }

            let raw_findings = validator_fn(*entity, world);

            // Canonicalize role from the descriptor (validators may set
            // any value at construction time; the sweep is the source
            // of truth) and apply the Discovery budget.
            let mut admitted: Vec<Finding> = Vec::with_capacity(raw_findings.len());
            for mut f in raw_findings {
                f.role = *role;
                if *role == ConstraintRole::Discovery {
                    let mut budget = world.resource_mut::<DiscoveryFindingsBudget>();
                    if !budget.take() {
                        // Budget exhausted; drop the finding silently
                        // beyond the suppressed_this_sweep counter.
                        continue;
                    }
                }
                admitted.push(f);
            }

            let key = (constraint_id.clone(), *element_id);
            for f in &admitted {
                new_index.insert(f.id.clone(), f.clone());
            }
            new_cache.insert(key, admitted);
        }
    }

    // Write results into the Findings resource (create if missing).
    if !world.contains_resource::<Findings>() {
        world.insert_resource(Findings::default());
    }
    let mut findings_res = world.resource_mut::<Findings>();
    findings_res.cache = new_cache;
    findings_res.index = new_index;
    findings_res.sweep_generation += 1;
}

/// Check whether an entity matches a constraint's `Applicability` filter.
fn is_applicable(
    applicability: &Applicability,
    class_id: Option<&ElementClassId>,
    state: RefinementState,
) -> bool {
    // Check class filter.
    if !applicability.element_classes.is_empty() {
        let Some(class) = class_id else {
            return false;
        };
        if !applicability.element_classes.contains(class) {
            return false;
        }
    }

    // Check state filter.
    if let Some(required) = applicability.required_state {
        if state < required {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// `DeclaredStateRequiresResolvedObligations` as a ConstraintDescriptor
// ---------------------------------------------------------------------------

/// Build the `DeclaredStateRequiresResolvedObligations` constraint descriptor.
///
/// This migrates the PP70 starter validator from the ad-hoc
/// `validate_declared_state_obligations` call path into the PP74 orchestration
/// engine. Behavior is unchanged.
///
/// Registered by `ValidationPlugin::build` and by integration tests that need
/// the completeness check.
pub fn declared_state_obligations_constraint() -> ConstraintDescriptor {
    use crate::capability_registry::Severity;
    use std::sync::Arc;

    ConstraintDescriptor {
        id: ConstraintId("DeclaredStateRequiresResolvedObligations".into()),
        label: "Declared State Requires Resolved Obligations".into(),
        description:
            "Entities at Schematic or higher must have obligations resolved per the severity \
             ladder: primary-structure obligations at Schematic → warning; all obligations \
             at Constructible or higher → error."
                .into(),
        applicability: Applicability {
            element_classes: Vec::new(), // applies to any class that has obligations
            required_state: None,        // the validator handles Conceptual → no-op internally
        },
        default_severity: Severity::Error,
        rationale:
            "Entities at Schematic state must have primary-structure obligations resolved or at \
             least flagged. At Constructible or higher, all obligations must be in a terminal \
             status (SatisfiedBy, Deferred, or Waived). This ensures that design intent is \
             captured before geometry generation proceeds."
                .into(),
        source_backlink: None, // PP77 fills in BBR-backed backlinks
        role: ConstraintRole::Validation,
        validator: Arc::new(run_declared_state_obligations),
    }
}

fn run_declared_state_obligations(subject: Entity, world: &World) -> Vec<Finding> {
    use crate::capability_registry::Severity;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let Some(state_comp) = world.get::<RefinementStateComponent>(subject) else {
        return Vec::new();
    };
    let state = state_comp.state;

    if state == RefinementState::Conceptual {
        return Vec::new();
    }

    let Some(eid) = world.get::<ElementId>(subject) else {
        return Vec::new();
    };
    let element_id = eid.0;

    let obligations = world
        .get::<ObligationSet>(subject)
        .cloned()
        .unwrap_or_default();

    let constraint_id = ConstraintId("DeclaredStateRequiresResolvedObligations".into());
    let mut findings = Vec::new();

    for obligation in &obligations.entries {
        if obligation.required_by_state > state {
            continue;
        }
        if !matches!(obligation.status, ObligationStatus::Unresolved) {
            continue;
        }

        let (severity, rationale) = match state {
            RefinementState::Conceptual => unreachable!("handled above"),
            RefinementState::Schematic => {
                if obligation.role == SemanticRole("primary_structure".into()) {
                    (
                        Severity::Warning,
                        "Primary-structure obligations must be resolved by the Schematic state \
                         to ensure structural intent is captured before further detail work begins.",
                    )
                } else {
                    (
                        Severity::Advice,
                        "This obligation is expected by the Schematic state. Consider resolving \
                         it before advancing further.",
                    )
                }
            }
            RefinementState::Constructible
            | RefinementState::Detailed
            | RefinementState::FabricationReady => (
                Severity::Error,
                "All obligations must be resolved at or above the Constructible state. \
                 Use SatisfiedBy, Deferred(reason), or Waived(rationale) to close this obligation.",
            ),
        };

        let finding_id = FindingId(format!(
            "DeclaredStateRequiresResolvedObligations:{}:{}",
            element_id, obligation.id.0
        ));

        findings.push(Finding {
            id: finding_id,
            constraint_id: constraint_id.clone(),
            subject: element_id,
            severity,
            message: format!(
                "Obligation '{}' (role: '{}', required by: {}) is Unresolved",
                obligation.id.0,
                obligation.role.0,
                obligation.required_by_state.as_str()
            ),
            rationale: rationale.to_string(),
            backlink: None,
            emitted_at: now,
            role: ConstraintRole::Validation,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// ValidationPlugin
// ---------------------------------------------------------------------------

/// Bevy plugin that adds the validation sweep system and initialises the
/// `Findings` resource.
///
/// Registers `DeclaredStateRequiresResolvedObligations` as a `ConstraintDescriptor`.
/// Domain plugins (e.g. `ArchitecturalPlugin`) register additional constraints
/// via `CapabilityRegistryAppExt::register_constraint` in their own `build`.
pub struct ValidationPlugin;

impl Plugin for ValidationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Findings>();
        app.init_resource::<DiscoveryFindingsBudget>();

        // Register the PP70 completeness validator through the new engine.
        if !app.world().contains_resource::<CapabilityRegistry>() {
            app.init_resource::<CapabilityRegistry>();
        }
        app.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(declared_state_obligations_constraint());

        app.add_systems(PostUpdate, validation_sweep_system);
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::{ConstraintDescriptor, Severity};
    use std::sync::Arc;

    fn make_world_with_resources() -> World {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(Findings::default());
        world
    }

    fn make_test_constraint(id: &str, class_filter: Option<&str>) -> ConstraintDescriptor {
        ConstraintDescriptor {
            id: ConstraintId(id.to_string()),
            label: id.to_string(),
            description: "test".into(),
            applicability: Applicability {
                element_classes: class_filter
                    .map(|c| vec![ElementClassId(c.to_string())])
                    .unwrap_or_default(),
                required_state: None,
            },
            default_severity: Severity::Error,
            rationale: "test rationale".into(),
            source_backlink: None,
            role: ConstraintRole::Validation,
            validator: Arc::new(move |_entity, _world| Vec::new()),
        }
    }

    #[test]
    fn register_constraint_and_retrieve() {
        let mut registry = CapabilityRegistry::default();
        let descriptor = make_test_constraint("test_constraint", None);
        registry.register_constraint(descriptor);

        assert_eq!(registry.constraint_descriptors().len(), 1);
        let found = registry.constraint_descriptor(&ConstraintId("test_constraint".into()));
        assert!(found.is_some());
        assert_eq!(found.unwrap().label, "test_constraint");
    }

    #[test]
    #[should_panic(expected = "Constraint 'test_constraint' was registered more than once")]
    fn register_duplicate_constraint_panics() {
        let mut registry = CapabilityRegistry::default();
        registry.register_constraint(make_test_constraint("test_constraint", None));
        registry.register_constraint(make_test_constraint("test_constraint", None));
    }

    #[test]
    fn findings_resource_inserts_and_reads() {
        let mut findings = Findings::default();
        let key = (ConstraintId("c1".into()), 42u64);
        let finding = Finding {
            id: FindingId("c1:42:ob1".into()),
            constraint_id: ConstraintId("c1".into()),
            subject: 42,
            severity: Severity::Error,
            message: "test".into(),
            rationale: "r".into(),
            backlink: None,
            emitted_at: 0,
            role: ConstraintRole::Validation,
        };
        findings.cache.insert(key, vec![finding.clone()]);
        findings
            .index
            .insert(FindingId("c1:42:ob1".into()), finding.clone());

        let for_42 = findings.for_entity(42);
        assert_eq!(for_42.len(), 1);
        assert_eq!(for_42[0].message, "test");

        assert!(findings.index.contains_key(&FindingId("c1:42:ob1".into())));
    }

    #[test]
    fn sweep_skips_non_applicable_entity() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());

        // Register a constraint that only applies to "wall_assembly".
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(false));
        let emitted_clone = emitted.clone();
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(ConstraintDescriptor {
                id: ConstraintId("wall_only".into()),
                label: "wall only".into(),
                description: "".into(),
                applicability: Applicability {
                    element_classes: vec![ElementClassId("wall_assembly".into())],
                    required_state: None,
                },
                default_severity: Severity::Error,
                rationale: "".into(),
                source_backlink: None,
                role: ConstraintRole::Validation,
                validator: Arc::new(move |_entity, _world| {
                    *emitted_clone
                        .lock()
                        .expect("constraint emission flag mutex poisoned") = true;
                    Vec::new()
                }),
            });

        // Spawn entity with no ElementClassAssignment (not a wall).
        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn((
            eid,
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
        ));

        validation_sweep_system(&mut world);

        assert!(
            !*emitted
                .lock()
                .expect("constraint emission flag mutex poisoned"),
            "Validator must not run on non-applicable entity"
        );
    }

    #[test]
    fn sweep_runs_on_applicable_entity() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());

        let called = Arc::new(std::sync::Mutex::new(false));
        let called_clone = called.clone();

        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(ConstraintDescriptor {
                id: ConstraintId("any_entity".into()),
                label: "any entity".into(),
                description: "".into(),
                applicability: Applicability::any(),
                default_severity: Severity::Advice,
                rationale: "".into(),
                source_backlink: None,
                role: ConstraintRole::Validation,
                validator: Arc::new(move |_entity, _world| {
                    *called_clone
                        .lock()
                        .expect("validator call flag mutex poisoned") = true;
                    Vec::new()
                }),
            });

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        assert!(
            *called
                .lock()
                .expect("validator call flag mutex poisoned"),
            "Validator must run on any entity"
        );
    }

    #[test]
    fn sweep_increments_generation() {
        let mut world = make_world_with_resources();
        world.insert_resource(crate::plugins::identity::ElementIdAllocator::default());

        validation_sweep_system(&mut world);
        let gen1 = world.resource::<Findings>().sweep_generation;
        validation_sweep_system(&mut world);
        let gen2 = world.resource::<Findings>().sweep_generation;

        assert_eq!(gen2, gen1 + 1);
    }

    fn make_role_test_constraint(
        id: &str,
        role: ConstraintRole,
        emit_count: usize,
    ) -> ConstraintDescriptor {
        let id_owned: String = id.to_string();
        ConstraintDescriptor {
            id: ConstraintId(id_owned.clone()),
            label: id_owned.clone(),
            description: "test".into(),
            applicability: Applicability::any(),
            default_severity: Severity::Warning,
            rationale: "test".into(),
            source_backlink: None,
            role,
            validator: Arc::new(move |entity, world| {
                let mut out = Vec::with_capacity(emit_count);
                let now = 0i64;
                let subject = world.get::<ElementId>(entity).map(|e| e.0).unwrap_or(0);
                for i in 0..emit_count {
                    out.push(Finding {
                        id: FindingId(format!("{id_owned}:{i}")),
                        constraint_id: ConstraintId(id_owned.clone()),
                        subject,
                        severity: Severity::Warning,
                        message: format!("finding {i}"),
                        rationale: "r".into(),
                        backlink: None,
                        emitted_at: now,
                        // The sweep canonicalizes role from the descriptor;
                        // any value here is fine.
                        role: ConstraintRole::Validation,
                    });
                }
                out
            }),
        }
    }

    #[test]
    fn constraint_role_default_is_validation() {
        assert_eq!(ConstraintRole::default(), ConstraintRole::Validation);
    }

    #[test]
    fn finding_role_round_trips_with_default() {
        // Round-trip via a synthesized Finding with all fields, then strip
        // `role` from the serialized JSON to simulate a legacy payload.
        let f0 = Finding {
            id: FindingId("x".into()),
            constraint_id: ConstraintId("c".into()),
            subject: 1,
            severity: Severity::Warning,
            message: "m".into(),
            rationale: "r".into(),
            backlink: None,
            emitted_at: 0,
            role: ConstraintRole::Promotion,
        };
        let json_full = serde_json::to_string(&f0).unwrap();
        let parsed_full: Finding = serde_json::from_str(&json_full).unwrap();
        assert_eq!(parsed_full.role, ConstraintRole::Promotion);

        // Strip the `role` field to simulate a pre-PP-D payload.
        let mut value: serde_json::Value = serde_json::from_str(&json_full).unwrap();
        value.as_object_mut().unwrap().remove("role");
        let legacy_json = serde_json::to_string(&value).unwrap();
        let parsed_legacy: Finding = serde_json::from_str(&legacy_json).unwrap();
        assert_eq!(
            parsed_legacy.role,
            ConstraintRole::Validation,
            "missing role defaults to Validation"
        );
    }

    #[test]
    fn sweep_canonicalizes_role_from_descriptor() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(DiscoveryFindingsBudget::with_capacity(64));
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(make_role_test_constraint(
                "promo",
                ConstraintRole::Promotion,
                1,
            ));

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        let findings = world.resource::<Findings>();
        let promo: Vec<&Finding> = findings.by_role(ConstraintRole::Promotion).collect();
        assert_eq!(promo.len(), 1, "expected one Promotion finding");
        assert_eq!(promo[0].role, ConstraintRole::Promotion);
    }

    #[test]
    fn discovery_budget_caps_emissions_per_sweep() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());
        // Cap at 2 so we can demonstrate truncation.
        world.insert_resource(DiscoveryFindingsBudget::with_capacity(2));
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(make_role_test_constraint(
                "discover",
                ConstraintRole::Discovery,
                5,
            ));

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        let findings = world.resource::<Findings>();
        let discoveries: Vec<&Finding> =
            findings.by_role(ConstraintRole::Discovery).collect();
        assert_eq!(discoveries.len(), 2, "budget caps Discovery emissions");

        let budget = world.resource::<DiscoveryFindingsBudget>();
        assert_eq!(budget.emitted_this_sweep, 2);
        assert_eq!(budget.suppressed_this_sweep, 3);
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn discovery_budget_resets_each_sweep() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(DiscoveryFindingsBudget::with_capacity(2));
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(make_role_test_constraint(
                "discover",
                ConstraintRole::Discovery,
                5,
            ));

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);
        validation_sweep_system(&mut world);

        let budget = world.resource::<DiscoveryFindingsBudget>();
        assert_eq!(
            budget.emitted_this_sweep, 2,
            "budget reset before second sweep"
        );
        assert_eq!(budget.suppressed_this_sweep, 3);
    }

    #[test]
    fn validation_findings_are_not_constrained_by_discovery_budget() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(DiscoveryFindingsBudget::with_capacity(1));
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(make_role_test_constraint(
                "validate",
                ConstraintRole::Validation,
                10,
            ));

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        let findings = world.resource::<Findings>();
        let validations: Vec<&Finding> =
            findings.by_role(ConstraintRole::Validation).collect();
        assert_eq!(
            validations.len(),
            10,
            "Validation findings unaffected by Discovery budget"
        );
    }

    #[test]
    fn promotion_findings_block_when_severity_at_or_above_warning() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(DiscoveryFindingsBudget::default());
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(make_role_test_constraint(
                "promotion_block",
                ConstraintRole::Promotion,
                1,
            ));

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        assert!(
            entity_has_unresolved_promotion_findings(&world, eid.0),
            "Warning-level Promotion finding must block"
        );
    }

    #[test]
    fn entity_has_unresolved_promotion_findings_returns_false_with_only_validation_findings() {
        use crate::plugins::identity::ElementIdAllocator;

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(DiscoveryFindingsBudget::default());
        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(make_role_test_constraint(
                "validate",
                ConstraintRole::Validation,
                3,
            ));

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        assert!(
            !entity_has_unresolved_promotion_findings(&world, eid.0),
            "no promotion finding ⇒ promotion not blocked"
        );
    }

    #[test]
    fn discovery_budget_with_capacity_sets_max() {
        let b = DiscoveryFindingsBudget::with_capacity(42);
        assert_eq!(b.max_per_sweep, 42);
        assert_eq!(b.remaining(), 42);
    }

    #[test]
    fn declared_state_obligations_constraint_emits_for_constructible_unresolved() {
        use crate::plugins::identity::ElementIdAllocator;
        use crate::plugins::refinement::{
            Obligation, ObligationId, ObligationSet, ObligationStatus,
        };

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());

        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(declared_state_obligations_constraint());

        let allocator = world.resource::<ElementIdAllocator>();
        let eid = allocator.next_id();
        world.spawn((
            eid,
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
            ObligationSet {
                entries: vec![Obligation {
                    id: ObligationId("structure".into()),
                    role: SemanticRole("primary_structure".into()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Unresolved,
                }],
            },
        ));

        validation_sweep_system(&mut world);

        let findings = world.resource::<Findings>();
        let key = (
            ConstraintId("DeclaredStateRequiresResolvedObligations".into()),
            eid.0,
        );
        let bucket = findings.cache.get(&key).expect("findings for entity");
        assert_eq!(bucket.len(), 1);
        assert_eq!(bucket[0].severity, Severity::Error);
    }
}
