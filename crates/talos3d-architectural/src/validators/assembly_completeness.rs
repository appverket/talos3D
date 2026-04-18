//! `AssemblyCompleteness` constraint validator (PP74).
//!
//! Generalises the PP70 starter `DeclaredStateRequiresResolvedObligations`
//! validator. The core engine already registers that constraint centrally
//! (via `ValidationPlugin`), so this module provides a richer
//! *architectural-domain* variant that:
//!
//! - Scopes to the three architectural element classes (`wall_assembly`,
//!   `foundation_system`, `roof_system`) rather than all entities.
//! - Runs the same obligation-walk logic as the core validator but is
//!   registered as a separate constraint so it can carry architectural-
//!   specific rationale.
//!
//! Since the logic is identical to the core validator, this module delegates
//! to the shared `validation.rs` implementation rather than duplicating it.
//! A PP77 follow-on can extend this with BBR regulatory references.

use std::sync::Arc;

use bevy::prelude::*;
use talos3d_core::{
    capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, ElementClassId, Finding, FindingId,
        Severity,
    },
    plugins::{
        identity::ElementId,
        refinement::{
            ObligationSet, ObligationStatus, RefinementState, RefinementStateComponent, SemanticRole,
        },
    },
};

/// Build the `ArchitecturalAssemblyCompleteness` `ConstraintDescriptor`.
///
/// Applies to all three architectural element classes. Runs the same
/// obligation-walk rules as `DeclaredStateRequiresResolvedObligations` but is
/// scoped to the architectural domain and carries architectural rationale.
pub fn assembly_completeness_constraint() -> ConstraintDescriptor {
    ConstraintDescriptor {
        id: ConstraintId("ArchitecturalAssemblyCompleteness".into()),
        label: "Architectural Assembly Completeness".into(),
        description:
            "Architectural assemblies (wall, foundation, roof) must have all obligations resolved \
             at or above the declared refinement state. Same severity ladder as ADR-038 §11: \
             primary-structure at Schematic → warning; all at Constructible → error."
                .into(),
        applicability: Applicability {
            element_classes: vec![
                ElementClassId("wall_assembly".into()),
                ElementClassId("foundation_system".into()),
                ElementClassId("roof_system".into()),
            ],
            required_state: None, // validator handles Conceptual → no-op internally
        },
        default_severity: Severity::Error,
        rationale:
            "Architectural assemblies must have fully resolved obligations before advancing to \
             Constructible. Unresolved obligations indicate incomplete design decisions that \
             will cause downstream errors in construction documents and fabrication."
                .into(),
        source_backlink: None, // PP77 fills in BBR backlinks
        validator: Arc::new(run_assembly_completeness),
    }
}

fn run_assembly_completeness(subject: Entity, world: &World) -> Vec<Finding> {
    let now = now_secs();

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

    let constraint_id = ConstraintId("ArchitecturalAssemblyCompleteness".into());
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
                        "Primary-structure obligations on architectural assemblies must be \
                         resolved by Schematic to ensure structural intent is captured before \
                         spatial coordination.",
                    )
                } else {
                    (
                        Severity::Advice,
                        "This obligation is expected at Schematic for architectural assemblies. \
                         Resolve before advancing to Constructible.",
                    )
                }
            }
            RefinementState::Constructible
            | RefinementState::Detailed
            | RefinementState::FabricationReady => (
                Severity::Error,
                "All obligations on architectural assemblies must be resolved at Constructible \
                 or higher. Use SatisfiedBy, Deferred(reason), or Waived(rationale).",
            ),
        };

        let finding_id = FindingId(format!(
            "ArchitecturalAssemblyCompleteness:{}:{}",
            element_id, obligation.id.0
        ));

        findings.push(Finding {
            id: finding_id,
            constraint_id: constraint_id.clone(),
            subject: element_id,
            severity,
            message: format!(
                "Obligation '{}' (role: '{}', required by: {}) is Unresolved on architectural \
                 assembly {}",
                obligation.id.0,
                obligation.role.0,
                obligation.required_by_state.as_str(),
                element_id,
            ),
            rationale: rationale.to_string(),
            backlink: None,
            emitted_at: now,
        });
    }

    findings
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use talos3d_core::{
        capability_registry::ElementClassAssignment,
        plugins::{
            identity::ElementIdAllocator,
            refinement::{Obligation, ObligationId, ObligationSet, ObligationStatus},
        },
    };

    fn make_world() -> World {
        let mut world = World::new();
        world.insert_resource(talos3d_core::capability_registry::CapabilityRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world
    }

    fn make_entity_with_obligation(
        world: &mut World,
        class: &str,
        state: RefinementState,
        obligation_status: ObligationStatus,
    ) -> Entity {
        use talos3d_core::capability_registry::ElementClassId;

        let eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((
            eid,
            ElementClassAssignment {
                element_class: ElementClassId(class.to_string()),
                active_recipe: None,
            },
            RefinementStateComponent { state },
            ObligationSet {
                entries: vec![Obligation {
                    id: ObligationId("structure".into()),
                    role: SemanticRole("primary_structure".into()),
                    required_by_state: RefinementState::Constructible,
                    status: obligation_status,
                }],
            },
        )).id()
    }

    #[test]
    fn conceptual_emits_no_findings() {
        let mut world = make_world();
        let entity = make_entity_with_obligation(
            &mut world,
            "wall_assembly",
            RefinementState::Conceptual,
            ObligationStatus::Unresolved,
        );
        let findings = run_assembly_completeness(entity, &world);
        assert!(findings.is_empty());
    }

    #[test]
    fn constructible_with_unresolved_emits_error() {
        let mut world = make_world();
        let entity = make_entity_with_obligation(
            &mut world,
            "wall_assembly",
            RefinementState::Constructible,
            ObligationStatus::Unresolved,
        );
        let findings = run_assembly_completeness(entity, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn constructible_with_satisfied_emits_no_findings() {
        let mut world = make_world();
        let entity = make_entity_with_obligation(
            &mut world,
            "foundation_system",
            RefinementState::Constructible,
            ObligationStatus::SatisfiedBy(99),
        );
        let findings = run_assembly_completeness(entity, &world);
        assert!(findings.is_empty());
    }

    #[test]
    fn schematic_primary_structure_is_warning() {
        let mut world = make_world();
        let eid = world.resource::<ElementIdAllocator>().next_id();
        let entity = world.spawn((
            eid,
            talos3d_core::capability_registry::ElementClassAssignment {
                element_class: ElementClassId("roof_system".into()),
                active_recipe: None,
            },
            RefinementStateComponent {
                state: RefinementState::Schematic,
            },
            ObligationSet {
                entries: vec![Obligation {
                    id: ObligationId("structural_layer".into()),
                    role: SemanticRole("primary_structure".into()),
                    required_by_state: RefinementState::Schematic,
                    status: ObligationStatus::Unresolved,
                }],
            },
        )).id();

        let findings = run_assembly_completeness(entity, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }
}
