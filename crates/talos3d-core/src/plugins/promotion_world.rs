//! ECS bridge for `PromotionPlan` flows.
//!
//! `crate::plugins::promotion` deliberately stays Bevy-free so the
//! boundary types are easy to test without a `World`. This sibling
//! module owns the ECS-aware glue: it gathers
//! `SemanticAssemblyPromotionInput` snapshots from world state for the
//! `SemanticAssemblyPromotionSource` adapter (slice B1), and — in
//! later slices — applies the migration diff back to the world after a
//! successful emission (slice B2) and surfaces an MCP entry point
//! (slice B3).
//!
//! Per ADR-047 / `ASSEMBLY_TO_DEFINITION_BRIDGE_AGREEMENT.md`, the
//! gather step here must classify every member as
//! `AuthoredEntity | Occurrence | NestedAssembly`, capture occurrence
//! definition ids so the adapter can reuse them as child-slot
//! definition ids, and collect every external assembly membership and
//! relation that touches a source member so the migration diff can
//! preview the retargeting Preview will display.
//!
//! The gather function is deliberately read-only: it queries the world
//! through `&World` accessors and returns owned data. World mutation
//! lives in slice B2.

use bevy::{ecs::world::EntityRef, prelude::*};

use crate::plugins::{
    identity::ElementId,
    modeling::{
        assembly::{SemanticAssembly, SemanticRelation},
        occurrence::OccurrenceIdentity,
    },
    promotion::{
        AssemblyCapabilityProjection, AssemblyMemberKind, AssemblyMemberSnapshot,
        ExternalAssemblyMembership, ExternalGraph, ExternalRelation,
        SemanticAssemblyPromotionInput,
    },
};

/// Errors produced by `gather_semantic_assembly_input`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyGatherError {
    /// No entity with the requested `ElementId` exists.
    AssemblyNotFound { assembly_id: ElementId },
    /// The entity exists but is not a `SemanticAssembly`.
    NotASemanticAssembly { assembly_id: ElementId },
}

impl std::fmt::Display for AssemblyGatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AssemblyNotFound { assembly_id } => {
                write!(f, "promotion gather: no entity with ElementId {assembly_id:?}")
            }
            Self::NotASemanticAssembly { assembly_id } => write!(
                f,
                "promotion gather: entity {assembly_id:?} is not a SemanticAssembly"
            ),
        }
    }
}

impl std::error::Error for AssemblyGatherError {}

/// Read-only entry point. Walks `world` once, finds the
/// `SemanticAssembly` with the requested `assembly_id`, classifies its
/// members, and collects every external assembly membership and
/// relation that touches a source member. The returned input is ready
/// for `SemanticAssemblyPromotionSource::build_plan_and_diff`.
///
/// Capability descriptor metadata is *not* sourced from the world here
/// — the agreement places `descriptor_id` / `descriptor_version` /
/// `role_vocabulary_version` on the capability registry. This function
/// fills the assembly type from the live `SemanticAssembly.assembly_type`
/// and leaves descriptor lookup to the caller (slice B3 will resolve it
/// through the capability registry).
pub fn gather_semantic_assembly_input(
    world: &World,
    assembly_id: ElementId,
) -> Result<SemanticAssemblyPromotionInput, AssemblyGatherError> {
    let assembly = read_assembly_component(world, assembly_id)?;

    // Classify every member by reading its components on a single pass.
    let members: Vec<AssemblyMemberSnapshot> = assembly
        .members
        .iter()
        .map(|member_ref| AssemblyMemberSnapshot {
            element_id: member_ref.target,
            role: member_ref.role.clone(),
            kind: classify_member(world, member_ref.target),
            occurrence_definition_id: read_occurrence_definition_id(world, member_ref.target),
        })
        .collect();

    let source_member_ids: std::collections::HashSet<ElementId> =
        members.iter().map(|m| m.element_id).collect();

    let external_graph = collect_external_graph(world, assembly_id, &source_member_ids);

    Ok(SemanticAssemblyPromotionInput {
        assembly_id,
        members,
        capability: AssemblyCapabilityProjection {
            assembly_type: assembly.assembly_type.clone(),
            descriptor_id: None,
            descriptor_version: None,
            role_vocabulary_version: None,
        },
        external_graph,
        source_parameters: assembly.parameters.clone(),
        source_label: assembly.label.clone(),
    })
}

fn read_assembly_component(
    world: &World,
    assembly_id: ElementId,
) -> Result<SemanticAssembly, AssemblyGatherError> {
    // Walk every entity that has an ElementId in one pass: distinguish
    // "no such ElementId at all" from "exists but is not a
    // SemanticAssembly" without needing two separate queries (which can
    // miss freshly-spawned archetypes when the test world has just one
    // entity per archetype).
    let Some(mut q) = world.try_query::<EntityRef>() else {
        return Err(AssemblyGatherError::AssemblyNotFound { assembly_id });
    };
    let mut found_any_with_id = false;
    for entity_ref in q.iter(world) {
        let Some(eid) = entity_ref.get::<ElementId>() else {
            continue;
        };
        if *eid != assembly_id {
            continue;
        }
        found_any_with_id = true;
        if let Some(assembly) = entity_ref.get::<SemanticAssembly>() {
            return Ok(assembly.clone());
        }
    }
    if found_any_with_id {
        Err(AssemblyGatherError::NotASemanticAssembly { assembly_id })
    } else {
        Err(AssemblyGatherError::AssemblyNotFound { assembly_id })
    }
}

fn classify_member(world: &World, member_id: ElementId) -> AssemblyMemberKind {
    let Some(mut q) = world.try_query::<EntityRef>() else {
        // No ElementId index at all: treat as authored leaf — the
        // adapter will reject if an Occurrence id is required.
        return AssemblyMemberKind::AuthoredEntity;
    };
    for entity_ref in q.iter(world) {
        let Some(eid) = entity_ref.get::<ElementId>() else {
            continue;
        };
        if *eid != member_id {
            continue;
        }
        if entity_ref.get::<SemanticAssembly>().is_some() {
            return AssemblyMemberKind::NestedAssembly;
        }
        if entity_ref.get::<OccurrenceIdentity>().is_some() {
            return AssemblyMemberKind::Occurrence;
        }
        return AssemblyMemberKind::AuthoredEntity;
    }
    // Member references a missing entity — the adapter rejects this
    // shape upstream when it actually validates members; here we just
    // pick the safest fallback.
    AssemblyMemberKind::AuthoredEntity
}

fn read_occurrence_definition_id(
    world: &World,
    member_id: ElementId,
) -> Option<crate::plugins::modeling::definition::DefinitionId> {
    let mut q = world.try_query::<EntityRef>()?;
    for entity_ref in q.iter(world) {
        let Some(eid) = entity_ref.get::<ElementId>() else {
            continue;
        };
        if *eid != member_id {
            continue;
        }
        return entity_ref
            .get::<OccurrenceIdentity>()
            .map(|identity| identity.definition_id.clone());
    }
    None
}

fn collect_external_graph(
    world: &World,
    source_assembly_id: ElementId,
    source_member_ids: &std::collections::HashSet<ElementId>,
) -> ExternalGraph {
    let mut memberships: Vec<ExternalAssemblyMembership> = Vec::new();
    if let Some(mut q) = world.try_query::<(&ElementId, &SemanticAssembly)>() {
        for (assembly_id, assembly) in q.iter(world) {
            if *assembly_id == source_assembly_id {
                continue;
            }
            let mut targets: Vec<ElementId> = Vec::new();
            for m in &assembly.members {
                if source_member_ids.contains(&m.target) {
                    targets.push(m.target);
                }
            }
            if !targets.is_empty() {
                memberships.push(ExternalAssemblyMembership {
                    assembly_id: *assembly_id,
                    member_targets: targets,
                });
            }
        }
    }

    let mut relations: Vec<ExternalRelation> = Vec::new();
    if let Some(mut q) = world.try_query::<(&ElementId, &SemanticRelation)>() {
        for (relation_id, relation) in q.iter(world) {
            if source_member_ids.contains(&relation.source)
                || source_member_ids.contains(&relation.target)
            {
                relations.push(ExternalRelation {
                    relation_id: *relation_id,
                    source: relation.source,
                    target: relation.target,
                    relation_type: relation.relation_type.clone(),
                });
            }
        }
    }

    ExternalGraph {
        memberships,
        relations,
    }
}

// === Commit step ===========================================================

/// Counts of mutations applied by `apply_assembly_migration_diff`.
/// Returned to the caller (Preview UI / MCP) so the user can see what
/// the commit actually did vs what the diff predicted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedAssemblyMigration {
    /// Number of `SemanticAssembly` entities whose `members` list was
    /// rewritten. Always `>=` the number of `RetargetedAssembly`
    /// entries in the diff for assemblies that were found in the
    /// world.
    pub assemblies_retargeted: usize,
    /// Number of source-assembly `members` entries collapsed into a
    /// single `realization` entry on the surviving wrapper.
    pub source_members_collapsed: usize,
    /// Number of external-assembly `members` entries retargeted to
    /// the new occurrence (preserving each entry's original role).
    pub external_member_retargets: usize,
    /// Number of `SemanticRelation` entities whose `source` and/or
    /// `target` was rewritten to the new occurrence.
    pub relations_retargeted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyCommitError {
    /// A `RetargetedAssembly` named an assembly that does not exist
    /// in the world. Surfaced as an error rather than silently
    /// skipped because the migration diff is meant to reflect the
    /// world snapshot and any drift indicates a stale Preview.
    AssemblyNotFound { assembly_id: ElementId },
    /// A `RetargetedRelation` named a relation that does not exist in
    /// the world.
    RelationNotFound { relation_id: ElementId },
    /// The diff named an entity but it carries the wrong component
    /// (e.g. an assembly id resolves to a non-`SemanticAssembly` entity
    /// — the world drifted between Preview and Commit).
    UnexpectedEntityShape { entity_id: ElementId, expected: &'static str },
}

impl std::fmt::Display for AssemblyCommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AssemblyNotFound { assembly_id } => write!(
                f,
                "promotion commit: SemanticAssembly {assembly_id:?} not found in world"
            ),
            Self::RelationNotFound { relation_id } => write!(
                f,
                "promotion commit: SemanticRelation {relation_id:?} not found in world"
            ),
            Self::UnexpectedEntityShape { entity_id, expected } => write!(
                f,
                "promotion commit: entity {entity_id:?} is not a {expected}"
            ),
        }
    }
}

impl std::error::Error for AssemblyCommitError {}

/// Apply a `SemanticGraphMigrationDiff` to `world` after the shared
/// emitter has produced a `DefinitionDraft` and the caller has spawned
/// a new Occurrence at `new_occurrence_id`.
///
/// Per the agreement, the source assembly survives as a retargeted
/// project-intent wrapper:
/// `members: [{ target: new_occurrence_id, role: "realization" }]`.
/// External assemblies that referenced any source member retain their
/// original roles but point at the new Occurrence. Relations whose
/// endpoints touched a source member are re-pointed to the new
/// Occurrence.
///
/// **Source assembly identification.** Slice A's adapter records the
/// source assembly's self-retarget with a single empty-slot entry in
/// `to_slot_ids`. We use that as the discriminator: any
/// `RetargetedAssembly` whose `to_slot_ids == [""]` is the source
/// assembly; everything else is external.
///
/// **Atomicity.** Mutations are applied in a single pass. If any
/// step fails, the function returns an error after the partially-
/// applied changes; the caller is responsible for the larger commit-
/// or-rollback envelope (slice B3 will wrap this in the MCP commit
/// flow). PP-A2DB-1 keeps the rollback discipline at the MCP layer.
pub fn apply_assembly_migration_diff(
    world: &mut World,
    diff: &crate::plugins::promotion::SemanticGraphMigrationDiff,
    new_occurrence_id: ElementId,
) -> Result<AppliedAssemblyMigration, AssemblyCommitError> {
    let mut applied = AppliedAssemblyMigration {
        assemblies_retargeted: 0,
        source_members_collapsed: 0,
        external_member_retargets: 0,
        relations_retargeted: 0,
    };

    // Compute the union of all `from_members` across the diff; this is
    // the set of source member ids that must be redirected to the new
    // Occurrence. Relations only re-point endpoints that match a source
    // member — endpoints pointing at unrelated entities stay put even
    // if they happen to equal the diff's `original_target` field for a
    // different reason (the `original_target` field on
    // `RetargetedRelation` is a snapshot, not a request).
    let source_member_set: std::collections::HashSet<ElementId> = diff
        .retargeted_assemblies
        .iter()
        .flat_map(|r| r.from_members.iter().copied())
        .collect();

    for retargeted in &diff.retargeted_assemblies {
        let entity = find_entity_with_element_id(world, retargeted.assembly_id)
            .ok_or(AssemblyCommitError::AssemblyNotFound {
                assembly_id: retargeted.assembly_id,
            })?;

        if !world.entity(entity).contains::<SemanticAssembly>() {
            return Err(AssemblyCommitError::UnexpectedEntityShape {
                entity_id: retargeted.assembly_id,
                expected: "SemanticAssembly",
            });
        }

        let is_source_assembly = is_source_assembly(retargeted);

        let mut assembly = world.get_mut::<SemanticAssembly>(entity).expect("checked");
        if is_source_assembly {
            // Surviving wrapper shape per the agreement: a single
            // realization entry pointing at the new Occurrence.
            let collapsed = retargeted.from_members.len();
            assembly.members.retain(|m| !retargeted.from_members.contains(&m.target));
            assembly.members.push(super_assembly_member_ref(
                new_occurrence_id,
                "realization",
            ));
            applied.source_members_collapsed += collapsed;
        } else {
            // External assemblies retain each member entry's role and
            // just retarget the `target` to the new Occurrence.
            let mut retargeted_count = 0usize;
            for m in assembly.members.iter_mut() {
                if retargeted.from_members.contains(&m.target) {
                    m.target = new_occurrence_id;
                    retargeted_count += 1;
                }
            }
            applied.external_member_retargets += retargeted_count;
        }
        applied.assemblies_retargeted += 1;
    }

    for relation_diff in &diff.retargeted_relations {
        let entity = find_entity_with_element_id(world, relation_diff.relation_id)
            .ok_or(AssemblyCommitError::RelationNotFound {
                relation_id: relation_diff.relation_id,
            })?;
        if !world.entity(entity).contains::<SemanticRelation>() {
            return Err(AssemblyCommitError::UnexpectedEntityShape {
                entity_id: relation_diff.relation_id,
                expected: "SemanticRelation",
            });
        }
        let mut relation = world.get_mut::<SemanticRelation>(entity).expect("checked");
        let mut touched = false;
        // Only rewrite endpoints that pointed at a source member.
        // Matching `original_source`/`original_target` is necessary
        // (the diff is a snapshot) but NOT sufficient — an endpoint
        // pointing at a non-source entity must stay put.
        if relation.source == relation_diff.original_source
            && source_member_set.contains(&relation_diff.original_source)
        {
            relation.source = new_occurrence_id;
            touched = true;
        }
        if relation.target == relation_diff.original_target
            && source_member_set.contains(&relation_diff.original_target)
        {
            relation.target = new_occurrence_id;
            touched = true;
        }
        if touched {
            applied.relations_retargeted += 1;
        }
    }

    Ok(applied)
}

fn is_source_assembly(retargeted: &crate::plugins::promotion::RetargetedAssembly) -> bool {
    retargeted.to_slot_ids.iter().any(|s| s.is_empty())
}

fn find_entity_with_element_id(world: &mut World, target: ElementId) -> Option<Entity> {
    let mut q = world.try_query::<(Entity, &ElementId)>()?;
    q.iter(world)
        .find_map(|(entity, eid)| if *eid == target { Some(entity) } else { None })
}

/// Construct an `AssemblyMemberRef` without forcing the public
/// re-export of the type into this module.
fn super_assembly_member_ref(
    target: ElementId,
    role: &str,
) -> crate::plugins::modeling::assembly::AssemblyMemberRef {
    crate::plugins::modeling::assembly::AssemblyMemberRef {
        target,
        role: role.to_string(),
    }
}

// === Tests =================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::modeling::{
        assembly::{AssemblyMemberRef, SemanticAssembly, SemanticRelation},
        definition::DefinitionId,
        occurrence::OccurrenceIdentity,
    };
    use crate::plugins::promotion::{
        AssemblyMemberKind, PromotionDraftEmitter, SemanticAssemblyAdapterError,
        SemanticAssemblyPromotionSource,
    };
    use serde_json::json;

    fn elem(n: u64) -> ElementId {
        ElementId(n)
    }

    /// Authored leaf — just an ElementId on the entity.
    fn spawn_authored_leaf(world: &mut World, id: ElementId) {
        world.spawn(id);
    }

    /// Occurrence — ElementId + OccurrenceIdentity referencing a
    /// definition id.
    fn spawn_occurrence(world: &mut World, id: ElementId, definition_id: &str) {
        world.spawn((
            id,
            OccurrenceIdentity::new(DefinitionId(definition_id.into()), 1u32),
        ));
    }

    /// SemanticAssembly — ElementId + SemanticAssembly component.
    fn spawn_assembly(
        world: &mut World,
        id: ElementId,
        assembly_type: &str,
        label: &str,
        members: Vec<AssemblyMemberRef>,
        parameters: serde_json::Value,
    ) {
        world.spawn((
            id,
            SemanticAssembly {
                assembly_type: assembly_type.into(),
                label: label.into(),
                members,
                parameters,
                metadata: serde_json::Value::Null,
            },
        ));
    }

    fn member(target: ElementId, role: &str) -> AssemblyMemberRef {
        AssemblyMemberRef {
            target,
            role: role.into(),
        }
    }

    #[test]
    fn gather_returns_assembly_not_found_for_missing_id() {
        let mut world = World::new();
        let err = gather_semantic_assembly_input(&world, elem(1)).unwrap_err();
        assert_eq!(err, AssemblyGatherError::AssemblyNotFound { assembly_id: elem(1) });
        let _ = &mut world; // silence unused_mut on World — reused in some impls
    }

    #[test]
    fn gather_returns_not_a_semantic_assembly_when_id_belongs_to_other_entity() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(1));
        let err = gather_semantic_assembly_input(&world, elem(1)).unwrap_err();
        assert_eq!(err, AssemblyGatherError::NotASemanticAssembly { assembly_id: elem(1) });
    }

    #[test]
    fn gather_classifies_authored_leaf_members() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(11));
        spawn_assembly(
            &mut world,
            elem(1),
            "test",
            "T",
            vec![member(elem(10), "wall"), member(elem(11), "wall")],
            serde_json::Value::Null,
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        assert_eq!(input.assembly_id, elem(1));
        assert_eq!(input.members.len(), 2);
        for m in &input.members {
            assert_eq!(m.kind, AssemblyMemberKind::AuthoredEntity);
            assert!(m.occurrence_definition_id.is_none());
        }
    }

    #[test]
    fn gather_classifies_occurrence_members_and_records_definition_id() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_occurrence(&mut world, elem(11), "lib.window");
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall"), member(elem(11), "window")],
            serde_json::Value::Null,
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        assert_eq!(input.members[0].kind, AssemblyMemberKind::AuthoredEntity);
        assert_eq!(input.members[1].kind, AssemblyMemberKind::Occurrence);
        assert_eq!(
            input.members[1].occurrence_definition_id,
            Some(DefinitionId("lib.window".into()))
        );
    }

    #[test]
    fn gather_flags_nested_semantic_assembly_members() {
        let mut world = World::new();
        // Nested assembly itself is a SemanticAssembly.
        spawn_assembly(
            &mut world,
            elem(2),
            "subroom",
            "S",
            vec![],
            serde_json::Value::Null,
        );
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(2), "subroom")],
            serde_json::Value::Null,
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        assert_eq!(input.members[0].kind, AssemblyMemberKind::NestedAssembly);
        // The adapter rejects this; the gather function is honest about
        // the classification.
        let adapter = SemanticAssemblyPromotionSource {
            name: "n".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let err = adapter.build_plan_and_diff(input).unwrap_err();
        assert!(matches!(
            err,
            SemanticAssemblyAdapterError::UnsupportedNestedAssemblyMembers { .. }
        ));
    }

    #[test]
    fn gather_collects_external_membership_pointing_at_source_member() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        // Source assembly with member 10.
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall")],
            serde_json::Value::Null,
        );
        // External assembly that ALSO references member 10.
        spawn_assembly(
            &mut world,
            elem(2),
            "context",
            "C",
            vec![member(elem(10), "wall")],
            serde_json::Value::Null,
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        assert_eq!(input.external_graph.memberships.len(), 1);
        assert_eq!(
            input.external_graph.memberships[0].assembly_id,
            elem(2)
        );
        assert_eq!(
            input.external_graph.memberships[0].member_targets,
            vec![elem(10)]
        );
    }

    #[test]
    fn gather_excludes_source_assembly_from_external_memberships() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall")],
            serde_json::Value::Null,
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        // The source assembly (`elem(1)`) must not appear in the
        // external memberships list — adapter handles self-retarget on
        // its own through the replace-source policy.
        assert!(input
            .external_graph
            .memberships
            .iter()
            .all(|m| m.assembly_id != elem(1)));
    }

    #[test]
    fn gather_collects_external_relations_touching_source_members() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(20));
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall")],
            serde_json::Value::Null,
        );
        // Relation: 10 -> 20 (touches a source member at the source
        // endpoint).
        world.spawn((
            elem(30),
            SemanticRelation {
                source: elem(10),
                target: elem(20),
                relation_type: "supports".into(),
                parameters: serde_json::Value::Null,
            },
        ));
        // Relation: 20 -> 10 (touches at the target endpoint).
        world.spawn((
            elem(31),
            SemanticRelation {
                source: elem(20),
                target: elem(10),
                relation_type: "bounds".into(),
                parameters: serde_json::Value::Null,
            },
        ));
        // Unrelated relation: 20 -> 21 (no source member touched).
        spawn_authored_leaf(&mut world, elem(21));
        world.spawn((
            elem(32),
            SemanticRelation {
                source: elem(20),
                target: elem(21),
                relation_type: "ignored".into(),
                parameters: serde_json::Value::Null,
            },
        ));
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        let relation_ids: Vec<ElementId> = input
            .external_graph
            .relations
            .iter()
            .map(|r| r.relation_id)
            .collect();
        assert!(relation_ids.contains(&elem(30)));
        assert!(relation_ids.contains(&elem(31)));
        assert!(!relation_ids.contains(&elem(32)));
    }

    #[test]
    fn gather_carries_source_label_and_parameters_for_provenance() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_assembly(
            &mut world,
            elem(1),
            "kitchen",
            "Kitchen #3",
            vec![member(elem(10), "wall")],
            json!({ "ceiling_height_m": 2.7 }),
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        assert_eq!(input.source_label, "Kitchen #3");
        assert_eq!(input.source_parameters["ceiling_height_m"], json!(2.7));
        assert_eq!(input.capability.assembly_type, "kitchen");
    }

    #[test]
    fn gathered_input_runs_through_adapter_with_clean_plan() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_occurrence(&mut world, elem(11), "lib.window");
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall"), member(elem(11), "window")],
            serde_json::Value::Null,
        );
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        let adapter = SemanticAssemblyPromotionSource {
            name: "GatheredHouse".into(),
            replace_source: true,
            provenance: Default::default(),
        };
        let out = adapter.build_plan_and_diff(input).unwrap();
        // 2 members -> 2 child slots.
        assert_eq!(out.plan.declared_slot_ids().len(), 2);
        // Replace-source self-retarget is recorded.
        assert!(out
            .migration_diff
            .retargeted_assemblies
            .iter()
            .any(|r| r.assembly_id == elem(1)));
        // Capability projection is missing descriptor metadata, so a
        // warning is recorded — gather does NOT fill descriptor info.
        assert!(out
            .migration_diff
            .warnings
            .iter()
            .any(|w| matches!(
                w,
                crate::plugins::promotion::MigrationWarning::CapabilityProjectionOutdated { .. }
            )));
    }

    // === apply_assembly_migration_diff =====================================

    fn diff_for_replace_source(
        source_assembly_id: ElementId,
        source_members: &[ElementId],
    ) -> crate::plugins::promotion::SemanticGraphMigrationDiff {
        crate::plugins::promotion::SemanticGraphMigrationDiff {
            retargeted_assemblies: vec![
                crate::plugins::promotion::RetargetedAssembly {
                    assembly_id: source_assembly_id,
                    from_members: source_members.to_vec(),
                    to_slot_ids: vec![String::new()], // marker: source self-retarget
                },
            ],
            retargeted_relations: Vec::new(),
            orphaned_memberships: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn commit_collapses_source_assembly_to_realization_entry() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(11));
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall"), member(elem(11), "wall")],
            serde_json::Value::Null,
        );
        let diff = diff_for_replace_source(elem(1), &[elem(10), elem(11)]);
        let new_occ = elem(500);

        let applied =
            apply_assembly_migration_diff(&mut world, &diff, new_occ).unwrap();
        assert_eq!(applied.assemblies_retargeted, 1);
        assert_eq!(applied.source_members_collapsed, 2);
        assert_eq!(applied.external_member_retargets, 0);
        assert_eq!(applied.relations_retargeted, 0);

        // Surviving wrapper now has exactly one realization member.
        let mut q = world.query::<(&ElementId, &SemanticAssembly)>();
        let (_, survivor) = q
            .iter(&world)
            .find(|(eid, _)| **eid == elem(1))
            .expect("source assembly survived");
        assert_eq!(survivor.members.len(), 1);
        assert_eq!(survivor.members[0].target, new_occ);
        assert_eq!(survivor.members[0].role, "realization");
    }

    #[test]
    fn commit_retargets_external_assembly_members_in_place() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        // External assembly references the source member with its own
        // role.
        spawn_assembly(
            &mut world,
            elem(2),
            "context",
            "C",
            vec![
                member(elem(10), "anchor"),
                member(elem(99), "unrelated"),
            ],
            serde_json::Value::Null,
        );
        // Source assembly (so the diff's source-self-retarget can run
        // through; not asserted here).
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall")],
            serde_json::Value::Null,
        );

        let diff = crate::plugins::promotion::SemanticGraphMigrationDiff {
            retargeted_assemblies: vec![
                crate::plugins::promotion::RetargetedAssembly {
                    assembly_id: elem(1),
                    from_members: vec![elem(10)],
                    to_slot_ids: vec![String::new()],
                },
                crate::plugins::promotion::RetargetedAssembly {
                    assembly_id: elem(2),
                    from_members: vec![elem(10)],
                    to_slot_ids: vec!["wall".into()],
                },
            ],
            retargeted_relations: Vec::new(),
            orphaned_memberships: Vec::new(),
            warnings: Vec::new(),
        };

        let applied =
            apply_assembly_migration_diff(&mut world, &diff, elem(500)).unwrap();
        assert_eq!(applied.assemblies_retargeted, 2);
        assert_eq!(applied.source_members_collapsed, 1);
        assert_eq!(applied.external_member_retargets, 1);

        // External assembly's "anchor"-role member now points at the
        // new Occurrence; the unrelated member is untouched.
        let mut q = world.query::<(&ElementId, &SemanticAssembly)>();
        let (_, external) = q
            .iter(&world)
            .find(|(eid, _)| **eid == elem(2))
            .expect("external assembly survived");
        assert_eq!(external.members.len(), 2);
        let anchor = external
            .members
            .iter()
            .find(|m| m.role == "anchor")
            .unwrap();
        assert_eq!(anchor.target, elem(500));
        let other = external
            .members
            .iter()
            .find(|m| m.role == "unrelated")
            .unwrap();
        assert_eq!(other.target, elem(99));
    }

    #[test]
    fn commit_retargets_external_relation_endpoints() {
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(20));
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall")],
            serde_json::Value::Null,
        );
        // Relation 30: source=10, target=20 (source endpoint touches
        // a source member).
        world.spawn((
            elem(30),
            SemanticRelation {
                source: elem(10),
                target: elem(20),
                relation_type: "supports".into(),
                parameters: serde_json::Value::Null,
            },
        ));
        // Relation 31: source=20, target=10 (target endpoint touches
        // a source member).
        world.spawn((
            elem(31),
            SemanticRelation {
                source: elem(20),
                target: elem(10),
                relation_type: "bounds".into(),
                parameters: serde_json::Value::Null,
            },
        ));

        let diff = crate::plugins::promotion::SemanticGraphMigrationDiff {
            retargeted_assemblies: vec![
                crate::plugins::promotion::RetargetedAssembly {
                    assembly_id: elem(1),
                    from_members: vec![elem(10)],
                    to_slot_ids: vec![String::new()],
                },
            ],
            retargeted_relations: vec![
                crate::plugins::promotion::RetargetedRelation {
                    relation_id: elem(30),
                    original_source: elem(10),
                    original_target: elem(20),
                    relation_type: "supports".into(),
                },
                crate::plugins::promotion::RetargetedRelation {
                    relation_id: elem(31),
                    original_source: elem(20),
                    original_target: elem(10),
                    relation_type: "bounds".into(),
                },
            ],
            orphaned_memberships: Vec::new(),
            warnings: Vec::new(),
        };

        let applied =
            apply_assembly_migration_diff(&mut world, &diff, elem(500)).unwrap();
        assert_eq!(applied.relations_retargeted, 2);

        let mut q = world.query::<(&ElementId, &SemanticRelation)>();
        for (eid, rel) in q.iter(&world) {
            match eid.0 {
                30 => {
                    assert_eq!(rel.source, elem(500), "30 source rewritten");
                    assert_eq!(rel.target, elem(20), "30 target untouched");
                }
                31 => {
                    assert_eq!(rel.source, elem(20), "31 source untouched");
                    assert_eq!(rel.target, elem(500), "31 target rewritten");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn commit_returns_assembly_not_found_when_diff_drifted() {
        let mut world = World::new();
        // No assembly in the world; diff names one anyway.
        let diff = diff_for_replace_source(elem(99), &[elem(10)]);
        let err = apply_assembly_migration_diff(&mut world, &diff, elem(500))
            .unwrap_err();
        assert_eq!(
            err,
            AssemblyCommitError::AssemblyNotFound { assembly_id: elem(99) }
        );
    }

    #[test]
    fn commit_returns_unexpected_entity_shape_when_id_resolves_to_non_assembly() {
        let mut world = World::new();
        // ElementId(99) on a leaf entity (NO SemanticAssembly).
        spawn_authored_leaf(&mut world, elem(99));
        let diff = diff_for_replace_source(elem(99), &[elem(10)]);
        let err = apply_assembly_migration_diff(&mut world, &diff, elem(500))
            .unwrap_err();
        assert_eq!(
            err,
            AssemblyCommitError::UnexpectedEntityShape {
                entity_id: elem(99),
                expected: "SemanticAssembly",
            }
        );
    }

    #[test]
    fn commit_returns_relation_not_found_when_diff_drifted() {
        let mut world = World::new();
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![],
            serde_json::Value::Null,
        );
        let diff = crate::plugins::promotion::SemanticGraphMigrationDiff {
            retargeted_assemblies: vec![
                crate::plugins::promotion::RetargetedAssembly {
                    assembly_id: elem(1),
                    from_members: Vec::new(),
                    to_slot_ids: vec![String::new()],
                },
            ],
            retargeted_relations: vec![
                crate::plugins::promotion::RetargetedRelation {
                    relation_id: elem(404),
                    original_source: elem(10),
                    original_target: elem(20),
                    relation_type: "ghost".into(),
                },
            ],
            orphaned_memberships: Vec::new(),
            warnings: Vec::new(),
        };
        let err = apply_assembly_migration_diff(&mut world, &diff, elem(500))
            .unwrap_err();
        assert_eq!(
            err,
            AssemblyCommitError::RelationNotFound { relation_id: elem(404) }
        );
    }

    #[test]
    fn end_to_end_gather_adapter_emit_commit_cycle() {
        // The integration covered: gather a flat assembly, run it
        // through the adapter, emit through the shared default emitter
        // with a compound body builder, then apply the migration diff
        // back to the world. Verifies the four pieces (slice A
        // adapter, slice B1 gather, slice B2 commit, PP-A2DB-0
        // emitter) compose end-to-end.
        let mut world = World::new();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_occurrence(&mut world, elem(11), "lib.window");
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![member(elem(10), "wall"), member(elem(11), "window")],
            serde_json::Value::Null,
        );

        // Gather + adapter + emit
        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        let adapter = crate::plugins::promotion::SemanticAssemblyPromotionSource {
            name: "House".into(),
            replace_source: true,
            provenance: Default::default(),
        };
        let out = adapter.build_plan_and_diff(input).unwrap();

        let mut drafts =
            crate::plugins::definition_authoring::DefinitionDraftRegistry::default();
        let existing = std::collections::HashSet::<ElementId>::new();
        let mut emitter = crate::plugins::promotion::DefaultPromotionDraftEmitter::new(
            &mut drafts,
            &existing,
            |plan: &crate::plugins::promotion::PromotionPlan| {
                let mut def =
                    crate::plugins::definition_authoring::blank_definition("House");
                if let crate::plugins::promotion::PromotionOutputShape::Compound {
                    child_slots,
                } = &plan.output_shape
                {
                    def.compound = Some(crate::plugins::modeling::definition::CompoundDefinition {
                        child_slots: child_slots.clone(),
                        ..Default::default()
                    });
                }
                Ok(def)
            },
        );
        let _record = emitter.emit(out.plan.clone()).unwrap();

        // Now commit the migration diff against the (mutable) world.
        let new_occ = elem(900);
        let applied = apply_assembly_migration_diff(&mut world, &out.migration_diff, new_occ)
            .expect("commit succeeds");
        assert_eq!(applied.source_members_collapsed, 2);

        // Source assembly should now be a 1-member realization wrapper.
        let mut q = world.query::<(&ElementId, &SemanticAssembly)>();
        let (_, survivor) = q
            .iter(&world)
            .find(|(eid, _)| **eid == elem(1))
            .expect("source assembly survived");
        assert_eq!(survivor.members.len(), 1);
        assert_eq!(survivor.members[0].target, new_occ);
        assert_eq!(survivor.members[0].role, "realization");
    }
}
