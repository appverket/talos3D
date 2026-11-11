//! Domain-neutral advice for reusable component structure.
//!
//! Advisory (never blocking) constraint that catches two common authoring
//! mistakes the guidance in
//! capability-owned authoring guidance warns against:
//!
//! 1. **Repeated same-role singletons.** Two or more children under the same
//!    refinement parent share an [`AuthoringRole`] but none share a
//!    `Definition` — a signal that they should collapse to one reusable
//!    `Definition` plus several `OccurrenceIdentity`s (Rule A).
//!
//! 2. **High override variance on a shared Definition.** A `Definition` whose
//!    `OccurrenceIdentity` entries override the same non-placement parameter
//!    well beyond a tolerance threshold — a signal that deriving a variant
//!    via `base_definition_id` would be cleaner than override-heavy
//!    occurrences (Rule B).
//!
//! Both checks emit [`Severity::Advice`] only in v1. The proposal (§7.1)
//! requires this: we do not yet have false-positive data, so findings must
//! not block `refine`, `promote`, or any publish step.

use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, ConstraintRole, Finding, FindingId,
        Severity,
    },
    plugins::{
        authoring_guidance::AuthoringRole,
        identity::ElementId,
        modeling::{
            assembly::SemanticRelation,
            definition::{DefinitionId, OverrideMap},
            occurrence::OccurrenceIdentity,
        },
    },
};
use bevy::prelude::*;

/// Default advisory threshold for the "high override variance" check: a
/// coefficient of variation above this is flagged as a derivation signal
/// (§7.2 point 2 of the proposal).
pub const DEFAULT_OVERRIDE_VARIANCE_THRESHOLD: f64 = 0.20;

/// Parameters starting with any of these strings are treated as placement
/// and excluded from the override-variance analysis (§3.1 allowlist).
const PLACEMENT_PARAM_PREFIXES: &[&str] = &["position", "orientation", "rotation"];

pub fn component_structure_quality_constraint() -> ConstraintDescriptor {
    ConstraintDescriptor {
        id: ConstraintId("ComponentStructureQuality".into()),
        label: "Component Structure Quality".into(),
        description: "Advises when an assembly's component structure diverges from the \
             COMPONENT_STRUCTURE contract: repeated same-role singletons that \
             should be Occurrences of one Definition, or high override variance \
             on a shared Definition that suggests a derived variant."
            .into(),
        applicability: Applicability::any(),
        default_severity: Severity::Advice,
        rationale: "Reusable Definitions plus Occurrences keep the model editable and \
             keep AI agents able to reason about repeated members. Off-contract \
             authoring produces piles of unrelated geometry that are hard to \
             change and hard to explain. v1 is advisory only."
            .into(),
        source_backlink: None,
        role: ConstraintRole::Validation,
        validator: Arc::new(run_component_structure_quality_validator),
    }
}

fn run_component_structure_quality_validator(subject: Entity, world: &World) -> Vec<Finding> {
    let Some(parent_eid_ref) = world.get::<ElementId>(subject) else {
        return Vec::new();
    };
    let parent_eid = *parent_eid_ref;

    let children = collect_refined_children(world, parent_eid);
    if children.len() < 2 {
        return Vec::new();
    }

    let mut findings = Vec::new();
    let now = now_secs();

    check_repeated_same_role_singletons(world, parent_eid, &children, now, &mut findings);
    check_high_override_variance(
        world,
        parent_eid,
        &children,
        DEFAULT_OVERRIDE_VARIANCE_THRESHOLD,
        now,
        &mut findings,
    );

    findings
}

fn collect_refined_children(world: &World, parent_eid: ElementId) -> Vec<ElementId> {
    // `SemanticRelation` may not be registered if no entity has ever carried
    // one; that is a legitimate "no children" state, not an error.
    let Some(mut q) = world.try_query::<&SemanticRelation>() else {
        return Vec::new();
    };
    q.iter(world)
        .filter(|rel| rel.relation_type == "refined_into" && rel.source == parent_eid)
        .map(|rel| rel.target)
        .collect()
}

fn entity_with_eid<'w>(world: &'w World, target: ElementId) -> Option<EntityRef<'w>> {
    let mut q = world
        .try_query::<(EntityRef,)>()
        .expect("EntityRef query construction is infallible in well-formed Bevy worlds");
    q.iter(world).find_map(|(entity_ref,)| {
        let eid = entity_ref.get::<ElementId>()?;
        if *eid == target {
            Some(entity_ref)
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Check 1 — repeated same-role singletons
// ---------------------------------------------------------------------------

fn check_repeated_same_role_singletons(
    world: &World,
    parent_eid: ElementId,
    children: &[ElementId],
    now: i64,
    findings: &mut Vec<Finding>,
) {
    // Group children by AuthoringRole. Singleton candidates are entities
    // **without** an OccurrenceIdentity. A role that has two or more
    // singletons and zero occurrences is flagged.
    let mut by_role: HashMap<String, RoleBucket> = HashMap::new();
    for child in children {
        let Some(entity_ref) = entity_with_eid(world, *child) else {
            continue;
        };
        let Some(role) = entity_ref.get::<AuthoringRole>() else {
            continue;
        };
        let has_occurrence = entity_ref.get::<OccurrenceIdentity>().is_some();
        let bucket = by_role.entry(role.0.clone()).or_default();
        if has_occurrence {
            bucket.with_occurrence += 1;
        } else {
            bucket.singletons.push(*child);
        }
    }

    for (role, bucket) in by_role {
        if bucket.singletons.len() >= 2 && bucket.with_occurrence == 0 {
            findings.push(Finding {
                id: FindingId(format!(
                    "ComponentStructureQuality:{}:repeated_same_role_singletons:{}",
                    parent_eid.0, role
                )),
                constraint_id: ConstraintId("ComponentStructureQuality".into()),
                subject: parent_eid.0,
                severity: Severity::Advice,
                message: format!(
                    "{} entities with role `{}` share topology but no shared Definition; \
                     consider authoring one reusable Definition with Occurrences.",
                    bucket.singletons.len(),
                    role
                ),
                rationale: "Rule A of COMPONENT_STRUCTURE: reuse an existing Definition when role \
                     and topology match. Repeating the same role as unrelated singleton \
                     entities produces unlinked geometry that is hard to edit and hard for \
                     agents to reason about."
                    .into(),
                backlink: None,
                emitted_at: now,
                role: ConstraintRole::Validation,
            });
        }
    }
}

#[derive(Default)]
struct RoleBucket {
    singletons: Vec<ElementId>,
    with_occurrence: usize,
}

// ---------------------------------------------------------------------------
// Check 2 — high override variance on a shared Definition
// ---------------------------------------------------------------------------

fn check_high_override_variance(
    world: &World,
    parent_eid: ElementId,
    children: &[ElementId],
    threshold: f64,
    now: i64,
    findings: &mut Vec<Finding>,
) {
    // Group occurrence override maps by DefinitionId.
    let mut by_definition: HashMap<DefinitionId, Vec<OverrideMap>> = HashMap::new();
    for child in children {
        let Some(entity_ref) = entity_with_eid(world, *child) else {
            continue;
        };
        let Some(occ) = entity_ref.get::<OccurrenceIdentity>() else {
            continue;
        };
        by_definition
            .entry(occ.definition_id.clone())
            .or_default()
            .push(occ.overrides.clone());
    }

    for (definition_id, override_maps) in by_definition {
        if override_maps.len() < 2 {
            continue;
        }
        for (param_name, coefficient_of_variation) in
            non_placement_cov_per_parameter(&override_maps)
        {
            if coefficient_of_variation > threshold {
                findings.push(Finding {
                    id: FindingId(format!(
                        "ComponentStructureQuality:{}:high_override_variance:{}:{}",
                        parent_eid.0, definition_id.0, param_name
                    )),
                    constraint_id: ConstraintId("ComponentStructureQuality".into()),
                    subject: parent_eid.0,
                    severity: Severity::Advice,
                    message: format!(
                        "Occurrences of Definition `{}` override parameter `{}` with a \
                         coefficient of variation of {:.2} (threshold {:.2}); \
                         consider deriving a Definition variant via base_definition_id.",
                        definition_id.0, param_name, coefficient_of_variation, threshold
                    ),
                    rationale:
                        "Rule B of COMPONENT_STRUCTURE: when topology or locked construction \
                         logic diverges materially, a derived Definition (base_definition_id) \
                         is cleaner than override-heavy Occurrences. High variance on a \
                         non-placement parameter is a signal the family has split."
                            .into(),
                    backlink: None,
                    emitted_at: now,
                    role: ConstraintRole::Validation,
                });
            }
        }
    }
}

/// For each non-placement numeric parameter present on 2+ override maps,
/// return its coefficient of variation (`stddev / |mean|`). A coefficient
/// above the threshold is a derivation signal.
fn non_placement_cov_per_parameter(override_maps: &[OverrideMap]) -> Vec<(String, f64)> {
    let mut by_param: HashMap<String, Vec<f64>> = HashMap::new();
    for map in override_maps {
        for (key, value) in &map.0 {
            if is_placement_param(key) {
                continue;
            }
            if let Some(number) = value.as_f64() {
                by_param.entry(key.clone()).or_default().push(number);
            }
        }
    }

    let mut out = Vec::new();
    for (param_name, samples) in by_param {
        if samples.len() < 2 {
            continue;
        }
        let n = samples.len() as f64;
        let mean = samples.iter().sum::<f64>() / n;
        if mean.abs() < f64::EPSILON {
            continue;
        }
        let variance = samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / n;
        let stddev = variance.sqrt();
        let coefficient_of_variation = (stddev / mean.abs()).abs();
        out.push((param_name, coefficient_of_variation));
    }
    out
}

fn is_placement_param(name: &str) -> bool {
    PLACEMENT_PARAM_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn now_secs() -> i64 {
    crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Install the shared advisory validator for any domain composition.
pub struct ComponentStructureQualityPlugin;

impl Plugin for ComponentStructureQualityPlugin {
    fn build(&self, app: &mut App) {
        use crate::capability_registry::CapabilityRegistryAppExt;
        app.register_constraint(component_structure_quality_constraint());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_registry::CapabilityRegistry,
        plugins::{
            identity::ElementIdAllocator,
            modeling::definition::{DefinitionId, DefinitionVersion, OverrideMap},
            refinement::{
                create_refinement_relation_pair, RefinementState, RefinementStateComponent,
            },
        },
    };
    use std::collections::HashMap;

    fn make_world() -> World {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(CapabilityRegistry::default());
        world
    }

    fn spawn_parent(world: &mut World) -> (Entity, ElementId) {
        let eid = world.resource::<ElementIdAllocator>().next_id();
        let entity = world
            .spawn((
                eid,
                RefinementStateComponent {
                    state: RefinementState::Constructible,
                },
            ))
            .id();
        (entity, eid)
    }

    fn spawn_singleton_child(world: &mut World, parent: ElementId, role: &str) -> ElementId {
        let eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((eid, AuthoringRole::new(role)));
        create_refinement_relation_pair(
            world,
            parent,
            eid,
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );
        eid
    }

    fn spawn_occurrence_child(
        world: &mut World,
        parent: ElementId,
        role: &str,
        definition_id: DefinitionId,
        overrides: OverrideMap,
    ) -> ElementId {
        let eid = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((
            eid,
            AuthoringRole::new(role),
            OccurrenceIdentity {
                definition_id,
                definition_version: 1 as DefinitionVersion,
                overrides,
                domain_data: serde_json::Value::Null,
                hosting: None,
                material_override: None,
                foreign_interpretation: None,
            },
        ));
        create_refinement_relation_pair(
            world,
            parent,
            eid,
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );
        eid
    }

    fn placement(map: &mut HashMap<String, serde_json::Value>, offset: f64) {
        map.insert("position".into(), serde_json::json!([0.0, 0.0, offset]));
    }

    // -----------------------------------------------------------------------
    // Severity is Advisory in v1 (§7.1)
    // -----------------------------------------------------------------------

    #[test]
    fn constraint_default_severity_is_advice() {
        let constraint = component_structure_quality_constraint();
        assert_eq!(constraint.default_severity, Severity::Advice);
        assert_eq!(constraint.id.0, "ComponentStructureQuality");
    }

    // -----------------------------------------------------------------------
    // Entities with no refined children are ignored
    // -----------------------------------------------------------------------

    #[test]
    fn subject_without_refined_children_produces_no_findings() {
        let mut world = make_world();
        let (parent_entity, _parent_eid) = spawn_parent(&mut world);
        let findings = run_component_structure_quality_validator(parent_entity, &world);
        assert!(findings.is_empty());
    }

    // -----------------------------------------------------------------------
    // Check 1 — repeated same-role singletons
    // -----------------------------------------------------------------------

    #[test]
    fn ten_unrelated_same_role_singletons_emit_advisory_finding() {
        let mut world = make_world();
        let (parent_entity, parent_eid) = spawn_parent(&mut world);
        for _ in 0..10 {
            spawn_singleton_child(&mut world, parent_eid, "truss");
        }
        let findings = run_component_structure_quality_validator(parent_entity, &world);
        let reuse: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.id.0.contains("repeated_same_role_singletons"))
            .collect();
        assert_eq!(reuse.len(), 1, "expected one reuse finding; got {reuse:?}");
        assert_eq!(reuse[0].severity, Severity::Advice);
        assert!(reuse[0].message.contains("`truss`"));
    }

    #[test]
    fn occurrences_of_one_definition_do_not_emit_reuse_finding() {
        let mut world = make_world();
        let (parent_entity, parent_eid) = spawn_parent(&mut world);
        let definition_id = DefinitionId::new();
        for index in 0..10 {
            let mut map = HashMap::new();
            placement(&mut map, 600.0 * index as f64);
            spawn_occurrence_child(
                &mut world,
                parent_eid,
                "fixture",
                definition_id.clone(),
                OverrideMap(map),
            );
        }
        let findings = run_component_structure_quality_validator(parent_entity, &world);
        let reuse: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.id.0.contains("repeated_same_role_singletons"))
            .collect();
        assert!(
            reuse.is_empty(),
            "10 occurrences of one Definition must not produce a reuse finding; got {reuse:?}"
        );
    }

    #[test]
    fn mixed_singletons_under_different_roles_do_not_trigger() {
        let mut world = make_world();
        let (parent_entity, parent_eid) = spawn_parent(&mut world);
        // One ridge_board, one sheathing, one underlayment, one finish —
        // each role has only one occurrence; no reuse signal.
        for role in ["ridge_board", "sheathing", "underlayment", "finish"] {
            spawn_singleton_child(&mut world, parent_eid, role);
        }
        let findings = run_component_structure_quality_validator(parent_entity, &world);
        let reuse: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.id.0.contains("repeated_same_role_singletons"))
            .collect();
        assert!(reuse.is_empty(), "no repeated role → no reuse finding");
    }

    // -----------------------------------------------------------------------
    // Check 2 — high override variance suggests derivation
    // -----------------------------------------------------------------------

    #[test]
    fn high_span_override_variance_emits_derivation_advisory() {
        let mut world = make_world();
        let (parent_entity, parent_eid) = spawn_parent(&mut world);
        let definition_id = DefinitionId::new();

        // Ten occurrences, eight of which override `span_mm` by wildly
        // different values (→ high coefficient of variation).
        let spans = [
            6000.0, 6000.0, 12000.0, 12000.0, 18000.0, 18000.0, 24000.0, 24000.0, 3000.0, 9000.0,
        ];
        for (index, span) in spans.iter().enumerate() {
            let mut map = HashMap::new();
            placement(&mut map, 600.0 * index as f64);
            map.insert("span_mm".into(), serde_json::json!(span));
            spawn_occurrence_child(
                &mut world,
                parent_eid,
                "fixture",
                definition_id.clone(),
                OverrideMap(map),
            );
        }

        let findings = run_component_structure_quality_validator(parent_entity, &world);
        let variance: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.id.0.contains("high_override_variance"))
            .collect();
        assert_eq!(
            variance.len(),
            1,
            "one variance finding expected; got {variance:?}"
        );
        assert_eq!(variance[0].severity, Severity::Advice);
        assert!(variance[0].message.contains("span_mm"));
    }

    #[test]
    fn placement_only_overrides_do_not_emit_variance_finding() {
        let mut world = make_world();
        let (parent_entity, parent_eid) = spawn_parent(&mut world);
        let definition_id = DefinitionId::new();
        // Ten occurrences with wildly different positions but no other
        // overrides. Placement is excluded; no variance finding should
        // trigger.
        for index in 0..10 {
            let mut map = HashMap::new();
            placement(&mut map, 1000.0 * index as f64);
            spawn_occurrence_child(
                &mut world,
                parent_eid,
                "fixture",
                definition_id.clone(),
                OverrideMap(map),
            );
        }
        let findings = run_component_structure_quality_validator(parent_entity, &world);
        let variance: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.id.0.contains("high_override_variance"))
            .collect();
        assert!(
            variance.is_empty(),
            "placement-only overrides must not trigger variance finding"
        );
    }

    #[test]
    fn low_override_variance_is_not_flagged() {
        let mut world = make_world();
        let (parent_entity, parent_eid) = spawn_parent(&mut world);
        let definition_id = DefinitionId::new();
        // `heel_height_mm` varies by 5% — well under the 20% threshold.
        let heel_heights = [150.0, 152.0, 155.0, 148.0, 151.0, 153.0];
        for (index, heel) in heel_heights.iter().enumerate() {
            let mut map = HashMap::new();
            placement(&mut map, 600.0 * index as f64);
            map.insert("travel_mm".into(), serde_json::json!(heel));
            spawn_occurrence_child(
                &mut world,
                parent_eid,
                "fixture",
                definition_id.clone(),
                OverrideMap(map),
            );
        }
        let findings = run_component_structure_quality_validator(parent_entity, &world);
        let variance: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.id.0.contains("high_override_variance"))
            .collect();
        assert!(
            variance.is_empty(),
            "within-tolerance variance must not be flagged; got {variance:?}"
        );
    }
}
