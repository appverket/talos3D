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
    Applicability, CapabilityRegistry, ConstraintDescriptor, ConstraintId, ElementClassAssignment,
    ElementClassId, Finding, FindingId,
};
use crate::plugins::{
    identity::ElementId,
    refinement::{
        ObligationSet, ObligationStatus, RefinementState, RefinementStateComponent,
        SemanticRole,
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
    // read arbitrary world components.
    let constraints: Vec<(
        ConstraintId,
        Applicability,
        crate::capability_registry::ValidatorFn,
    )> = {
        let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
            return;
        };
        registry
            .constraint_descriptors()
            .iter()
            .map(|c| (c.id.clone(), c.applicability.clone(), c.validator.clone()))
            .collect()
    };

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

    for (constraint_id, applicability, validator_fn) in &constraints {
        for (entity, element_id, class_id, state) in &entity_info {
            if !is_applicable(applicability, class_id.as_ref(), *state) {
                continue;
            }

            let findings = validator_fn(*entity, world);

            let key = (constraint_id.clone(), *element_id);
            for f in &findings {
                new_index.insert(f.id.clone(), f.clone());
            }
            new_cache.insert(key, findings);
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
        use crate::plugins::identity::{ElementId, ElementIdAllocator};

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());

        // Register a constraint that only applies to "wall_assembly".
        let mut emitted = std::sync::Arc::new(std::sync::Mutex::new(false));
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
                validator: Arc::new(move |_entity, _world| {
                    *emitted_clone.lock().unwrap() = true;
                    Vec::new()
                }),
            });

        // Spawn entity with no ElementClassAssignment (not a wall).
        let allocator = world.resource::<ElementIdAllocator>().clone();
        let eid = allocator.next_id();
        world.spawn((eid, RefinementStateComponent { state: RefinementState::Constructible }));

        validation_sweep_system(&mut world);

        assert!(
            !*emitted.lock().unwrap(),
            "Validator must not run on non-applicable entity"
        );
    }

    #[test]
    fn sweep_runs_on_applicable_entity() {
        use crate::plugins::identity::{ElementId, ElementIdAllocator};

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
                validator: Arc::new(move |_entity, _world| {
                    *called_clone.lock().unwrap() = true;
                    Vec::new()
                }),
            });

        let allocator = world.resource::<ElementIdAllocator>().clone();
        let eid = allocator.next_id();
        world.spawn(eid);

        validation_sweep_system(&mut world);

        assert!(*called.lock().unwrap(), "Validator must run on any entity");
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

    #[test]
    fn declared_state_obligations_constraint_emits_for_constructible_unresolved() {
        use crate::plugins::identity::{ElementId, ElementIdAllocator};
        use crate::plugins::refinement::{Obligation, ObligationId, ObligationSet, ObligationStatus};

        let mut world = make_world_with_resources();
        world.insert_resource(ElementIdAllocator::default());

        world
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(declared_state_obligations_constraint());

        let allocator = world.resource::<ElementIdAllocator>().clone();
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
