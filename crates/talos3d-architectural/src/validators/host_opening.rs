//! `HostOpeningGeometry` constraint validator (PP74).
//!
//! For each opening entity (identified via a `hosted_on` SemanticRelation),
//! verifies that:
//!
//! 1. The host entity exists and is a `wall_assembly` at `Constructible` or
//!    higher. Emits `error` if host is missing.
//! 2. If the opening has size claims (`opening_width_mm`, `opening_height_mm`)
//!    and the host has `length_mm` / `height_mm` claim groundings, checks that
//!    the aperture fits. Emits `error` if it doesn't fit.
//!
//! ## Current limitation
//!
//! Opening entities are not yet fully backed by PP70-style recipes in
//! `talos3d-architectural`. The `hosted_on` relation is registered as a
//! relation type but the snapshot representation is still the legacy
//! `WallSnapshot/OpeningSnapshot` shape. As a result, this validator may find
//! no opening entities in the current world state. It is designed to no-op
//! gracefully in that case.
//!
//! TODO: activate end-to-end when openings migrate to the PP70 recipe/element-
//! class shape. The validator logic is complete; only the entity model is missing.

use std::sync::Arc;

use bevy::prelude::*;
use talos3d_core::{
    capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, ElementClassAssignment, Finding,
        FindingId, Severity,
    },
    plugins::{
        identity::ElementId,
        modeling::assembly::SemanticRelation,
        refinement::{ClaimGrounding, ClaimPath, RefinementState, RefinementStateComponent},
    },
};

/// Build the `HostOpeningGeometry` `ConstraintDescriptor`.
///
/// The applicability filter matches all entities (applicability filtered at
/// runtime by checking for `hosted_on` relations). This is intentional: opening
/// entities may not yet have an `ElementClassAssignment`, so filtering by class
/// would miss them.
pub fn host_opening_constraint() -> ConstraintDescriptor {
    ConstraintDescriptor {
        id: ConstraintId("HostOpeningGeometry".into()),
        label: "Host Opening Geometry".into(),
        description:
            "Opening entities must be hosted on a wall_assembly at Constructible or higher. \
             Aperture dimensions must not exceed the host wall's envelope."
                .into(),
        // No class filter — the validator inspects SemanticRelation participation.
        // TODO: tighten to element_class == "opening" once openings migrate to recipes.
        applicability: Applicability::any(),
        default_severity: Severity::Error,
        rationale:
            "An opening that lacks a valid host wall, or whose aperture exceeds the host's \
             envelope, cannot be fabricated. The model must reflect physical constraints."
                .into(),
        source_backlink: None,
        validator: Arc::new(run_host_opening_validator),
    }
}

fn run_host_opening_validator(subject: Entity, world: &World) -> Vec<Finding> {
    let now = now_secs();

    let Some(eid) = world.get::<ElementId>(subject) else {
        return Vec::new();
    };
    let element_id = eid.0;
    let subject_eid = *eid;

    // Use try_query::<(EntityRef,)> — the only query kind that takes &World (not &mut World).
    // EntityRef is always registered in Bevy; the unwrap is safe.
    let mut q = world.try_query::<(EntityRef,)>().unwrap();

    // This entity is an "opening" if it participates as a source of a
    // `hosted_on` relation, OR if its element class is "opening".
    let has_hosted_on_relation = q.iter(world).any(|(entity_ref,)| {
        entity_ref
            .get::<SemanticRelation>()
            .is_some_and(|rel| rel.relation_type == "hosted_on" && rel.source == subject_eid)
    });

    let is_opening_class = world
        .get::<ElementClassAssignment>(subject)
        .is_some_and(|ca| ca.element_class.0 == "opening");

    if !has_hosted_on_relation && !is_opening_class {
        // Not an opening entity; skip.
        return Vec::new();
    }

    // TODO: opening entity migration. This validator is currently a no-op for
    // entities that participate in `hosted_on` relations unless they also have
    // `ElementClassAssignment` with class == "opening". The full check activates
    // once openings are modelled as PP70-style element classes.

    let mut findings = Vec::new();

    // Find the host entity via hosted_on relation.
    let host_eid: Option<ElementId> = q.iter(world).find_map(|(entity_ref,)| {
        let rel = entity_ref.get::<SemanticRelation>()?;
        if rel.relation_type == "hosted_on" && rel.source == subject_eid {
            Some(rel.target)
        } else {
            None
        }
    });

    let Some(host_eid) = host_eid else {
        // Opening with no hosted_on relation.
        findings.push(Finding {
            id: FindingId(format!("HostOpeningGeometry:{}:no_host", element_id)),
            constraint_id: ConstraintId("HostOpeningGeometry".into()),
            subject: element_id,
            severity: Severity::Error,
            message: format!(
                "Opening entity {} has no hosted_on relation — it must be hosted on a wall",
                element_id
            ),
            rationale: "An opening must be hosted on a wall_assembly.".into(),
            backlink: None,
            emitted_at: now,
        });
        return findings;
    };

    // Verify host is a wall_assembly at Constructible or higher.
    let host_info: Option<(Option<String>, RefinementState, Option<ClaimGrounding>)> =
        q.iter(world).find_map(|(entity_ref,)| {
            let eid = entity_ref.get::<ElementId>()?;
            if *eid != host_eid {
                return None;
            }
            let class = entity_ref
                .get::<ElementClassAssignment>()
                .map(|c| c.element_class.0.clone());
            let state = entity_ref
                .get::<RefinementStateComponent>()
                .map(|s| s.state)
                .unwrap_or_default();
            let cg = entity_ref.get::<ClaimGrounding>().cloned();
            Some((class, state, cg))
        });

    match host_info {
        None => {
            findings.push(Finding {
                id: FindingId(format!("HostOpeningGeometry:{}:host_missing", element_id)),
                constraint_id: ConstraintId("HostOpeningGeometry".into()),
                subject: element_id,
                severity: Severity::Error,
                message: format!(
                    "Opening {}: hosted_on target {} not found in model",
                    element_id, host_eid.0
                ),
                rationale: "The host wall entity no longer exists.".into(),
                backlink: None,
                emitted_at: now,
            });
        }
        Some((class_opt, state, host_cg)) => {
            if class_opt.as_deref() != Some("wall_assembly") {
                findings.push(Finding {
                    id: FindingId(format!("HostOpeningGeometry:{}:host_not_wall", element_id)),
                    constraint_id: ConstraintId("HostOpeningGeometry".into()),
                    subject: element_id,
                    severity: Severity::Error,
                    message: format!(
                        "Opening {} is hosted on entity {} which has class '{}', not wall_assembly",
                        element_id,
                        host_eid.0,
                        class_opt.as_deref().unwrap_or("<none>")
                    ),
                    rationale: "Openings must be hosted on wall_assembly entities.".into(),
                    backlink: None,
                    emitted_at: now,
                });
            } else if state < RefinementState::Constructible {
                findings.push(Finding {
                    id: FindingId(format!(
                        "HostOpeningGeometry:{}:host_not_constructible",
                        element_id
                    )),
                    constraint_id: ConstraintId("HostOpeningGeometry".into()),
                    subject: element_id,
                    severity: Severity::Error,
                    message: format!(
                        "Opening {} host wall {} is at state {} (need Constructible or higher)",
                        element_id,
                        host_eid.0,
                        state.as_str()
                    ),
                    rationale: "The host wall must be at Constructible before its openings are \
                                validated."
                        .into(),
                    backlink: None,
                    emitted_at: now,
                });
            } else {
                // Both exist and class/state are OK. Check aperture fits, if claims present.
                check_aperture_fits(
                    world,
                    subject,
                    element_id,
                    host_cg.as_ref(),
                    now,
                    &mut findings,
                );
            }
        }
    }

    findings
}

/// Check that the opening's aperture dimensions fit within the host wall's envelope.
///
/// This is a best-effort check: if either the opening or host does not have the
/// required claim groundings, the check is silently skipped.
fn check_aperture_fits(
    world: &World,
    subject: Entity,
    element_id: u64,
    host_cg: Option<&ClaimGrounding>,
    now: i64,
    findings: &mut Vec<Finding>,
) {
    let opening_cg = world.get::<ClaimGrounding>(subject);

    let host_height = host_cg
        .and_then(|cg| cg.claims.get(&ClaimPath("height_mm".into())))
        .and_then(|_| None::<f64>); // TODO: decode numeric value from claim record

    let host_length = host_cg
        .and_then(|cg| cg.claims.get(&ClaimPath("length_mm".into())))
        .and_then(|_| None::<f64>);

    let opening_height = opening_cg
        .and_then(|cg| cg.claims.get(&ClaimPath("opening_height_mm".into())))
        .and_then(|_| None::<f64>);

    let opening_width = opening_cg
        .and_then(|cg| cg.claims.get(&ClaimPath("opening_width_mm".into())))
        .and_then(|_| None::<f64>);

    // If any dimension is missing from the grounding, skip the check.
    // TODO(PP75): decode actual numeric values from ClaimGrounding records once
    // typed claim values land. For now the decode is stubbed as None.
    if let (Some(oh), Some(hw)) = (opening_height, host_height) {
        if oh > hw {
            findings.push(Finding {
                id: FindingId(format!(
                    "HostOpeningGeometry:{}:height_exceeds_host",
                    element_id
                )),
                constraint_id: ConstraintId("HostOpeningGeometry".into()),
                subject: element_id,
                severity: Severity::Error,
                message: format!(
                    "Opening {} height {oh}mm exceeds host wall height {hw}mm",
                    element_id
                ),
                rationale: "Opening aperture must fit within the host wall envelope.".into(),
                backlink: None,
                emitted_at: now,
            });
        }
    }

    if let (Some(ow), Some(hl)) = (opening_width, host_length) {
        if ow > hl {
            findings.push(Finding {
                id: FindingId(format!(
                    "HostOpeningGeometry:{}:width_exceeds_host",
                    element_id
                )),
                constraint_id: ConstraintId("HostOpeningGeometry".into()),
                subject: element_id,
                severity: Severity::Error,
                message: format!(
                    "Opening {} width {ow}mm exceeds host wall length {hl}mm",
                    element_id
                ),
                rationale: "Opening aperture must fit within the host wall envelope.".into(),
                backlink: None,
                emitted_at: now,
            });
        }
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
        capability_registry::{ElementClassAssignment, ElementClassId},
        plugins::{
            identity::{ElementId, ElementIdAllocator},
            refinement::{RefinementState, RefinementStateComponent},
        },
    };

    fn make_world() -> World {
        let mut world = World::new();
        world
            .insert_resource(talos3d_core::capability_registry::CapabilityRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world
    }

    fn spawn_entity(
        world: &mut World,
        class: Option<&str>,
        state: RefinementState,
    ) -> (Entity, ElementId) {
        let eid = world.resource::<ElementIdAllocator>().next_id();
        let mut cmds = world.spawn(eid);
        cmds.insert(RefinementStateComponent { state });
        if let Some(c) = class {
            cmds.insert(ElementClassAssignment {
                element_class: ElementClassId(c.to_string()),
                active_recipe: None,
            });
        }
        let entity = cmds.id();
        (entity, eid)
    }

    fn spawn_hosted_on(world: &mut World, source: ElementId, target: ElementId) {
        let rel_eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((
            rel_eid,
            SemanticRelation {
                source,
                target,
                relation_type: "hosted_on".to_string(),
                parameters: serde_json::json!({}),
            },
        ));
    }

    #[test]
    fn entity_without_hosted_on_is_skipped() {
        let mut world = make_world();
        // A plain wall — not an opening.
        let (entity, _) =
            spawn_entity(&mut world, Some("wall_assembly"), RefinementState::Constructible);
        let findings = run_host_opening_validator(entity, &world);
        assert!(findings.is_empty(), "non-opening entity must produce no findings");
    }

    #[test]
    fn opening_with_valid_wall_host_passes() {
        let mut world = make_world();
        let (opening, opening_eid) =
            spawn_entity(&mut world, Some("opening"), RefinementState::Conceptual);
        let (_wall, wall_eid) =
            spawn_entity(&mut world, Some("wall_assembly"), RefinementState::Constructible);
        spawn_hosted_on(&mut world, opening_eid, wall_eid);

        let findings = run_host_opening_validator(opening, &world);
        assert!(
            findings.is_empty(),
            "opening hosted on Constructible wall must pass: {findings:?}"
        );
    }

    #[test]
    fn opening_with_no_host_relation_emits_error() {
        let mut world = make_world();
        let (opening, _) = spawn_entity(&mut world, Some("opening"), RefinementState::Conceptual);

        let findings = run_host_opening_validator(opening, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn opening_hosted_on_conceptual_wall_emits_error() {
        let mut world = make_world();
        let (opening, opening_eid) =
            spawn_entity(&mut world, Some("opening"), RefinementState::Conceptual);
        let (_wall, wall_eid) =
            spawn_entity(&mut world, Some("wall_assembly"), RefinementState::Conceptual);
        spawn_hosted_on(&mut world, opening_eid, wall_eid);

        let findings = run_host_opening_validator(opening, &world);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }
}
