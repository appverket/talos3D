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
        AssemblyMemberKind, SemanticAssemblyPromotionSource, SemanticAssemblyAdapterError,
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
}
