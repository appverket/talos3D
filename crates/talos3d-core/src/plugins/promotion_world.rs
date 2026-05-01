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
    let internal_relations =
        collect_internal_relations(world, assembly_id, &source_member_ids);

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
        internal_relations,
        // No-registry default: empty rules + `default_unknown = None`.
        // Every preserved relation stays `classification: None` and
        // surfaces an `UnknownRelationDescriptor` warning — the safe
        // default per the agreement. Callers that want descriptor-
        // backed classification go through
        // `gather_semantic_assembly_input_with_capability` (PP-A2DB-2
        // slice C4b) which seeds the rules from the live capability
        // registry.
        relation_classification:
            crate::plugins::promotion::RelationClassificationRules::default(),
        source_parameters: assembly.parameters.clone(),
        source_label: assembly.label.clone(),
    })
}

/// Gather variant that seeds `RelationClassificationRules` from the
/// live capability registry. Walks every `RelationTypeDescriptor`
/// whose `external_classification` is `Some(_)` and folds it into the
/// `by_descriptor` map. `default_unknown` stays `None` per the
/// agreement (unknown descriptors must surface as warnings, not
/// silently classify).
///
/// Domain crates declare `external_classification` on their relation
/// descriptors at registration time
/// (`register_relation_type(RelationTypeDescriptor { ...,
/// external_classification: Some(...), ... })`). Slice C4b doesn't
/// alter the in-tree descriptors — none of talos3d-core's current
/// relation types declare a classification yet — but the surface is
/// now ready for talos3d-architecture and other domain crates to
/// fill in.
pub fn gather_semantic_assembly_input_with_capability(
    world: &World,
    capability: &crate::capability_registry::CapabilityRegistry,
    assembly_id: ElementId,
) -> Result<SemanticAssemblyPromotionInput, AssemblyGatherError> {
    let mut input = gather_semantic_assembly_input(world, assembly_id)?;
    let mut by_descriptor = std::collections::HashMap::new();
    for desc in capability.relation_type_descriptors() {
        if let Some(classification) = desc.external_classification {
            by_descriptor.insert(desc.relation_type.clone(), classification);
        }
    }
    input.relation_classification =
        crate::plugins::promotion::RelationClassificationRules {
            by_descriptor,
            default_unknown: None,
        };
    Ok(input)
}

/// Walk every `SemanticRelation` in the world that **touches** the
/// promoting assembly — i.e. at least one endpoint is the source
/// assembly id itself or one of its source members. The PP-A2DB-2
/// slice A adapter then classifies each: relations where BOTH
/// endpoints resolve into the Definition (self or a slot) become
/// `SemanticRelationTemplate` candidates; partial-touch relations
/// (one endpoint inside, one outside) land in `preserved_relations`
/// where slice B's `RelationClassificationRules` further classify
/// them into `HostContract` / `RequiredContext` / `AdvisoryContext` /
/// `DropWithAudit`.
///
/// Per the agreement: "During promotion, inspect SemanticRelations
/// involving source assembly members" — that's the broader "at least
/// one endpoint" predicate, not the strict "both endpoints" one.
/// The two earlier slice B1 commits used the strict variant, which
/// is why slice A's adapter routes endpoints with `Some/None`
/// resolution into `preserved_relations`; this gather function
/// broadens the input so slice B's classifier actually sees the
/// boundary-spanning relations.
fn collect_internal_relations(
    world: &World,
    source_assembly_id: ElementId,
    source_member_ids: &std::collections::HashSet<ElementId>,
) -> Vec<crate::plugins::promotion::InternalRelationSnapshot> {
    let mut out = Vec::new();
    let Some(mut q) = world.try_query::<(&ElementId, &SemanticRelation)>() else {
        return out;
    };
    let endpoint_in_source = |id: ElementId| -> bool {
        id == source_assembly_id || source_member_ids.contains(&id)
    };
    for (relation_id, relation) in q.iter(world) {
        if endpoint_in_source(relation.source) || endpoint_in_source(relation.target) {
            out.push(crate::plugins::promotion::InternalRelationSnapshot {
                relation_id: *relation_id,
                source: relation.source,
                target: relation.target,
                relation_type: relation.relation_type.clone(),
                parameters: relation.parameters.clone(),
            });
        }
    }
    out
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

// === Spawn / despawn / metadata helpers ====================================

/// Spawn the new Occurrence entity for a promoted Definition.
///
/// `preserved_element_id` follows the `ElementIdPreservationPlan`:
/// when set, the existing entity holding that id is despawned and the
/// new Occurrence is spawned with the same `ElementId`, so external
/// references (other assemblies, relations, agent-held ids) survive
/// the promotion. When unset, a fresh `ElementId` is allocated from
/// the world's `ElementIdAllocator`.
///
/// Returns the `ElementId` of the new Occurrence.
pub fn spawn_promoted_occurrence(
    world: &mut World,
    definition_id: crate::plugins::modeling::definition::DefinitionId,
    definition_version: crate::plugins::modeling::definition::DefinitionVersion,
    preserved_element_id: Option<ElementId>,
) -> ElementId {
    let new_id = match preserved_element_id {
        Some(id) => {
            if let Some(entity) = find_entity_with_element_id(world, id) {
                world.entity_mut(entity).despawn();
            }
            id
        }
        None => {
            // `ElementIdAllocator::next_id` is `&self` (atomic), so a
            // shared resource borrow suffices.
            let allocator =
                world.resource::<crate::plugins::identity::ElementIdAllocator>();
            allocator.next_id()
        }
    };
    world.spawn((
        new_id,
        crate::plugins::modeling::occurrence::OccurrenceIdentity::new(
            definition_id,
            definition_version,
        ),
    ));
    new_id
}

/// Despawn every entity whose `ElementId` is in `source_members`,
/// except the one matching `except_preserved` (which is now the new
/// Occurrence). Returns the number of entities despawned.
pub fn despawn_source_members(
    world: &mut World,
    source_members: &[ElementId],
    except_preserved: Option<ElementId>,
) -> usize {
    let to_despawn: Vec<Entity> = {
        let Some(mut q) = world.try_query::<(Entity, &ElementId)>() else {
            return 0;
        };
        q.iter(world)
            .filter_map(|(entity, eid)| {
                if source_members.contains(eid) && Some(*eid) != except_preserved {
                    Some(entity)
                } else {
                    None
                }
            })
            .collect()
    };
    let count = to_despawn.len();
    for entity in to_despawn {
        world.entity_mut(entity).despawn();
    }
    count
}

/// Metadata block written onto the surviving assembly's
/// `SemanticAssembly.metadata` field per the
/// `ASSEMBLY_TO_DEFINITION_BRIDGE_AGREEMENT.md` "SemanticAssembly
/// Survival" section.
#[derive(Debug, Clone)]
pub struct AssemblyPromotionMetadata {
    pub promoted_definition_id: crate::plugins::modeling::definition::DefinitionId,
    pub promoted_occurrence_id: ElementId,
    /// Opaque ref into AuthoringScript provenance for the original
    /// member graph snapshot. The provenance store is not yet
    /// formalized; for now this is a free-form id the caller chooses
    /// (e.g. the `DefinitionDraftId` or a uuid).
    pub source_member_snapshot_ref: Option<String>,
    pub source_relation_snapshot_ref: Option<String>,
}

/// Merge `metadata` into the surviving assembly's
/// `SemanticAssembly.metadata` JSON object. Existing keys are
/// preserved; the four agreement-defined keys are written
/// unconditionally.
pub fn write_assembly_promotion_metadata(
    world: &mut World,
    source_assembly_id: ElementId,
    metadata: &AssemblyPromotionMetadata,
) -> Result<(), AssemblyCommitError> {
    let entity = find_entity_with_element_id(world, source_assembly_id)
        .ok_or(AssemblyCommitError::AssemblyNotFound {
            assembly_id: source_assembly_id,
        })?;
    if !world.entity(entity).contains::<SemanticAssembly>() {
        return Err(AssemblyCommitError::UnexpectedEntityShape {
            entity_id: source_assembly_id,
            expected: "SemanticAssembly",
        });
    }
    let mut assembly = world.get_mut::<SemanticAssembly>(entity).expect("checked");

    let mut object = match assembly.metadata.take() {
        serde_json::Value::Object(map) => map,
        // Replace any prior non-object value (Null, etc.) with a fresh
        // map. The agreement requires the four keys to live as named
        // metadata; non-object values would shadow that.
        _ => serde_json::Map::new(),
    };
    object.insert(
        "promoted_definition_id".into(),
        serde_json::Value::String(metadata.promoted_definition_id.0.clone()),
    );
    object.insert(
        "promoted_occurrence_id".into(),
        serde_json::json!(metadata.promoted_occurrence_id),
    );
    if let Some(snapshot) = &metadata.source_member_snapshot_ref {
        object.insert(
            "source_member_snapshot_ref".into(),
            serde_json::Value::String(snapshot.clone()),
        );
    }
    if let Some(snapshot) = &metadata.source_relation_snapshot_ref {
        object.insert(
            "source_relation_snapshot_ref".into(),
            serde_json::Value::String(snapshot.clone()),
        );
    }
    assembly.metadata = serde_json::Value::Object(object);
    Ok(())
}

// === Full commit orchestrator ==============================================

/// Configuration for `commit_assembly_promotion`. The caller fills
/// this from the `PromotionPlan` (definition id + version), the
/// `PromotionEmissionRecord` (which entry of `identity_map` to
/// preserve), and from AuthoringScript provenance (snapshot refs).
#[derive(Debug, Clone)]
pub struct AssemblyCommitConfig {
    pub source_assembly_id: ElementId,
    pub source_member_ids: Vec<ElementId>,
    pub promoted_definition_id: crate::plugins::modeling::definition::DefinitionId,
    pub promoted_definition_version:
        crate::plugins::modeling::definition::DefinitionVersion,
    pub preserved_element_id: Option<ElementId>,
    pub source_member_snapshot_ref: Option<String>,
    pub source_relation_snapshot_ref: Option<String>,
}

/// Result of a successful `commit_assembly_promotion` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedAssemblyPromotion {
    pub new_occurrence_id: ElementId,
    pub applied: AppliedAssemblyMigration,
    pub source_members_despawned: usize,
}

/// Full world-side commit cycle for a SemanticAssembly Make Reusable
/// flow. Orchestrates spawn -> apply (slice B2) -> despawn -> metadata
/// in one call so the MCP layer (slice B3b) only needs a single
/// commit invocation.
///
/// The diff is the same shape produced by
/// `SemanticAssemblyPromotionSource::build_plan_and_diff`. The
/// `source_member_ids` list is the union of every member id that the
/// adapter recorded; on commit, all of those entities are despawned
/// (except the one preserved as the new Occurrence). External
/// references to preserved-but-now-occurrence ids are still valid
/// because `spawn_promoted_occurrence` reuses the ElementId.
pub fn commit_assembly_promotion(
    world: &mut World,
    diff: &crate::plugins::promotion::SemanticGraphMigrationDiff,
    config: AssemblyCommitConfig,
) -> Result<CommittedAssemblyPromotion, AssemblyCommitError> {
    // 1. Spawn the new Occurrence — possibly reusing a preserved id.
    let new_occ = spawn_promoted_occurrence(
        world,
        config.promoted_definition_id.clone(),
        config.promoted_definition_version,
        config.preserved_element_id,
    );

    // 2. Apply the migration diff (slice B2). Note that the source
    //    assembly's `from_members` may include the preserved id; that's
    //    fine — `apply_assembly_migration_diff` filters by the
    //    `from_members` set it iterates, not by entity existence.
    let applied = apply_assembly_migration_diff(world, diff, new_occ)?;

    // 3. Despawn the leftover source members (those NOT preserved as
    //    the new Occurrence). spawn_promoted_occurrence already
    //    despawned the preserved id's old entity, so it's safe to skip
    //    it here.
    let despawned =
        despawn_source_members(world, &config.source_member_ids, Some(new_occ));

    // 4. Write the agreement metadata block onto the surviving
    //    assembly. `apply_assembly_migration_diff` already collapsed
    //    the source assembly's members; now we tag it with provenance.
    let metadata = AssemblyPromotionMetadata {
        promoted_definition_id: config.promoted_definition_id.clone(),
        promoted_occurrence_id: new_occ,
        source_member_snapshot_ref: config.source_member_snapshot_ref.clone(),
        source_relation_snapshot_ref: config.source_relation_snapshot_ref.clone(),
    };
    write_assembly_promotion_metadata(world, config.source_assembly_id, &metadata)?;

    Ok(CommittedAssemblyPromotion {
        new_occurrence_id: new_occ,
        applied,
        source_members_despawned: despawned,
    })
}

// === Template materialization (PP-A2DB-2 slice C3) =========================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializationError {
    /// A `RelationEndpoint::Slot(slot_id)` referenced a slot that the
    /// caller's realization map didn't resolve. The agreement requires
    /// templates with unresolved endpoints to surface as an error
    /// rather than silently spawn dangling relations.
    UnknownSlot { slot_id: String },
    /// `ElementIdAllocator` resource is not present in the world.
    /// Materialization needs it to mint fresh ids for the new
    /// `SemanticRelation` entities.
    AllocatorMissing,
}

impl std::fmt::Display for MaterializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSlot { slot_id } => write!(
                f,
                "materialize: template references unknown slot id '{slot_id}'"
            ),
            Self::AllocatorMissing => write!(
                f,
                "materialize: ElementIdAllocator resource missing from world"
            ),
        }
    }
}

impl std::error::Error for MaterializationError {}

/// Walk every `SemanticRelationTemplate` on the given Definition's
/// compound body and spawn one authored `SemanticRelation` entity per
/// template, with endpoints resolved through the supplied
/// `slot_id_to_realization_id` map. `RelationEndpoint::SelfRoot`
/// resolves to `occurrence_root_id`. Returns the number of relations
/// spawned.
///
/// Caller responsibilities:
///
/// - Provide `slot_id_to_realization_id` covering every slot the
///   templates reference. Unknown slot ids surface as
///   `MaterializationError::UnknownSlot`.
/// - Have an `ElementIdAllocator` resource in the world (the same
///   allocator used elsewhere for new ids).
///
/// Slice C4 will wire this into the actual Occurrence-creation
/// pipeline so it runs automatically; for slice C3 it's a free-
/// standing primitive callers invoke after they spawn an Occurrence.
pub fn materialize_relation_templates(
    world: &mut World,
    definition: &crate::plugins::modeling::definition::Definition,
    occurrence_root_id: ElementId,
    slot_id_to_realization_id: &std::collections::HashMap<String, ElementId>,
) -> Result<usize, MaterializationError> {
    let Some(compound) = definition.compound.as_ref() else {
        return Ok(0);
    };
    if compound.relation_templates.is_empty() {
        return Ok(0);
    }

    // Pre-resolve every endpoint so we surface `UnknownSlot` errors
    // before mutating the world. This keeps materialization atomic
    // from the caller's point of view: either every template spawns
    // or none do.
    let mut resolved: Vec<(ElementId, ElementId, &crate::plugins::promotion::SemanticRelationTemplate)> =
        Vec::with_capacity(compound.relation_templates.len());
    for template in &compound.relation_templates {
        let source = resolve_template_endpoint(
            &template.subject,
            occurrence_root_id,
            slot_id_to_realization_id,
        )?;
        let target = resolve_template_endpoint(
            &template.object,
            occurrence_root_id,
            slot_id_to_realization_id,
        )?;
        resolved.push((source, target, template));
    }

    if !world.contains_resource::<crate::plugins::identity::ElementIdAllocator>() {
        return Err(MaterializationError::AllocatorMissing);
    }

    let mut count = 0usize;
    for (source, target, template) in resolved {
        let relation_id = world
            .resource::<crate::plugins::identity::ElementIdAllocator>()
            .next_id();
        world.spawn((
            relation_id,
            crate::plugins::modeling::assembly::SemanticRelation {
                source,
                target,
                relation_type: template.relation_type.clone(),
                parameters: template.parameters.clone(),
            },
        ));
        count += 1;
    }
    Ok(count)
}

fn resolve_template_endpoint(
    endpoint: &crate::plugins::promotion::RelationEndpoint,
    occurrence_root_id: ElementId,
    slot_id_to_realization_id: &std::collections::HashMap<String, ElementId>,
) -> Result<ElementId, MaterializationError> {
    use crate::plugins::promotion::RelationEndpoint;
    match endpoint {
        RelationEndpoint::SelfRoot => Ok(occurrence_root_id),
        RelationEndpoint::Slot(slot_id) => slot_id_to_realization_id
            .get(slot_id)
            .copied()
            .ok_or_else(|| MaterializationError::UnknownSlot {
                slot_id: slot_id.clone(),
            }),
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
            candidate_relation_templates: Vec::new(),
            preserved_relations: Vec::new(),
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
            candidate_relation_templates: Vec::new(),
            preserved_relations: Vec::new(),
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
            candidate_relation_templates: Vec::new(),
            preserved_relations: Vec::new(),
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
            candidate_relation_templates: Vec::new(),
            preserved_relations: Vec::new(),
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

    // === spawn / despawn / metadata helpers ================================

    fn world_with_allocator() -> World {
        let mut world = World::new();
        world.insert_resource(crate::plugins::identity::ElementIdAllocator::default());
        world
    }

    fn count_entities_with(world: &mut World, target: ElementId) -> usize {
        let mut q = world.query::<&ElementId>();
        q.iter(&world).filter(|eid| **eid == target).count()
    }

    #[test]
    fn spawn_promoted_occurrence_with_fresh_id_allocates_via_resource() {
        let mut world = world_with_allocator();
        // Bump the allocator past 0 so the test can verify we got a
        // truly fresh id, not just `ElementId(0)`.
        world
            .resource_mut::<crate::plugins::identity::ElementIdAllocator>()
            .set_next(50);
        let new_id = spawn_promoted_occurrence(
            &mut world,
            DefinitionId("lib.window".into()),
            7u32,
            None,
        );
        assert_eq!(new_id, ElementId(50));
        // New entity carries OccurrenceIdentity referencing the right
        // definition id and version.
        let mut q = world.query::<(&ElementId, &OccurrenceIdentity)>();
        let (_, identity) = q
            .iter(&world)
            .find(|(eid, _)| **eid == new_id)
            .expect("new occurrence entity exists");
        assert_eq!(identity.definition_id, DefinitionId("lib.window".into()));
        assert_eq!(identity.definition_version, 7u32);
    }

    #[test]
    fn spawn_promoted_occurrence_preserves_existing_element_id() {
        let mut world = world_with_allocator();
        // Pre-existing entity with ElementId(7); should be despawned
        // and a new Occurrence spawned in its place with the same id.
        spawn_authored_leaf(&mut world, elem(7));
        let new_id = spawn_promoted_occurrence(
            &mut world,
            DefinitionId("lib.x".into()),
            1u32,
            Some(elem(7)),
        );
        assert_eq!(new_id, elem(7));
        // Exactly one entity with id 7 — the old one was despawned.
        assert_eq!(count_entities_with(&mut world, elem(7)), 1);
        // It carries OccurrenceIdentity now.
        let mut q = world.query::<(&ElementId, &OccurrenceIdentity)>();
        assert!(q.iter(&world).any(|(eid, _)| *eid == elem(7)));
    }

    #[test]
    fn spawn_promoted_occurrence_with_unmatched_preserved_id_still_spawns() {
        let mut world = world_with_allocator();
        // No entity has id 999. Caller asked to preserve it anyway —
        // spawn behaves as a fresh spawn under that id.
        let new_id = spawn_promoted_occurrence(
            &mut world,
            DefinitionId("lib.x".into()),
            1u32,
            Some(elem(999)),
        );
        assert_eq!(new_id, elem(999));
        let mut q = world.query::<(&ElementId, &OccurrenceIdentity)>();
        assert!(q.iter(&world).any(|(eid, _)| *eid == elem(999)));
    }

    #[test]
    fn despawn_source_members_skips_preserved_id() {
        let mut world = world_with_allocator();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(11));
        spawn_authored_leaf(&mut world, elem(12));
        // Imagine elem(11) became the new Occurrence; despawn should
        // leave it.
        let despawned = despawn_source_members(
            &mut world,
            &[elem(10), elem(11), elem(12)],
            Some(elem(11)),
        );
        assert_eq!(despawned, 2);
        assert_eq!(count_entities_with(&mut world, elem(10)), 0);
        assert_eq!(count_entities_with(&mut world, elem(11)), 1);
        assert_eq!(count_entities_with(&mut world, elem(12)), 0);
    }

    #[test]
    fn write_assembly_promotion_metadata_writes_four_keys_and_preserves_existing() {
        let mut world = world_with_allocator();
        // spawn_assembly's last arg is `parameters`, so we set
        // `metadata` separately via a Bevy mutation right after spawn
        // to seed a pre-existing metadata key.
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![],
            serde_json::Value::Null,
        );
        {
            let mut q = world.query::<(&ElementId, &mut SemanticAssembly)>();
            for (eid, mut assembly) in q.iter_mut(&mut world) {
                if *eid == elem(1) {
                    assembly.metadata = serde_json::json!({ "preexisting": "kept" });
                    break;
                }
            }
        }
        let metadata = AssemblyPromotionMetadata {
            promoted_definition_id: DefinitionId("draft-foo".into()),
            promoted_occurrence_id: elem(500),
            source_member_snapshot_ref: Some("snap-mem".into()),
            source_relation_snapshot_ref: Some("snap-rel".into()),
        };
        write_assembly_promotion_metadata(&mut world, elem(1), &metadata).unwrap();
        let mut q = world.query::<(&ElementId, &SemanticAssembly)>();
        let (_, assembly) = q
            .iter(&world)
            .find(|(eid, _)| **eid == elem(1))
            .unwrap();
        let m = assembly.metadata.as_object().unwrap();
        assert_eq!(m["promoted_definition_id"], serde_json::json!("draft-foo"));
        assert_eq!(m["promoted_occurrence_id"], serde_json::json!(elem(500)));
        assert_eq!(m["source_member_snapshot_ref"], serde_json::json!("snap-mem"));
        assert_eq!(m["source_relation_snapshot_ref"], serde_json::json!("snap-rel"));
        // Pre-existing metadata key is preserved.
        assert_eq!(m["preexisting"], serde_json::json!("kept"));
    }

    #[test]
    fn write_assembly_promotion_metadata_replaces_non_object_metadata() {
        let mut world = world_with_allocator();
        spawn_assembly(
            &mut world,
            elem(1),
            "house",
            "H",
            vec![],
            serde_json::Value::Null, // not an object
        );
        let metadata = AssemblyPromotionMetadata {
            promoted_definition_id: DefinitionId("draft-foo".into()),
            promoted_occurrence_id: elem(500),
            source_member_snapshot_ref: None,
            source_relation_snapshot_ref: None,
        };
        write_assembly_promotion_metadata(&mut world, elem(1), &metadata).unwrap();
        let mut q = world.query::<(&ElementId, &SemanticAssembly)>();
        let (_, assembly) = q
            .iter(&world)
            .find(|(eid, _)| **eid == elem(1))
            .unwrap();
        let m = assembly.metadata.as_object().unwrap();
        assert_eq!(m.len(), 2); // only the two non-Optional keys
        assert!(m.contains_key("promoted_definition_id"));
        assert!(m.contains_key("promoted_occurrence_id"));
    }

    #[test]
    fn commit_assembly_promotion_orchestrates_full_world_cycle() {
        let mut world = world_with_allocator();
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
        let diff = crate::plugins::promotion::SemanticGraphMigrationDiff {
            retargeted_assemblies: vec![
                crate::plugins::promotion::RetargetedAssembly {
                    assembly_id: elem(1),
                    from_members: vec![elem(10), elem(11)],
                    to_slot_ids: vec![String::new()],
                },
            ],
            retargeted_relations: Vec::new(),
            orphaned_memberships: Vec::new(),
            candidate_relation_templates: Vec::new(),
            preserved_relations: Vec::new(),
            warnings: Vec::new(),
        };

        let committed = commit_assembly_promotion(
            &mut world,
            &diff,
            AssemblyCommitConfig {
                source_assembly_id: elem(1),
                source_member_ids: vec![elem(10), elem(11)],
                promoted_definition_id: DefinitionId("draft-house".into()),
                promoted_definition_version: 1u32,
                preserved_element_id: Some(elem(10)),
                source_member_snapshot_ref: Some("snap-1".into()),
                source_relation_snapshot_ref: None,
            },
        )
        .expect("commit succeeds");

        // 1. New Occurrence id reuses the preserved member id.
        assert_eq!(committed.new_occurrence_id, elem(10));
        // 2. Source assembly collapsed to a single realization member.
        assert_eq!(committed.applied.source_members_collapsed, 2);
        // 3. The non-preserved source member (elem 11) was despawned.
        assert_eq!(committed.source_members_despawned, 1);
        assert_eq!(count_entities_with(&mut world, elem(11)), 0);
        // 4. The preserved id now hosts an OccurrenceIdentity (the
        //    old leaf was despawned; new occ took its id).
        let mut q = world.query::<(&ElementId, &OccurrenceIdentity)>();
        let (_, identity) =
            q.iter(&world).find(|(eid, _)| **eid == elem(10)).unwrap();
        assert_eq!(identity.definition_id, DefinitionId("draft-house".into()));
        // 5. Surviving assembly's metadata block was written.
        let mut qa = world.query::<(&ElementId, &SemanticAssembly)>();
        let (_, survivor) =
            qa.iter(&world).find(|(eid, _)| **eid == elem(1)).unwrap();
        let m = survivor.metadata.as_object().unwrap();
        assert_eq!(m["promoted_definition_id"], serde_json::json!("draft-house"));
        assert_eq!(m["promoted_occurrence_id"], serde_json::json!(elem(10)));
        assert_eq!(m["source_member_snapshot_ref"], serde_json::json!("snap-1"));
        // 6. Surviving assembly points at the new Occurrence with role
        //    "realization".
        assert_eq!(survivor.members.len(), 1);
        assert_eq!(survivor.members[0].target, elem(10));
        assert_eq!(survivor.members[0].role, "realization");
    }

    #[test]
    fn commit_assembly_promotion_without_preservation_allocates_fresh_id() {
        let mut world = world_with_allocator();
        world
            .resource_mut::<crate::plugins::identity::ElementIdAllocator>()
            .set_next(900);
        spawn_authored_leaf(&mut world, elem(10));
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
            ],
            retargeted_relations: Vec::new(),
            orphaned_memberships: Vec::new(),
            candidate_relation_templates: Vec::new(),
            preserved_relations: Vec::new(),
            warnings: Vec::new(),
        };
        let committed = commit_assembly_promotion(
            &mut world,
            &diff,
            AssemblyCommitConfig {
                source_assembly_id: elem(1),
                source_member_ids: vec![elem(10)],
                promoted_definition_id: DefinitionId("draft-x".into()),
                promoted_definition_version: 1u32,
                preserved_element_id: None,
                source_member_snapshot_ref: None,
                source_relation_snapshot_ref: None,
            },
        )
        .expect("commit succeeds");
        // Fresh id from the allocator.
        assert_eq!(committed.new_occurrence_id, ElementId(900));
        // All source members despawned; new occ at fresh id.
        assert_eq!(committed.source_members_despawned, 1);
        assert_eq!(count_entities_with(&mut world, elem(10)), 0);
        assert_eq!(count_entities_with(&mut world, ElementId(900)), 1);
    }

    // === PP-A2DB-2 slice A: gather collects internal relations =============

    #[test]
    fn gather_collects_internal_relations_for_template_classification() {
        let mut world = world_with_allocator();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(11));
        spawn_assembly(
            &mut world,
            elem(1),
            "door",
            "D",
            vec![member(elem(10), "frame"), member(elem(11), "leaf")],
            serde_json::Value::Null,
        );
        // Internal: both endpoints are source members.
        world.spawn((
            elem(30),
            SemanticRelation {
                source: elem(10),
                target: elem(11),
                relation_type: "hinges_on".into(),
                parameters: serde_json::json!({ "hinge_count": 3 }),
            },
        ));
        // Internal: source is the assembly itself, target is a member.
        world.spawn((
            elem(31),
            SemanticRelation {
                source: elem(1),
                target: elem(10),
                relation_type: "contains".into(),
                parameters: serde_json::Value::Null,
            },
        ));
        // External: target is outside the source.
        spawn_authored_leaf(&mut world, elem(99));
        world.spawn((
            elem(32),
            SemanticRelation {
                source: elem(10),
                target: elem(99),
                relation_type: "anchored_to".into(),
                parameters: serde_json::Value::Null,
            },
        ));

        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        // Two internal relations gathered; the boundary-spanning one is
        // captured by external_graph.relations instead.
        let internal_ids: Vec<ElementId> = input
            .internal_relations
            .iter()
            .map(|r| r.relation_id)
            .collect();
        // Both fully-internal relations (30, 31) and the boundary-
        // spanning relation (32) appear in `internal_relations` —
        // gather collects every relation that touches the source so
        // slice B's classifier can decide its fate. The adapter
        // routes fully-internal relations into template candidates
        // and partial-touch ones into `preserved_relations`.
        assert!(internal_ids.contains(&elem(30)));
        assert!(internal_ids.contains(&elem(31)));
        assert!(internal_ids.contains(&elem(32)));
        // The boundary-spanning relation is *also* in
        // `external_graph.relations` so the migration diff's
        // `retargeted_relations` rule still fires for it.
        assert!(input
            .external_graph
            .relations
            .iter()
            .any(|r| r.relation_id == elem(32)));

        // The relation parameters should be preserved verbatim on the
        // internal snapshot.
        let r30 = input
            .internal_relations
            .iter()
            .find(|r| r.relation_id == elem(30))
            .unwrap();
        assert_eq!(r30.parameters["hinge_count"], serde_json::json!(3));
    }

    #[test]
    fn gathered_input_drives_relation_template_classification_end_to_end() {
        let mut world = world_with_allocator();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(11));
        spawn_assembly(
            &mut world,
            elem(1),
            "door",
            "D",
            vec![member(elem(10), "frame"), member(elem(11), "leaf")],
            serde_json::Value::Null,
        );
        world.spawn((
            elem(30),
            SemanticRelation {
                source: elem(10),
                target: elem(11),
                relation_type: "hinges_on".into(),
                parameters: serde_json::Value::Null,
            },
        ));

        let input = gather_semantic_assembly_input(&world, elem(1)).unwrap();
        let adapter = crate::plugins::promotion::SemanticAssemblyPromotionSource {
            name: "Door".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let out = adapter.build_plan_and_diff(input).unwrap();
        assert_eq!(out.migration_diff.candidate_relation_templates.len(), 1);
        let template = &out.migration_diff.candidate_relation_templates[0];
        assert_eq!(
            template.subject,
            crate::plugins::promotion::RelationEndpoint::Slot("frame".into())
        );
        assert_eq!(
            template.object,
            crate::plugins::promotion::RelationEndpoint::Slot("leaf".into())
        );
    }

    // === PP-A2DB-2 slice C3: template materialization ======================

    fn template_definition(
        templates: Vec<crate::plugins::promotion::SemanticRelationTemplate>,
    ) -> crate::plugins::modeling::definition::Definition {
        crate::plugins::definition_authoring::blank_definition("TemplateHost")
            .with_relation_templates(templates)
    }

    fn template(
        subject: crate::plugins::promotion::RelationEndpoint,
        relation_type: &str,
        object: crate::plugins::promotion::RelationEndpoint,
    ) -> crate::plugins::promotion::SemanticRelationTemplate {
        crate::plugins::promotion::SemanticRelationTemplate {
            subject,
            relation_type: relation_type.to_string(),
            object,
            parameters: serde_json::Value::Null,
            source_relation_id: ElementId(0),
        }
    }

    #[test]
    fn materialize_returns_zero_when_definition_has_no_templates() {
        let mut world = world_with_allocator();
        let def = crate::plugins::definition_authoring::blank_definition("Empty");
        let count = materialize_relation_templates(
            &mut world,
            &def,
            elem(100),
            &std::collections::HashMap::new(),
        )
        .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn materialize_spawns_one_relation_per_template() {
        use crate::plugins::promotion::RelationEndpoint;
        let mut world = world_with_allocator();
        let def = template_definition(vec![
            template(
                RelationEndpoint::SelfRoot,
                "contains",
                RelationEndpoint::Slot("frame".into()),
            ),
            template(
                RelationEndpoint::Slot("frame".into()),
                "hinges_on",
                RelationEndpoint::Slot("leaf".into()),
            ),
        ]);
        let mut realizations = std::collections::HashMap::new();
        realizations.insert("frame".into(), elem(10));
        realizations.insert("leaf".into(), elem(11));
        let occ = elem(500);

        let count = materialize_relation_templates(&mut world, &def, occ, &realizations)
            .unwrap();
        assert_eq!(count, 2);

        let mut q = world.query::<&SemanticRelation>();
        let relations: Vec<&SemanticRelation> = q.iter(&world).collect();
        assert_eq!(relations.len(), 2);
        // Look up by relation_type since spawn order isn't part of
        // the contract.
        let contains = relations
            .iter()
            .find(|r| r.relation_type == "contains")
            .expect("`contains` relation spawned");
        assert_eq!(contains.source, occ); // SelfRoot -> occurrence root
        assert_eq!(contains.target, elem(10)); // Slot("frame") -> realization
        let hinges_on = relations
            .iter()
            .find(|r| r.relation_type == "hinges_on")
            .expect("`hinges_on` relation spawned");
        assert_eq!(hinges_on.source, elem(10));
        assert_eq!(hinges_on.target, elem(11));
    }

    #[test]
    fn materialize_returns_unknown_slot_error_and_does_not_mutate_world() {
        use crate::plugins::promotion::RelationEndpoint;
        let mut world = world_with_allocator();
        let def = template_definition(vec![template(
            RelationEndpoint::SelfRoot,
            "needs_unknown",
            RelationEndpoint::Slot("ghost".into()),
        )]);
        let realizations = std::collections::HashMap::new();
        let err = materialize_relation_templates(&mut world, &def, elem(500), &realizations)
            .unwrap_err();
        assert_eq!(
            err,
            MaterializationError::UnknownSlot { slot_id: "ghost".into() }
        );
        // No relation entities spawned.
        let mut q = world.query::<&SemanticRelation>();
        assert_eq!(q.iter(&world).count(), 0);
    }

    #[test]
    fn materialize_returns_allocator_missing_when_resource_absent() {
        use crate::plugins::promotion::RelationEndpoint;
        // World without the ElementIdAllocator resource.
        let mut world = World::new();
        let def = template_definition(vec![template(
            RelationEndpoint::SelfRoot,
            "contains",
            RelationEndpoint::Slot("frame".into()),
        )]);
        let mut realizations = std::collections::HashMap::new();
        realizations.insert("frame".into(), elem(10));
        let err = materialize_relation_templates(&mut world, &def, elem(500), &realizations)
            .unwrap_err();
        assert_eq!(err, MaterializationError::AllocatorMissing);
    }

    #[test]
    fn materialize_carries_template_parameters_verbatim_into_relation() {
        use crate::plugins::promotion::RelationEndpoint;
        let mut world = world_with_allocator();
        let mut t = template(
            RelationEndpoint::SelfRoot,
            "load_path",
            RelationEndpoint::Slot("post".into()),
        );
        t.parameters = serde_json::json!({ "shear_kn": 12.5 });
        let def = template_definition(vec![t]);
        let mut realizations = std::collections::HashMap::new();
        realizations.insert("post".into(), elem(20));

        materialize_relation_templates(&mut world, &def, elem(500), &realizations)
            .expect("materialize succeeds");
        let mut q = world.query::<&SemanticRelation>();
        let r = q.iter(&world).next().expect("one relation spawned");
        assert_eq!(r.parameters["shear_kn"], serde_json::json!(12.5));
    }

    #[test]
    fn definition_with_relation_templates_round_trips_through_serde() {
        use crate::plugins::promotion::RelationEndpoint;
        let def = template_definition(vec![template(
            RelationEndpoint::SelfRoot,
            "contains",
            RelationEndpoint::Slot("frame".into()),
        )]);
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("relation_templates"));
        let back: crate::plugins::modeling::definition::Definition =
            serde_json::from_str(&json).unwrap();
        let compound = back.compound.expect("compound body survives reload");
        assert_eq!(compound.relation_templates.len(), 1);
        assert_eq!(compound.relation_templates[0].relation_type, "contains");
    }

    #[test]
    fn empty_relation_templates_are_skipped_in_serialization() {
        // Sanity: a Definition without templates must not start
        // emitting `relation_templates: []` after this PP. Keeps
        // existing project files bit-stable.
        let def = crate::plugins::definition_authoring::blank_definition("NoTemplates");
        let json = serde_json::to_string(&def).unwrap();
        assert!(
            !json.contains("relation_templates"),
            "an empty list should not appear in the serialized form; got: {json}"
        );
    }

    // === PP-A2DB-2 slice C4b: capability-registry-driven rules =============

    fn relation_descriptor_with_classification(
        relation_type: &str,
        classification: Option<crate::plugins::promotion::ExternalRelationClassification>,
    ) -> crate::capability_registry::RelationTypeDescriptor {
        crate::capability_registry::RelationTypeDescriptor {
            relation_type: relation_type.into(),
            label: relation_type.into(),
            description: String::new(),
            valid_source_types: Vec::new(),
            valid_target_types: Vec::new(),
            parameter_schema: serde_json::Value::Null,
            participates_in_dependency_graph: false,
            external_classification: classification,
        }
    }

    #[test]
    fn gather_with_capability_seeds_rules_from_registered_relation_descriptors() {
        use crate::capability_registry::CapabilityRegistry;
        use crate::plugins::promotion::ExternalRelationClassification;
        let mut world = world_with_allocator();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_assembly(
            &mut world,
            elem(1),
            "door",
            "Door",
            vec![member(elem(10), "frame")],
            serde_json::Value::Null,
        );

        let mut registry = CapabilityRegistry::default();
        registry.register_relation_type(relation_descriptor_with_classification(
            "hosted_on_wall",
            Some(ExternalRelationClassification::HostContract),
        ));
        registry.register_relation_type(relation_descriptor_with_classification(
            "needs_room",
            Some(ExternalRelationClassification::RequiredContext),
        ));
        // A descriptor without a classification — must NOT appear in
        // the seeded rules.
        registry.register_relation_type(relation_descriptor_with_classification(
            "ad_hoc",
            None,
        ));

        let input =
            gather_semantic_assembly_input_with_capability(&world, &registry, elem(1)).unwrap();
        assert_eq!(
            input.relation_classification.by_descriptor.len(),
            2,
            "only descriptors with Some(classification) are seeded"
        );
        assert_eq!(
            input
                .relation_classification
                .by_descriptor
                .get("hosted_on_wall"),
            Some(&ExternalRelationClassification::HostContract),
        );
        assert_eq!(
            input
                .relation_classification
                .by_descriptor
                .get("needs_room"),
            Some(&ExternalRelationClassification::RequiredContext),
        );
        assert!(input
            .relation_classification
            .by_descriptor
            .get("ad_hoc")
            .is_none());
        // `default_unknown` stays None — unknown descriptors must
        // surface as warnings, not silently classify.
        assert!(input.relation_classification.default_unknown.is_none());
    }

    #[test]
    fn gather_with_capability_returns_empty_rules_when_registry_has_no_classified_descriptors() {
        use crate::capability_registry::CapabilityRegistry;
        let mut world = world_with_allocator();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_assembly(
            &mut world,
            elem(1),
            "door",
            "Door",
            vec![member(elem(10), "frame")],
            serde_json::Value::Null,
        );

        let mut registry = CapabilityRegistry::default();
        // Single descriptor without classification.
        registry.register_relation_type(relation_descriptor_with_classification(
            "lonely",
            None,
        ));
        let input =
            gather_semantic_assembly_input_with_capability(&world, &registry, elem(1)).unwrap();
        assert!(input.relation_classification.by_descriptor.is_empty());
    }

    #[test]
    fn gather_with_capability_drives_adapter_classification_end_to_end() {
        use crate::capability_registry::CapabilityRegistry;
        use crate::plugins::promotion::{
            ExternalRelationClassification, SemanticAssemblyPromotionSource,
        };
        let mut world = world_with_allocator();
        spawn_authored_leaf(&mut world, elem(10));
        spawn_authored_leaf(&mut world, elem(20)); // outsider
        spawn_assembly(
            &mut world,
            elem(1),
            "door",
            "Door",
            vec![member(elem(10), "frame")],
            serde_json::Value::Null,
        );
        // Boundary-spanning relation: frame -> outsider with type
        // `hosted_on_wall`.
        world.spawn((
            elem(30),
            SemanticRelation {
                source: elem(10),
                target: elem(20),
                relation_type: "hosted_on_wall".into(),
                parameters: serde_json::Value::Null,
            },
        ));

        let mut registry = CapabilityRegistry::default();
        registry.register_relation_type(relation_descriptor_with_classification(
            "hosted_on_wall",
            Some(ExternalRelationClassification::HostContract),
        ));

        let input =
            gather_semantic_assembly_input_with_capability(&world, &registry, elem(1)).unwrap();
        let adapter = SemanticAssemblyPromotionSource {
            name: "Door".into(),
            replace_source: false,
            provenance: Default::default(),
        };
        let out = adapter.build_plan_and_diff(input).unwrap();
        // The boundary-spanning relation classifies as HostContract
        // and a corresponding ExternalContextRequirement appears on
        // the plan — without the test having to set the rules
        // manually.
        assert_eq!(out.plan.external_context_requirements.len(), 1);
        let req = &out.plan.external_context_requirements[0];
        assert_eq!(req.relation_type, "hosted_on_wall");
        assert_eq!(
            req.classification,
            ExternalRelationClassification::HostContract
        );
    }
}
