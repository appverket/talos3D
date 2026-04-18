//! `SupportPathIntegrity` constraint validator (PP74).
//!
//! For each `wall_assembly` entity at `Constructible` or higher, walks the
//! `bears_on` `SemanticRelation` chain until it reaches a `foundation_system`
//! at `Constructible` or higher. If the chain is broken (no relation, target
//! not Constructible, or cycle), emits an `error` finding.
//!
//! The same check is applied to `roof_system` entities: they must reach a
//! `wall_assembly` at `Constructible` or higher via `bears_on`.

use std::collections::HashSet;
use std::sync::Arc;

use bevy::prelude::*;
use talos3d_core::{
    capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, ElementClassAssignment, ElementClassId,
        Finding, FindingId, Severity,
    },
    plugins::{
        identity::ElementId,
        modeling::assembly::SemanticRelation,
        refinement::{RefinementState, RefinementStateComponent},
    },
};

/// Build the `SupportPathIntegrity` `ConstraintDescriptor`.
pub fn support_path_constraint() -> ConstraintDescriptor {
    ConstraintDescriptor {
        id: ConstraintId("SupportPathIntegrity".into()),
        label: "Support Path Integrity".into(),
        description:
            "Every wall assembly at Constructible must have a continuous bears_on chain \
             reaching a foundation system at Constructible or higher. Every roof system \
             at Constructible must reach a wall assembly at Constructible or higher via \
             bears_on."
                .into(),
        applicability: Applicability {
            element_classes: vec![
                ElementClassId("wall_assembly".into()),
                ElementClassId("roof_system".into()),
            ],
            required_state: Some(RefinementState::Constructible),
        },
        default_severity: Severity::Error,
        rationale:
            "Structural loads must trace an unbroken path to the foundation. A broken \
             support chain means the model does not represent a structurally sound \
             assembly and will produce invalid construction documents."
                .into(),
        source_backlink: None,
        validator: Arc::new(run_support_path_validator),
    }
}

fn run_support_path_validator(subject: Entity, world: &World) -> Vec<Finding> {
    let now = now_secs();

    let Some(eid) = world.get::<ElementId>(subject) else {
        return Vec::new();
    };
    let element_id = eid.0;
    let subject_eid = *eid;

    let Some(class_assignment) = world.get::<ElementClassAssignment>(subject) else {
        return Vec::new();
    };
    let class = class_assignment.element_class.0.as_str();

    let (chain_label, target_class) = match class {
        "wall_assembly" => ("bears_on chain from wall to foundation", "foundation_system"),
        "roof_system" => ("bears_on chain from roof to wall", "wall_assembly"),
        _ => return Vec::new(),
    };

    match walk_bears_on_chain(world, subject_eid, target_class) {
        ChainResult::Ok => Vec::new(),
        ChainResult::Broken(reason) => {
            vec![Finding {
                id: FindingId(format!("SupportPathIntegrity:{}:chain", element_id)),
                constraint_id: ConstraintId("SupportPathIntegrity".into()),
                subject: element_id,
                severity: Severity::Error,
                message: format!(
                    "Entity {} ({class}): {chain_label} is broken — {reason}",
                    element_id
                ),
                rationale:
                    "Structural loads must trace an unbroken path to the foundation. \
                     Ensure a bears_on relation exists and its target is at Constructible \
                     or higher."
                        .into(),
                backlink: None,
                emitted_at: now,
            }]
        }
    }
}

enum ChainResult {
    Ok,
    Broken(String),
}

/// Walk the `bears_on` relation from `start` until we reach an entity of
/// `target_class` at `Constructible` or higher.
///
/// Uses `world.try_query::<(EntityRef,)>()` which takes `&World` (not `&mut World`)
/// and returns `None` only if `EntityRef` is unregistered (never in practice).
fn walk_bears_on_chain(world: &World, start: ElementId, target_class: &str) -> ChainResult {
    let mut visited: HashSet<u64> = HashSet::new();
    let current = start;

    loop {
        if visited.contains(&current.0) {
            return ChainResult::Broken("cycle detected in bears_on chain".into());
        }
        visited.insert(current.0);

        // EntityRef is always registered; this unwrap is safe.
        let mut q = world.try_query::<(EntityRef,)>().unwrap();

        let target_eid: Option<ElementId> = q.iter(world).find_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type == "bears_on" && rel.source == current {
                Some(rel.target)
            } else {
                None
            }
        });

        let Some(target_eid) = target_eid else {
            return ChainResult::Broken(format!(
                "no bears_on relation from entity {}",
                current.0
            ));
        };

        let target_info: Option<(Option<String>, RefinementState)> =
            q.iter(world).find_map(|(entity_ref,)| {
                let eid = entity_ref.get::<ElementId>()?;
                if *eid != target_eid {
                    return None;
                }
                let class = entity_ref
                    .get::<ElementClassAssignment>()
                    .map(|c| c.element_class.0.clone());
                let state = entity_ref
                    .get::<RefinementStateComponent>()
                    .map(|s| s.state)
                    .unwrap_or_default();
                Some((class, state))
            });

        let Some((class_opt, state)) = target_info else {
            return ChainResult::Broken(format!(
                "bears_on target entity {} not found",
                target_eid.0
            ));
        };

        if class_opt.as_deref() == Some(target_class) {
            if state >= RefinementState::Constructible {
                return ChainResult::Ok;
            } else {
                return ChainResult::Broken(format!(
                    "bears_on target {} ({}) is at state {} (need Constructible or higher)",
                    target_eid.0,
                    target_class,
                    state.as_str()
                ));
            }
        }

        return ChainResult::Broken(format!(
            "bears_on target {} has class '{}', expected '{}'",
            target_eid.0,
            class_opt.as_deref().unwrap_or("<none>"),
            target_class
        ));
    }
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
        capability_registry::{CapabilityRegistry, ElementClassAssignment, ElementClassId},
        plugins::{
            identity::{ElementId, ElementIdAllocator},
            modeling::assembly::SemanticRelation,
            refinement::{RefinementState, RefinementStateComponent},
        },
    };

    fn make_world() -> World {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world
    }

    fn spawn_entity_with_class(
        world: &mut World,
        class: &str,
        state: RefinementState,
    ) -> (Entity, ElementId) {
        let eid = world.resource::<ElementIdAllocator>().next_id();
        let entity = world
            .spawn((
                eid,
                ElementClassAssignment {
                    element_class: ElementClassId(class.to_string()),
                    active_recipe: None,
                },
                RefinementStateComponent { state },
            ))
            .id();
        (entity, eid)
    }

    fn spawn_bears_on(world: &mut World, source: ElementId, target: ElementId) {
        let rel_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((
            rel_eid,
            SemanticRelation {
                source,
                target,
                relation_type: "bears_on".to_string(),
                parameters: serde_json::json!({}),
            },
        ));
    }

    #[test]
    fn wall_with_constructible_foundation_passes() {
        let mut world = make_world();
        let (wall_entity, wall_eid) =
            spawn_entity_with_class(&mut world, "wall_assembly", RefinementState::Constructible);
        let (_foundation_entity, foundation_eid) = spawn_entity_with_class(
            &mut world,
            "foundation_system",
            RefinementState::Constructible,
        );
        spawn_bears_on(&mut world, wall_eid, foundation_eid);

        let findings = run_support_path_validator(wall_entity, &world);
        assert!(
            findings.is_empty(),
            "wall bearing on Constructible foundation must pass: {findings:?}"
        );
    }

    #[test]
    fn wall_with_no_bears_on_fails() {
        let mut world = make_world();
        let (wall_entity, _wall_eid) =
            spawn_entity_with_class(&mut world, "wall_assembly", RefinementState::Constructible);

        let findings = run_support_path_validator(wall_entity, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("no bears_on relation"));
    }

    #[test]
    fn wall_with_conceptual_foundation_fails() {
        let mut world = make_world();
        let (wall_entity, wall_eid) =
            spawn_entity_with_class(&mut world, "wall_assembly", RefinementState::Constructible);
        let (_foundation_entity, foundation_eid) = spawn_entity_with_class(
            &mut world,
            "foundation_system",
            RefinementState::Conceptual,
        );
        spawn_bears_on(&mut world, wall_eid, foundation_eid);

        let findings = run_support_path_validator(wall_entity, &world);
        assert_eq!(findings.len(), 1, "conceptual foundation must fail");
    }

    #[test]
    fn roof_with_constructible_wall_passes() {
        let mut world = make_world();
        let (roof_entity, roof_eid) =
            spawn_entity_with_class(&mut world, "roof_system", RefinementState::Constructible);
        let (_wall_entity, wall_eid) =
            spawn_entity_with_class(&mut world, "wall_assembly", RefinementState::Constructible);
        spawn_bears_on(&mut world, roof_eid, wall_eid);

        let findings = run_support_path_validator(roof_entity, &world);
        assert!(
            findings.is_empty(),
            "roof bearing on Constructible wall must pass: {findings:?}"
        );
    }

    #[test]
    fn roof_with_no_bears_on_fails() {
        let mut world = make_world();
        let (roof_entity, _) =
            spawn_entity_with_class(&mut world, "roof_system", RefinementState::Constructible);

        let findings = run_support_path_validator(roof_entity, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }
}
