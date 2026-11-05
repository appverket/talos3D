use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::Serialize;
use serde_json::{json, Value};

#[cfg(test)]
use crate::plugins::persistence::deserialize_project_entity_records;
use crate::{
    authored_entity::{AuthoredEntity, BoxedEntity, EntityBounds},
    capability_registry::CapabilityRegistry,
    plugins::{
        command_registry::CommandResult,
        commands::{snapshot_dependency_order, ApplyEntityChangesCommand, CreateEntityCommand},
        document_state::DocumentState,
        history::{EditorCommand, History, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        layers::{LayerRegistry, ObjectVisibility},
        materials::{ensure_builtin_materials, MaterialRegistry, TextureRegistry},
        modeling::{
            definition::DefinitionRegistry,
            dependency_graph::stamp_authored_entity_dependencies,
            group::{
                collect_group_members_recursive, compose_snapshot_into_frame,
                compute_group_bounds_from_world, compute_group_bounds_in_frame_from_world,
                group_frame, group_frame_change_snapshots, is_group, GroupFrame, GroupMembers,
                GroupSnapshot, LinkedModelIdMapping, LinkedModelRef,
            },
            primitives::TriangleMesh,
            snapshots::{EditableMeshSnapshot, TriangleMeshSnapshot},
        },
        persistence::{
            apply_semantic_sidecars, capture_semantic_sidecars,
            deserialize_project_entity_records_with_assets, load_project_from_path,
            serialize_entity_records_as_project, PersistedEntityRecord, PersistedSemanticSidecars,
            ProjectEntityRecords,
        },
        selection::Selected,
        storage::Storage,
        transform::{
            apply_transform_plan_modifiers, TransformMode, TransformStartOptions, TransformState,
        },
    },
};

const RELOAD_INTERVAL_SECONDS: f32 = 1.0;

/// Domain-neutral description of one live relationship between an external
/// Talos3D document and its instance root in the current scene.
///
/// Domain capabilities consume this relationship instead of reaching into the
/// linked group's ECS components or treating the imported members as ordinary
/// copied geometry. Architecture-specific concepts such as a building or
/// foundation intentionally do not appear here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkedModelRelationship {
    pub relationship_kind: &'static str,
    pub instance_root_id: u64,
    pub instance_name: String,
    pub source_path: String,
    pub source_root_id: u64,
    pub source_kind: &'static str,
    pub loaded_content_hash: u64,
    pub frame: LinkedModelFrame,
    pub root_member_ids: Vec<u64>,
    pub source_to_scene_ids: Vec<LinkedModelRelationshipMapping>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkedModelFrame {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedModelRelationshipMapping {
    pub source_id: u64,
    pub scene_id: u64,
}

/// A domain-neutral placement view over a linked instance.
///
/// Source-owned scene ids are refreshed from the external document and must not
/// be edited individually. Host dependents belong to the containing document;
/// terrain foundations and other derived placement representations attach there
/// and survive source refresh without leaking into the linked source.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkedModelPlacementSubject {
    pub subject_kind: &'static str,
    pub relationship: LinkedModelRelationship,
    pub source_owned_scene_ids: Vec<u64>,
    pub host_dependent_scene_ids: Vec<u64>,
    pub bounds: LinkedModelPlacementBounds,
    pub dependency_anchor: LinkedModelDependencyAnchor,
    pub mapping_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinkedModelPlacementBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

/// Stable source-side identity used by host-owned dependent representations.
///
/// `scene_id` is deliberately not stored: refresh is allowed to reallocate a
/// scene id, and resolution follows the source id through the live mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LinkedModelDependencyAnchor {
    pub instance_root_id: u64,
    pub source_id: u64,
    pub observed_content_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResolvedLinkedModelDependencyAnchor {
    pub instance_root_id: u64,
    pub source_id: u64,
    pub scene_id: u64,
    pub loaded_content_hash: u64,
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedModelPlacementError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for LinkedModelPlacementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for LinkedModelPlacementError {}

/// A single rigid placement operation. Domain capabilities build this plan for
/// preview and commit instead of translating linked members independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinkedModelPlacementOperation {
    Translate { delta: Vec3 },
    Rotate { rotation: Quat, pivot: Option<Vec3> },
}

/// Authored before/after snapshots for one linked-instance placement.
///
/// The plan is pure until its command is queued. Callers may compose additional
/// host-owned dependent snapshots into the same command before committing.
#[derive(Clone)]
pub struct LinkedModelPlacementPlan {
    pub subject: LinkedModelPlacementSubject,
    before: Vec<BoxedEntity>,
    after: Vec<BoxedEntity>,
}

impl LinkedModelPlacementPlan {
    pub fn before(&self) -> &[BoxedEntity] {
        &self.before
    }

    pub fn after(&self) -> &[BoxedEntity] {
        &self.after
    }

    pub fn into_apply_command(self, label: &'static str) -> ApplyEntityChangesCommand {
        ApplyEntityChangesCommand {
            label,
            before: self.before,
            after: self.after,
        }
    }
}

/// Inspect one linked-model relationship without loading or mutating its
/// source document.
pub fn linked_model_relationship(
    world: &World,
    instance_root_id: ElementId,
) -> Result<LinkedModelRelationship, String> {
    let members = group_members(world, instance_root_id)
        .ok_or_else(|| format!("Group {} not found", instance_root_id.0))?;
    let linked = members.linked_model.as_ref().ok_or_else(|| {
        format!(
            "Group {} is not a linked model instance",
            instance_root_id.0
        )
    })?;
    let mapped_scene_ids = linked
        .source_to_scene_ids
        .iter()
        .map(|mapping| mapping.scene_id)
        .collect::<HashSet<_>>();
    let mut root_member_ids = members
        .member_ids
        .iter()
        .filter(|id| mapped_scene_ids.contains(id))
        .map(|id| id.0)
        .collect::<Vec<_>>();
    root_member_ids.sort_unstable();
    let mut source_to_scene_ids = linked
        .source_to_scene_ids
        .iter()
        .map(|mapping| LinkedModelRelationshipMapping {
            source_id: mapping.source_id.0,
            scene_id: mapping.scene_id.0,
        })
        .collect::<Vec<_>>();
    source_to_scene_ids.sort_by_key(|mapping| (mapping.source_id, mapping.scene_id));

    Ok(LinkedModelRelationship {
        relationship_kind: "linked_model_instance",
        instance_root_id: instance_root_id.0,
        instance_name: members.name.clone(),
        source_path: linked.path.clone(),
        source_root_id: linked.source_root_id.0,
        source_kind: if linked.source_root_is_document {
            "document"
        } else {
            "group"
        },
        loaded_content_hash: linked.content_hash,
        frame: LinkedModelFrame {
            translation: members.frame.translation.to_array(),
            rotation: members.frame.rotation.to_array(),
        },
        root_member_ids,
        source_to_scene_ids,
    })
}

fn placement_error(code: &'static str, message: impl Into<String>) -> LinkedModelPlacementError {
    LinkedModelPlacementError {
        code,
        message: message.into(),
    }
}

/// Resolve and validate the placement subject for a linked instance.
///
/// Validation is intentionally complete before any edit plan is built. A
/// missing/duplicate mapping therefore refuses safely without partial movement.
pub fn linked_model_placement_subject(
    world: &World,
    instance_root_id: ElementId,
) -> Result<LinkedModelPlacementSubject, LinkedModelPlacementError> {
    let relationship = linked_model_relationship(world, instance_root_id).map_err(|message| {
        placement_error(
            "not_applicable",
            format!(
                "Element {} is not an applicable linked-model placement subject: {message}",
                instance_root_id.0
            ),
        )
    })?;
    let members = group_members(world, instance_root_id).ok_or_else(|| {
        placement_error(
            "stale_mapping",
            format!("Linked instance root {} is missing", instance_root_id.0),
        )
    })?;

    let mut source_ids = HashSet::new();
    let mut scene_ids = HashSet::new();
    for mapping in &relationship.source_to_scene_ids {
        if !source_ids.insert(mapping.source_id) {
            return Err(placement_error(
                "stale_mapping",
                format!("Source id {} is mapped more than once", mapping.source_id),
            ));
        }
        if !scene_ids.insert(mapping.scene_id) {
            return Err(placement_error(
                "stale_mapping",
                format!("Scene id {} is mapped more than once", mapping.scene_id),
            ));
        }
        capture_snapshot(world, ElementId(mapping.scene_id)).map_err(|_| {
            placement_error(
                "stale_mapping",
                format!(
                    "Mapped source id {} points to missing scene id {}",
                    mapping.source_id, mapping.scene_id
                ),
            )
        })?;
    }
    if !relationship.source_to_scene_ids.iter().any(|mapping| {
        mapping.source_id == relationship.source_root_id
            && mapping.scene_id == relationship.instance_root_id
    }) {
        return Err(placement_error(
            "stale_mapping",
            format!(
                "Linked source root {} does not resolve to instance root {}",
                relationship.source_root_id, relationship.instance_root_id
            ),
        ));
    }

    let root_member_ids = members.member_ids.iter().copied().collect::<HashSet<_>>();
    for root_member_id in &relationship.root_member_ids {
        let root_member_id = ElementId(*root_member_id);
        if !root_member_ids.contains(&root_member_id) || !scene_ids.contains(&root_member_id.0) {
            return Err(placement_error(
                "stale_mapping",
                format!(
                    "Linked root member {} is absent from the current source mapping",
                    root_member_id.0
                ),
            ));
        }
    }

    let source_root_members = relationship
        .root_member_ids
        .iter()
        .copied()
        .map(ElementId)
        .collect::<Vec<_>>();
    let bounds = compute_group_bounds_from_world(world, &source_root_members).ok_or_else(|| {
        placement_error(
            "not_applicable",
            format!(
                "Linked instance {} has no resolved placement bounds",
                instance_root_id.0
            ),
        )
    })?;

    let mut source_owned_scene_ids = scene_ids.iter().copied().collect::<Vec<_>>();
    source_owned_scene_ids.sort_unstable();
    let mut host_dependent_scene_ids = members
        .member_ids
        .iter()
        .filter(|id| !scene_ids.contains(&id.0))
        .map(|id| id.0)
        .collect::<Vec<_>>();
    host_dependent_scene_ids.sort_unstable();

    let mut hasher = DefaultHasher::new();
    relationship.source_path.hash(&mut hasher);
    relationship.source_root_id.hash(&mut hasher);
    relationship.loaded_content_hash.hash(&mut hasher);
    for mapping in &relationship.source_to_scene_ids {
        mapping.source_id.hash(&mut hasher);
        mapping.scene_id.hash(&mut hasher);
    }

    Ok(LinkedModelPlacementSubject {
        subject_kind: "linked_model_placement_subject",
        dependency_anchor: LinkedModelDependencyAnchor {
            instance_root_id: relationship.instance_root_id,
            source_id: relationship.source_root_id,
            observed_content_hash: relationship.loaded_content_hash,
        },
        mapping_fingerprint: hasher.finish(),
        bounds: LinkedModelPlacementBounds {
            min: bounds.min.to_array(),
            max: bounds.max.to_array(),
        },
        relationship,
        source_owned_scene_ids,
        host_dependent_scene_ids,
    })
}

/// Resolve a source-stable dependency anchor against the current mapping.
/// A changed content hash reports `rebased`; removal of the anchored source id
/// is a structured stale-mapping error.
pub fn resolve_linked_model_dependency_anchor(
    world: &World,
    anchor: LinkedModelDependencyAnchor,
) -> Result<ResolvedLinkedModelDependencyAnchor, LinkedModelPlacementError> {
    let subject = linked_model_placement_subject(world, ElementId(anchor.instance_root_id))?;
    let mapping = subject
        .relationship
        .source_to_scene_ids
        .iter()
        .find(|mapping| mapping.source_id == anchor.source_id)
        .ok_or_else(|| {
            placement_error(
                "stale_mapping",
                format!(
                    "Anchored source id {} no longer exists in linked instance {}",
                    anchor.source_id, anchor.instance_root_id
                ),
            )
        })?;
    Ok(ResolvedLinkedModelDependencyAnchor {
        instance_root_id: anchor.instance_root_id,
        source_id: anchor.source_id,
        scene_id: mapping.scene_id,
        loaded_content_hash: subject.relationship.loaded_content_hash,
        status: if anchor.observed_content_hash == subject.relationship.loaded_content_hash {
            "current"
        } else {
            "rebased"
        },
    })
}

/// Find the linked instance that owns a source-derived scene entity.
pub fn linked_model_source_owner(
    world: &World,
    scene_id: ElementId,
) -> Option<(ElementId, ElementId)> {
    let mut roots = world
        .iter_entities()
        .filter_map(|entity_ref| {
            let root_id = *entity_ref.get::<ElementId>()?;
            let linked = entity_ref.get::<GroupMembers>()?.linked_model.as_ref()?;
            linked
                .source_to_scene_ids
                .iter()
                .find(|mapping| mapping.scene_id == scene_id)
                .map(|mapping| (root_id, mapping.source_id))
        })
        .collect::<Vec<_>>();
    roots.sort_by_key(|(root_id, source_id)| (root_id.0, source_id.0));
    roots.into_iter().next()
}

/// Build one atomic authored edit plan for translating or rotating a linked
/// instance. The same `after` snapshots can drive a live preview and the
/// committed [`ApplyEntityChangesCommand`].
pub fn plan_linked_model_placement(
    world: &World,
    instance_root_id: ElementId,
    operation: LinkedModelPlacementOperation,
) -> Result<LinkedModelPlacementPlan, LinkedModelPlacementError> {
    let subject = linked_model_placement_subject(world, instance_root_id)?;
    let all_member_ids = collect_group_members_recursive(world, instance_root_id);
    let leaf_ids = all_member_ids
        .iter()
        .copied()
        .filter(|id| !is_group(world, *id))
        .collect::<Vec<_>>();
    let pivot = match operation {
        LinkedModelPlacementOperation::Rotate {
            pivot: Some(pivot), ..
        } => pivot,
        _ => {
            let min = Vec3::from_array(subject.bounds.min);
            let max = Vec3::from_array(subject.bounds.max);
            (min + max) * 0.5
        }
    };

    let mut before = Vec::new();
    for leaf_id in leaf_ids {
        let snapshot = capture_snapshot(world, leaf_id).map_err(|message| {
            placement_error(
                "stale_mapping",
                format!(
                    "Could not capture placement member {}: {message}",
                    leaf_id.0
                ),
            )
        })?;
        before.push(snapshot);
    }
    use crate::plugins::transform::{rigid_after, TransformMotion};
    let motion = match operation {
        LinkedModelPlacementOperation::Translate { delta } => TransformMotion::Move(delta),
        LinkedModelPlacementOperation::Rotate { rotation, .. } => TransformMotion::Rotate(rotation),
    };
    let mut after = rigid_after(before.iter(), motion, pivot);

    let mut group_ids = vec![instance_root_id];
    group_ids.extend(
        all_member_ids
            .iter()
            .copied()
            .filter(|id| is_group(world, *id)),
    );
    group_ids.sort_by_key(|id| id.0);
    group_ids.dedup();
    for group_id in group_ids {
        let old = group_frame(world, group_id).unwrap_or_default();
        let new = match operation {
            LinkedModelPlacementOperation::Translate { delta } => GroupFrame {
                translation: old.translation + delta,
                rotation: old.rotation,
            },
            LinkedModelPlacementOperation::Rotate { rotation, .. } => GroupFrame {
                translation: pivot + rotation * (old.translation - pivot),
                rotation: rotation * old.rotation,
            },
        };
        if let Some((group_before, group_after)) =
            group_frame_change_snapshots(world, group_id, new)
        {
            before.push(group_before);
            after.push(group_after);
        }
    }

    let mode = match operation {
        LinkedModelPlacementOperation::Translate { .. } => TransformMode::Moving,
        LinkedModelPlacementOperation::Rotate { .. } => TransformMode::Rotating,
    };
    apply_transform_plan_modifiers(world, mode, &mut before, &mut after);
    before.sort_by_key(snapshot_dependency_order);
    after.sort_by_key(snapshot_dependency_order);

    Ok(LinkedModelPlacementPlan {
        subject,
        before,
        after,
    })
}

/// A native file insertion stays cancellable while its shared Move gesture is active.
#[derive(Resource)]
struct LinkedPlacementGesture {
    creation_revision: u64,
}

pub(crate) fn finish_linked_placement_gesture(world: &mut World, commit: bool) {
    let Some(gesture) = world.remove_resource::<LinkedPlacementGesture>() else {
        return;
    };
    if !commit && world.resource::<History>().model_revision() == gesture.creation_revision {
        // Only undo our own just-created instance. Never undo a later external edit.
        world.resource_mut::<PendingCommandQueue>().queue_undo();
    }
}

struct CreateLinkedModelInstanceHistoryCommand {
    sidecars: Vec<(ElementId, PersistedSemanticSidecars)>,
    hidden: Vec<ElementId>,
    snapshots: Vec<BoxedEntity>,
    instance_root_id: ElementId,
}

impl EditorCommand for CreateLinkedModelInstanceHistoryCommand {
    fn label(&self) -> &'static str {
        "Place linked model"
    }

    fn apply(&mut self, world: &mut World) {
        self.snapshots.sort_by_key(snapshot_dependency_order);
        for snapshot in &self.snapshots {
            snapshot.apply_to(world);
            stamp_authored_entity_dependencies(world, snapshot);
        }
        for (id, sidecars) in &self.sidecars {
            apply_semantic_sidecars(world, *id, sidecars);
        }
        if let Some(mut visibility) = world.get_resource_mut::<ObjectVisibility>() {
            visibility.hidden.extend(self.hidden.iter().copied());
        }
        select_only(world, self.instance_root_id);
    }

    fn undo(&mut self, world: &mut World) {
        self.snapshots.sort_by_key(snapshot_dependency_order);
        for snapshot in self.snapshots.iter().rev() {
            snapshot.remove_from(world);
        }
        if let Some(mut visibility) = world.get_resource_mut::<ObjectVisibility>() {
            for id in &self.hidden {
                visibility.hidden.remove(id);
            }
        }
    }
}

#[derive(Resource, Debug, Default)]
pub struct LinkedModelReloadState {
    elapsed: f32,
}

pub fn execute_create_linked_model(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let (group_id, created_group) = selected_or_new_group(world, params)?;
    let path = linked_model_path(world, params, group_id)?;
    let (source_root_id, bytes, hash, frame, member_ids, source_ids) =
        build_linked_model_file(world, group_id)?;

    let key = path.to_string_lossy().into_owned();
    world.resource::<Storage>().0.save(&bytes, &key)?;

    let before = capture_snapshot(world, group_id)?;
    let after = linked_group_snapshot(
        world,
        group_id,
        member_ids,
        frame,
        Some(LinkedModelRef {
            source_root_is_document: false,
            path: key.clone(),
            source_root_id,
            source_to_scene_ids: source_ids
                .iter()
                .copied()
                .map(|id| LinkedModelIdMapping {
                    source_id: id,
                    scene_id: id,
                })
                .collect(),
            content_hash: hash,
        }),
    )?;
    let mut before_snapshots = vec![before];
    let mut after_snapshots = vec![after];
    for id in source_ids.iter().copied().filter(|id| *id != group_id) {
        if !is_group(world, id) {
            continue;
        }
        let snapshot = capture_snapshot(world, id)?;
        let group = snapshot.0.as_any().downcast_ref::<GroupSnapshot>().unwrap();
        let mut cleaned = group.clone();
        cleaned.member_ids.retain(|id| source_ids.contains(id));
        if cleaned.member_ids != group.member_ids {
            before_snapshots.push(snapshot);
            after_snapshots.push(cleaned.into());
        }
    }
    for snapshot in &after_snapshots {
        snapshot.apply_to(world);
    }
    world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .write(ApplyEntityChangesCommand {
            label: "Create linked model",
            before: before_snapshots,
            after: after_snapshots,
        });
    let relationship = linked_model_relationship(world, group_id)?;

    Ok(CommandResult {
        created: created_group.into_iter().map(|id| id.0).collect(),
        modified: vec![group_id.0],
        deleted: Vec::new(),
        output: Some(json!({
            "path": key,
            "root_group_id": group_id.0,
            "source_root_id": source_root_id.0,
            "content_hash": hash,
            "frame": {
                "translation": [frame.translation.x, frame.translation.y, frame.translation.z],
                "rotation": [frame.rotation.x, frame.rotation.y, frame.rotation.z, frame.rotation.w],
            },
            "relationship": relationship,
        })),
    })
}

pub fn execute_open_linked_model(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let group_id = selected_linked_group(world, params)?;
    let linked = group_members(world, group_id)
        .and_then(|members| members.linked_model.clone())
        .ok_or_else(|| {
            format!(
                "Group {} is not a linked model. Use Create Linked Model first.",
                group_id.0
            )
        })?;
    let path = PathBuf::from(&linked.path);
    load_project_from_path(world, path.clone())?;

    Ok(CommandResult {
        modified: Vec::new(),
        output: Some(json!({
            "path": path.to_string_lossy(),
            "source_group_id": group_id.0,
        })),
        ..CommandResult::empty()
    })
}

/// Read-only command handler for UI, command-palette, and MCP consumers that
/// need the explicit linked-document/instance relationship.
pub fn execute_inspect_linked_models(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let group_ids = inspect_linked_group_ids(world, params)?;
    let relationships = group_ids
        .into_iter()
        .map(|group_id| {
            let relationship = linked_model_relationship(world, group_id)?;
            let source_state = linked_model_source_state(world, &relationship);
            Ok(json!({
                "relationship": relationship,
                "source_state": source_state,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(CommandResult {
        output: Some(json!({
            "relationship_kind": "linked_model_instance",
            "count": relationships.len(),
            "relationships": relationships,
        })),
        ..CommandResult::empty()
    })
}

/// Read-only structured applicability probe for placement capabilities.
pub fn execute_inspect_linked_model_placement_subject(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let instance_root_id = params
        .get("instance_root_id")
        .or_else(|| params.get("group_id"))
        .and_then(Value::as_u64)
        .map(ElementId)
        .or_else(|| {
            world
                .query_filtered::<(&ElementId, &GroupMembers), With<Selected>>()
                .iter(world)
                .find_map(|(id, members)| members.linked_model.is_some().then_some(*id))
        })
        .ok_or_else(|| {
            "Pass instance_root_id (or select one linked-model instance root)".to_string()
        })?;

    let output = match linked_model_placement_subject(world, instance_root_id) {
        Ok(subject) => json!({
            "status": "applicable",
            "subject": subject,
        }),
        Err(error) => json!({
            "status": error.code,
            "instance_root_id": instance_root_id.0,
            "message": error.message,
        }),
    };
    Ok(CommandResult {
        output: Some(output),
        ..CommandResult::empty()
    })
}

pub fn execute_place_linked_model(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    if world.get_resource::<PendingCommandQueue>().is_none() {
        return Err("Linked-model placement requires the command history plugin".to_string());
    }
    if params.get("path").is_none() {
        return request_linked_model_file(world, params);
    }
    let Some(path) = place_linked_model_path(world, params)? else {
        return Ok(CommandResult::empty());
    };
    let key = path.to_string_lossy().into_owned();
    let bytes = world.resource::<Storage>().0.load(&key)?;
    let hash = content_hash(&bytes);
    let mut linked_project = load_linked_project(world, &bytes, &key)?;
    let default_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Linked Model");
    let (root, source_root_is_document) =
        linked_project_root_group(&mut linked_project, default_name)?;
    let group_id = world.resource::<ElementIdAllocator>().next_id();
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(root.name.as_str())
        .to_string();
    let linked = LinkedModelRef {
        source_root_is_document,
        path: key.clone(),
        source_root_id: root.element_id,
        source_to_scene_ids: Vec::new(),
        content_hash: 0,
    };
    GroupSnapshot {
        element_id: group_id,
        name,
        member_ids: Vec::new(),
        frame: GroupFrame::identity(),
        composite: None,
        linked_model: Some(linked.clone()),
        cached_bounds: None,
    }
    .apply_to(world);

    apply_linked_snapshots(world, group_id, linked, hash, linked_project)?;
    select_only(world, group_id);

    let relationship = linked_model_relationship(world, group_id)?;
    let mut created = relationship
        .source_to_scene_ids
        .iter()
        .map(|mapping| ElementId(mapping.scene_id))
        .collect::<Vec<_>>();
    created.sort_by_key(|id| id.0);
    created.dedup();
    let mut snapshots = created
        .iter()
        .copied()
        .map(|id| capture_snapshot(world, id))
        .collect::<Result<Vec<_>, _>>()?;
    snapshots.sort_by_key(snapshot_dependency_order);
    let sidecars = created
        .iter()
        .filter_map(|id| {
            let entity = find_entity(world, *id)?;
            Some((*id, capture_semantic_sidecars(&world.entity(entity))?))
        })
        .collect();
    let hidden = created
        .iter()
        .copied()
        .filter(|id| {
            world
                .get_resource::<ObjectVisibility>()
                .is_some_and(|v| v.hidden.contains(id))
        })
        .collect();
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(CreateLinkedModelInstanceHistoryCommand {
            sidecars,
            hidden,
            snapshots,
            instance_root_id: group_id,
        }));

    Ok(CommandResult {
        created: created.iter().map(|id| id.0).collect(),
        modified: Vec::new(),
        deleted: Vec::new(),
        output: Some(json!({
            "path": key,
            "root_group_id": group_id.0,
            "source_root_id": root.element_id.0,
            "content_hash": hash,
            "relationship": relationship,
        })),
    })
}

pub fn execute_refresh_linked_models(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(true);
    let group_ids = params
        .get("group_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_u64)
                .map(ElementId)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| linked_group_ids(world));

    let mut modified = Vec::new();
    let mut results = Vec::new();
    for group_id in group_ids {
        let refreshed = refresh_linked_group(world, group_id, force)?.is_some();
        if refreshed {
            modified.push(group_id.0);
        }
        results.push(json!({
            "instance_root_id": group_id.0,
            "status": if refreshed { "refreshed" } else { "unchanged" },
            "relationship": linked_model_relationship(world, group_id)?,
        }));
    }
    Ok(CommandResult {
        modified,
        output: Some(json!({
            "relationship_kind": "linked_model_instance",
            "results": results,
        })),
        ..CommandResult::empty()
    })
}

pub fn reload_linked_models_system(world: &mut World) {
    let delta = world.resource::<Time>().delta_secs();
    {
        let mut state = world.resource_mut::<LinkedModelReloadState>();
        state.elapsed += delta;
        if state.elapsed < RELOAD_INTERVAL_SECONDS {
            return;
        }
        state.elapsed = 0.0;
    }

    for group_id in linked_group_ids(world) {
        if let Err(error) = refresh_linked_group(world, group_id, false) {
            warn!(
                "Failed to refresh linked model group {}: {}",
                group_id.0, error
            );
        }
    }
}

fn inspect_linked_group_ids(world: &mut World, params: &Value) -> Result<Vec<ElementId>, String> {
    let singular = params
        .get("group_id")
        .and_then(Value::as_u64)
        .map(ElementId);
    let plural = params
        .get("group_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_u64)
                .map(ElementId)
                .collect::<Vec<_>>()
        });
    if singular.is_some() && plural.is_some() {
        return Err("Pass group_id or group_ids, not both".to_string());
    }

    let mut group_ids = if let Some(group_id) = singular {
        vec![group_id]
    } else if let Some(group_ids) = plural {
        group_ids
    } else {
        let mut selected = world
            .query_filtered::<(&ElementId, &GroupMembers), With<Selected>>()
            .iter(world)
            .filter_map(|(id, members)| members.linked_model.is_some().then_some(*id))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            selected = linked_group_ids(world);
        }
        selected
    };
    group_ids.sort_by_key(|id| id.0);
    group_ids.dedup();
    for group_id in &group_ids {
        linked_model_relationship(world, *group_id)?;
    }
    Ok(group_ids)
}

fn linked_model_source_state(world: &World, relationship: &LinkedModelRelationship) -> Value {
    let Some(storage) = world.get_resource::<Storage>() else {
        return json!({
            "status": "unavailable",
            "loaded_content_hash": relationship.loaded_content_hash,
            "error": "storage backend is not available",
        });
    };
    match storage.0.load(&relationship.source_path) {
        Ok(bytes) => {
            let observed_content_hash = content_hash(&bytes);
            json!({
                "status": if observed_content_hash == relationship.loaded_content_hash {
                    "current"
                } else {
                    "changed"
                },
                "loaded_content_hash": relationship.loaded_content_hash,
                "observed_content_hash": observed_content_hash,
            })
        }
        Err(error) => json!({
            "status": "unavailable",
            "loaded_content_hash": relationship.loaded_content_hash,
            "error": error,
        }),
    }
}

fn selected_or_new_group(
    world: &mut World,
    params: &Value,
) -> Result<(ElementId, Option<ElementId>), String> {
    if let Some(group_id) = params
        .get("group_id")
        .and_then(Value::as_u64)
        .map(ElementId)
    {
        ensure_group(world, group_id)?;
        return Ok((group_id, None));
    }

    let selected: Vec<ElementId> = world
        .query_filtered::<&ElementId, With<Selected>>()
        .iter(world)
        .copied()
        .collect();
    if selected.is_empty() {
        return Err("Select the house group or its members before creating a linked model".into());
    }
    if selected.len() == 1 && ensure_group(world, selected[0]).is_ok() {
        return Ok((selected[0], None));
    }

    let group_id = world.resource::<ElementIdAllocator>().next_id();
    let cached_bounds = compute_group_bounds_from_world(world, &selected);
    let snapshot = GroupSnapshot {
        element_id: group_id,
        name: params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Linked Model")
            .to_string(),
        member_ids: selected,
        frame: GroupFrame::identity(),
        composite: None,
        linked_model: None,
        cached_bounds,
    };
    snapshot.apply_to(world);
    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });
    select_only(world, group_id);
    Ok((group_id, Some(group_id)))
}

fn selected_linked_group(world: &mut World, params: &Value) -> Result<ElementId, String> {
    if let Some(group_id) = params
        .get("group_id")
        .and_then(Value::as_u64)
        .map(ElementId)
    {
        ensure_group(world, group_id)?;
        if group_members(world, group_id)
            .and_then(|members| members.linked_model.as_ref())
            .is_some()
        {
            return Ok(group_id);
        }
        return Err(format!(
            "Group {} is not a linked model. Use Create Linked Model first.",
            group_id.0
        ));
    }

    let selected: Vec<ElementId> = world
        .query_filtered::<(&ElementId, &GroupMembers), With<Selected>>()
        .iter(world)
        .filter_map(|(element_id, members)| members.linked_model.is_some().then_some(*element_id))
        .collect();
    match selected.as_slice() {
        [group_id] => Ok(*group_id),
        [] => {
            Err("Select one linked model group before opening its external model file".to_string())
        }
        _ => Err("Select only one linked model group to open".to_string()),
    }
}

fn linked_model_path(
    world: &World,
    params: &Value,
    group_id: ElementId,
) -> Result<PathBuf, String> {
    if let Some(path) = params.get("path").and_then(Value::as_str) {
        if path.trim().is_empty() {
            return Err("path must not be empty".into());
        }
        return Ok(PathBuf::from(path));
    }
    let group_name = group_members(world, group_id)
        .map(|members| members.name.as_str())
        .unwrap_or("linked-model");
    let file_name = format!("{}.talos3d", safe_file_stem(group_name));
    let base = world
        .get_resource::<DocumentState>()
        .and_then(|state| state.current_path.as_ref())
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(base.join("linked-models").join(file_name))
}

fn place_linked_model_path(_world: &World, params: &Value) -> Result<Option<PathBuf>, String> {
    if let Some(path) = params.get("path").and_then(Value::as_str) {
        if path.trim().is_empty() {
            return Err("path must not be empty".into());
        }
        return Ok(Some(PathBuf::from(path)));
    }
    Err("path must be provided".into())
}

#[cfg(target_arch = "wasm32")]
fn request_linked_model_file(_: &mut World, _: &Value) -> Result<CommandResult, String> {
    Err("Native open dialogs are not available in the browser shell".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn request_linked_model_file(world: &mut World, params: &Value) -> Result<CommandResult, String> {
    let mut dialog = rfd::AsyncFileDialog::new().add_filter("Talos3D Project", &["talos3d"]);
    if let Some(parent) = world
        .get_resource::<DocumentState>()
        .and_then(|state| state.current_path.as_ref())
        .and_then(|path| path.parent())
    {
        dialog = dialog.set_directory(parent);
    }
    let mut params = params.clone();
    crate::plugins::native_dialog::request(
        world,
        async move {
            dialog
                .pick_file()
                .await
                .map(|file| file.path().to_path_buf())
        },
        move |world, path| {
            if let Some(path) = path {
                params["path"] = Value::String(path.to_string_lossy().into_owned());
                let creation_revision = world.resource::<History>().model_revision() + 1;
                if let Err(error) = execute_place_linked_model(world, &params).map(|result| {
                    if let Some(root_id) = result
                        .output
                        .as_ref()
                        .and_then(|value| value["root_group_id"].as_u64())
                    {
                        world.insert_resource(LinkedPlacementGesture { creation_revision });
                        let anchor = current_group_frame(world, ElementId(root_id)).translation;
                        let mut state = world.resource_mut::<TransformState>();
                        state.pending_mode = Some(TransformMode::Moving);
                        state.pending_options = Some(TransformStartOptions {
                            initial_cursor: Some(anchor),
                            ..Default::default()
                        });
                    }
                }) {
                    if let Some(mut status) =
                        world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
                    {
                        status.set_feedback(format!("Linked model failed: {error}"), 5.0);
                    }
                }
            }
        },
    )?;
    Ok(CommandResult::empty())
}

fn build_linked_model_file(
    world: &World,
    group_id: ElementId,
) -> Result<
    (
        ElementId,
        Vec<u8>,
        u64,
        GroupFrame,
        Vec<ElementId>,
        Vec<ElementId>,
    ),
    String,
> {
    let root_members = group_members(world, group_id)
        .ok_or_else(|| format!("Group {} not found", group_id.0))?
        .member_ids
        .clone();
    let frame = extraction_frame_for_linked_model(world, group_id)?;

    let mut ids = vec![group_id];
    ids.extend(collect_group_members_recursive(world, group_id));
    ids.sort_by_key(|id| id.0);
    ids.dedup();

    let excluded: HashSet<_> = ids
        .iter()
        .copied()
        .filter(|id| {
            find_entity(world, *id).is_some_and(|entity| {
                world
                    .get::<crate::plugins::lighting::SceneLightNode>(entity)
                    .is_some()
            })
        })
        .collect();
    ids.retain(|id| !excluded.contains(id));
    let root_members = root_members
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .collect();
    let source_ids = ids.clone();
    let mut snapshots = ids
        .into_iter()
        .map(|id| capture_snapshot(world, id))
        .collect::<Result<Vec<_>, _>>()?;
    snapshots.sort_by_key(snapshot_dependency_order);

    let mut records = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        let mut normalized = normalize_snapshot_for_linked_file(snapshot, group_id, &frame)?;
        if let Some(group) = normalized.0.as_any().downcast_ref::<GroupSnapshot>() {
            let mut group = group.clone();
            group.member_ids.retain(|id| !excluded.contains(id));
            normalized = group.into();
        }
        records.push(PersistedEntityRecord {
            type_name: normalized.type_name().to_string(),
            data: normalized.to_persisted_json(),
            semantic: find_entity(world, normalized.element_id())
                .and_then(|entity| capture_semantic_sidecars(&world.entity(entity))),
        });
    }
    records.sort_by_key(|record| {
        (
            crate::plugins::commands::snapshot_dependency_order_by_name(&record.type_name),
            record
                .data
                .get("element_id")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
        )
    });
    let max_id = records
        .iter()
        .filter_map(|record| record.data.get("element_id").and_then(Value::as_u64))
        .max()
        .unwrap_or(group_id.0);
    let bytes = serialize_entity_records_as_project(world, max_id + 1, records)?;
    let hash = content_hash(&bytes);
    Ok((group_id, bytes, hash, frame, root_members, source_ids))
}

fn extraction_frame_for_linked_model(
    world: &World,
    group_id: ElementId,
) -> Result<GroupFrame, String> {
    let members =
        group_members(world, group_id).ok_or_else(|| format!("Group {} not found", group_id.0))?;
    let basis = if members.frame.is_identity() {
        GroupFrame {
            translation: Vec3::ZERO,
            rotation: infer_group_yaw_from_world(world, &members.member_ids)
                .unwrap_or(Quat::IDENTITY),
        }
    } else {
        members.frame
    };
    let local_bounds = compute_group_bounds_in_frame_from_world(world, &[group_id], &basis)
        .or_else(|| linked_model_extraction_bounds_in_frame(world, &members.member_ids, &basis))
        .or_else(|| compute_group_bounds_from_world(world, &[group_id]))
        .ok_or_else(|| "Selected group has no authored bounds".to_string())?;
    let local_origin = local_bounds.min;
    Ok(GroupFrame {
        translation: basis.point_to_world(local_origin),
        rotation: basis.rotation,
    })
}

fn infer_group_yaw_from_world(world: &World, member_ids: &[ElementId]) -> Option<Quat> {
    let registry = world.resource::<CapabilityRegistry>();
    let mut stack = member_ids.to_vec();
    let mut best: Option<(f32, Vec3)> = None;

    while let Some(id) = stack.pop() {
        let mut q = world.try_query::<EntityRef>()?;
        let Some(entity_ref) = q
            .iter(world)
            .find(|entity| entity.get::<ElementId>().copied() == Some(id))
        else {
            continue;
        };
        if let Some(members) = entity_ref.get::<GroupMembers>() {
            stack.extend_from_slice(&members.member_ids);
            continue;
        }
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        for (start, end) in snapshot.0.snap_segments() {
            let delta = end - start;
            if delta.y.abs() > 1e-3 {
                continue;
            }
            let horizontal = Vec3::new(delta.x, 0.0, delta.z);
            let length = horizontal.length();
            if length <= 1e-4 {
                continue;
            }
            if best
                .map(|(best_length, _)| length > best_length)
                .unwrap_or(true)
            {
                best = Some((length, horizontal / length));
            }
        }
    }

    let (_, mut direction) = best?;
    if direction.z < -1e-4 || (direction.z.abs() <= 1e-4 && direction.x < 0.0) {
        direction = -direction;
    }
    let yaw = direction.x.atan2(direction.z);
    Some(Quat::from_rotation_y(yaw))
}

fn linked_model_extraction_bounds_in_frame(
    world: &World,
    member_ids: &[ElementId],
    frame: &GroupFrame,
) -> Option<EntityBounds> {
    let registry = world.resource::<CapabilityRegistry>();
    let inverse_rotation = frame.rotation.inverse();
    let to_local = |point: Vec3| inverse_rotation * (point - frame.translation);
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;
    let mut stack = member_ids.to_vec();

    while let Some(id) = stack.pop() {
        let mut q = world.try_query::<EntityRef>()?;
        let Some(entity_ref) = q
            .iter(world)
            .find(|entity| entity.get::<ElementId>().copied() == Some(id))
        else {
            continue;
        };
        if let Some(members) = entity_ref.get::<GroupMembers>() {
            stack.extend_from_slice(&members.member_ids);
            continue;
        }

        let mut include_point = |point: Vec3| {
            let local = to_local(point);
            min = min.min(local);
            max = max.max(local);
            any = true;
        };

        if let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) {
            let segments = snapshot.0.snap_segments();
            if !segments.is_empty() {
                for (start, end) in segments {
                    include_point(start);
                    include_point(end);
                }
                continue;
            }
            if let Some(bounds) = snapshot.bounds() {
                for corner in bounds.corners() {
                    include_point(corner);
                }
                continue;
            }
        }

        if let Some(mesh) = entity_ref.get::<TriangleMesh>() {
            for vertex in &mesh.vertices {
                include_point(*vertex);
            }
        } else if let Some(aabb) = entity_ref.get::<bevy::camera::primitives::Aabb>() {
            let center = Vec3::from(aabb.center);
            let half = Vec3::from(aabb.half_extents);
            let bounds = EntityBounds {
                min: center - half,
                max: center + half,
            };
            for corner in bounds.corners() {
                include_point(corner);
            }
        }
    }

    any.then_some(EntityBounds { min, max })
}

fn normalize_snapshot_for_linked_file(
    snapshot: BoxedEntity,
    root_group_id: ElementId,
    frame: &GroupFrame,
) -> Result<BoxedEntity, String> {
    let inverse_rotation = frame.rotation.inverse();
    if let Some(group) = snapshot.0.as_any().downcast_ref::<GroupSnapshot>() {
        let mut normalized = group.clone();
        normalized.linked_model = None;
        normalized.cached_bounds = None;
        if normalized.element_id == root_group_id {
            normalized.frame = GroupFrame::identity();
        } else {
            normalized.frame = GroupFrame {
                translation: inverse_rotation * (normalized.frame.translation - frame.translation),
                rotation: inverse_rotation * normalized.frame.rotation,
            };
        }
        return Ok(normalized.into());
    }
    if let Some(mesh) = snapshot.0.as_any().downcast_ref::<TriangleMeshSnapshot>() {
        let mut normalized = mesh.clone();
        for vertex in &mut normalized.primitive.vertices {
            *vertex = inverse_rotation * (*vertex - frame.translation);
        }
        if let Some(normals) = &mut normalized.primitive.normals {
            for normal in normals {
                *normal = inverse_rotation * *normal;
            }
        }
        return Ok(normalized.into());
    }
    if let Some(mesh) = snapshot.0.as_any().downcast_ref::<EditableMeshSnapshot>() {
        let mut normalized = mesh.clone();
        for vertex in &mut normalized.mesh.vertices {
            *vertex = inverse_rotation * (*vertex - frame.translation);
        }
        normalized.mesh.recompute_all_normals();
        return Ok(normalized.into());
    }
    if frame.rotation == Quat::IDENTITY {
        return Ok(snapshot.translate_with_parent(-frame.translation));
    }
    Ok(snapshot
        .translate_by(-frame.translation)
        .rotate_by(inverse_rotation))
}

fn refresh_linked_group(
    world: &mut World,
    group_id: ElementId,
    force: bool,
) -> Result<Option<u64>, String> {
    let linked = group_members(world, group_id)
        .and_then(|members| members.linked_model.clone())
        .ok_or_else(|| format!("Group {} is not a linked model", group_id.0))?;
    let bytes = world.resource::<Storage>().0.load(&linked.path)?;
    let hash = content_hash(&bytes);
    if !force && hash == linked.content_hash {
        return Ok(None);
    }
    let mut linked_project = load_linked_project(world, &bytes, &linked.path)?;
    if linked.source_root_is_document {
        append_document_root(&mut linked_project, linked.source_root_id, "Linked Model")?;
    }
    apply_linked_snapshots(world, group_id, linked, hash, linked_project)?;
    Ok(Some(hash))
}

fn load_linked_project(
    world: &World,
    bytes: &[u8],
    source_path: &str,
) -> Result<LinkedProject, String> {
    let ProjectEntityRecords {
        entities: records,
        materials,
        textures,
        definitions,
        layers,
        hidden_objects,
    } = deserialize_project_entity_records_with_assets(bytes)?;
    // Source-scoped identities keep host definitions and separate linked files
    // independent, while remaining stable across refresh and save/reload.
    let namespace = blake3::hash(source_path.as_bytes()).to_hex();
    let definition_ids = definitions
        .as_ref()
        .map(|registry| {
            registry
                .list()
                .into_iter()
                .map(|definition| {
                    (
                        definition.id.0.clone(),
                        format!("linked-{}-{}", &namespace[..16], definition.id.0),
                    )
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut imported_definitions = DefinitionRegistry::default();
    if let Some(definitions) = definitions {
        for definition in definitions.list() {
            let mut value = serde_json::to_value(definition).map_err(|error| error.to_string())?;
            remap_definition_ids(&mut value, &definition_ids);
            imported_definitions
                .insert(serde_json::from_value(value).map_err(|error| error.to_string())?);
        }
    }
    let mut imported_layers = LayerRegistry::default();
    imported_layers.layers.clear();
    let mut layer_ids = HashMap::new();
    if let Some(layers) = layers {
        for (name, mut layer) in layers.layers {
            let mapped = format!("Linked {} / {}", &namespace[..8], name);
            layer_ids.insert(name, mapped.clone());
            layer.name = mapped.clone();
            imported_layers.layers.insert(mapped, layer);
        }
    }
    let registry = world.resource::<CapabilityRegistry>();
    let mut snapshots = Vec::with_capacity(records.len());
    let mut sidecars = HashMap::new();
    let mut excluded = HashSet::new();
    for mut record in records {
        // A linked model contributes model content, not the source document's
        // sun/sky/scene-light rig. The host document owns its environment.
        if record.type_name == "scene_light" {
            if let Some(id) = record.data.get("element_id").and_then(Value::as_u64) {
                excluded.insert(ElementId(id));
            }
            continue;
        }
        remap_layer_ids(&mut record.data, &layer_ids);
        remap_definition_ids(&mut record.data, &definition_ids);
        let factory = registry
            .factory_for(&record.type_name)
            .ok_or_else(|| format!("No factory registered for '{}'", record.type_name))?;
        let snapshot = factory.from_persisted_json(&record.data)?;
        if let Some(semantic) = record.semantic {
            let mut value = serde_json::to_value(semantic).map_err(|e| e.to_string())?;
            remap_definition_ids(&mut value, &definition_ids);
            remap_layer_ids(&mut value, &layer_ids);
            sidecars.insert(snapshot.element_id(), value);
        }
        snapshots.push(snapshot);
    }
    for snapshot in &mut snapshots {
        if let Some(group) = snapshot.0.as_any().downcast_ref::<GroupSnapshot>() {
            let mut group = group.clone();
            group.member_ids.retain(|id| !excluded.contains(id));
            *snapshot = group.into();
        }
    }
    snapshots.sort_by_key(snapshot_dependency_order);
    Ok(LinkedProject {
        layers: imported_layers,
        hidden_objects,
        sidecars,
        snapshots,
        materials,
        textures,
        definitions: imported_definitions,
    })
}

fn remap_layer_ids(value: &mut Value, ids: &HashMap<String, String>) {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if key == "layer" {
                    if let Some(mapped) = value.as_str().and_then(|name| ids.get(name)) {
                        *value = Value::String(mapped.clone());
                        continue;
                    }
                }
                remap_layer_ids(value, ids);
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| remap_layer_ids(value, ids)),
        _ => {}
    }
}

fn remap_definition_ids(value: &mut Value, ids: &HashMap<String, String>) {
    match value {
        Value::String(id) => {
            if let Some(mapped) = ids.get(id) {
                *id = mapped.clone();
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| remap_definition_ids(value, ids)),
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, mut value) in original {
                remap_definition_ids(&mut value, ids);
                values.insert(ids.get(&key).cloned().unwrap_or(key), value);
            }
        }
        _ => {}
    }
}

struct LinkedProject {
    layers: LayerRegistry,
    hidden_objects: std::collections::BTreeSet<ElementId>,
    sidecars: HashMap<ElementId, Value>,
    definitions: DefinitionRegistry,
    snapshots: Vec<BoxedEntity>,
    materials: Option<MaterialRegistry>,
    textures: Option<TextureRegistry>,
}

/// Preserve the extracted-group format when one group contains all source
/// roots. Ordinary documents may have loose objects or several independent
/// groups; give those one virtual root, without modifying the source file.
fn linked_project_root_group(
    project: &mut LinkedProject,
    name: &str,
) -> Result<(GroupSnapshot, bool), String> {
    if project.snapshots.is_empty() {
        return Err("Linked model file is empty".to_string());
    }
    let child_ids = project
        .snapshots
        .iter()
        .filter_map(|snapshot| snapshot.0.as_any().downcast_ref::<GroupSnapshot>())
        .flat_map(|group| group.member_ids.iter().copied())
        .collect::<HashSet<_>>();
    let roots = project
        .snapshots
        .iter()
        .filter(|snapshot| !child_ids.contains(&snapshot.element_id()))
        .collect::<Vec<_>>();
    if let [snapshot] = roots.as_slice() {
        if let Some(group) = snapshot.0.as_any().downcast_ref::<GroupSnapshot>() {
            return Ok((group.clone(), false));
        }
    }
    let source_ids = project
        .snapshots
        .iter()
        .map(|snapshot| snapshot.element_id())
        .collect::<HashSet<_>>();
    // Reserve from the top of JSON's exact-integer range, away from normal
    // sequential element allocation. This id is stable in the relationship,
    // including across source edits, and never written into the source file.
    let root_id = (0..=((1_u64 << 53) - 1))
        .rev()
        .map(ElementId)
        .find(|id| !source_ids.contains(id))
        .ok_or_else(|| "Linked model has no available document-root identity".to_string())?;
    Ok((append_document_root(project, root_id, name)?, true))
}

fn append_document_root(
    project: &mut LinkedProject,
    root_id: ElementId,
    name: &str,
) -> Result<GroupSnapshot, String> {
    if project
        .snapshots
        .iter()
        .any(|snapshot| snapshot.element_id() == root_id)
    {
        return Err(
            "Linked source now uses its reserved document-root identity; place the file again"
                .to_string(),
        );
    }
    if project.snapshots.is_empty() {
        return Err("Linked model file is empty".to_string());
    }
    let child_ids = project
        .snapshots
        .iter()
        .filter_map(|snapshot| snapshot.0.as_any().downcast_ref::<GroupSnapshot>())
        .flat_map(|group| group.member_ids.iter().copied())
        .collect::<HashSet<_>>();
    let mut member_ids = project
        .snapshots
        .iter()
        .map(|snapshot| snapshot.element_id())
        .filter(|id| !child_ids.contains(id))
        .collect::<Vec<_>>();
    member_ids.sort_unstable();
    member_ids.dedup();
    if member_ids.is_empty() {
        return Err(
            "Linked model file has no top-level objects (cyclic group membership)".to_string(),
        );
    }
    let root = GroupSnapshot {
        element_id: root_id,
        name: name.to_string(),
        member_ids,
        frame: GroupFrame::identity(),
        composite: None,
        linked_model: None,
        cached_bounds: None,
    };
    project.snapshots.push(root.clone().into());
    Ok(root)
}

fn apply_linked_snapshots(
    world: &mut World,
    group_id: ElementId,
    mut linked: LinkedModelRef,
    hash: u64,
    linked_project: LinkedProject,
) -> Result<(), String> {
    if world.get_resource::<DefinitionRegistry>().is_none() {
        world.insert_resource(DefinitionRegistry::default());
    }
    for definition in linked_project.definitions.list() {
        world
            .resource_mut::<DefinitionRegistry>()
            .insert(definition.clone());
    }
    merge_linked_project_assets(world, linked_project.materials, linked_project.textures);
    if world.get_resource::<LayerRegistry>().is_none() {
        world.insert_resource(LayerRegistry::default());
    }
    for (name, source_layer) in linked_project.layers.layers {
        let mut registry = world.resource_mut::<LayerRegistry>();
        registry.ensure_layer(&name);
        let layer = registry.layers.get_mut(&name).unwrap();
        layer.visible = source_layer.visible;
        layer.locked = source_layer.locked;
        layer.color = source_layer.color;
    }
    let snapshots = linked_project.snapshots;
    let root = snapshots
        .iter()
        .filter_map(|snapshot| snapshot.0.as_any().downcast_ref::<GroupSnapshot>())
        .find(|group| group.element_id == linked.source_root_id)
        .cloned()
        .ok_or_else(|| "Linked model file is missing its root group".to_string())?;
    let frame = current_group_frame(world, group_id);
    // Root members absent from the source mapping are owned by the host
    // document (for example a terrain-conforming foundation). Refresh may
    // replace source-owned members, but must preserve these dependents and must
    // never serialize them back into the linked source.
    let mapped_scene_ids = linked
        .source_to_scene_ids
        .iter()
        .map(|mapping| mapping.scene_id)
        .collect::<HashSet<_>>();
    let host_dependent_root_members = group_members(world, group_id)
        .map(|members| {
            members
                .member_ids
                .iter()
                .copied()
                .filter(|member_id| !mapped_scene_ids.contains(member_id))
                .filter(|member_id| capture_snapshot(world, *member_id).is_ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let id_map = build_linked_instance_id_map(world, group_id, &linked, &snapshots);
    let new_scene_ids: HashSet<ElementId> = snapshots
        .iter()
        .map(|snapshot| snapshot.element_id())
        .filter(|id| *id != linked.source_root_id)
        .filter_map(|source_id| id_map.get(&source_id).copied())
        .collect();
    let mut old_ids: HashSet<ElementId> = linked
        .source_to_scene_ids
        .iter()
        .map(|mapping| mapping.scene_id)
        .filter(|scene_id| *scene_id != group_id)
        .collect();
    if old_ids.is_empty() {
        old_ids.extend(collect_group_members_recursive(world, group_id));
    }

    for old_id in old_ids {
        if !new_scene_ids.contains(&old_id) {
            if let Ok(snapshot) = capture_snapshot(world, old_id) {
                snapshot.remove_from(world);
            }
        }
    }

    for snapshot in snapshots {
        if snapshot.element_id() == linked.source_root_id {
            continue;
        }
        let remapped = remap_linked_snapshot(world, snapshot, &id_map)?;
        // Source records are in dependency order. Reuse the transformed host
        // for children whose authored controls are expressed in its frame.
        let child = remapped.transform_parent().and_then(|id| {
            let parent = capture_snapshot(world, id).ok()?;
            remapped.0.with_transformed_parent(parent.0.as_ref())
        });
        let composed = child.unwrap_or_else(|| compose_linked_snapshot(remapped, &frame));
        composed.apply_to(world);
        stamp_authored_entity_dependencies(world, &composed);
    }

    for (source_id, mut value) in linked_project.sidecars {
        if let Some(scene_id) = id_map.get(&source_id) {
            remap_element_ids_in_value(&mut value, None, &id_map);
            let sidecars: PersistedSemanticSidecars =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            apply_semantic_sidecars(world, *scene_id, &sidecars);
        }
    }
    if world.get_resource::<ObjectVisibility>().is_none() {
        world.insert_resource(ObjectVisibility::default());
    }
    {
        let mut visibility = world.resource_mut::<ObjectVisibility>();
        for mapping in &linked.source_to_scene_ids {
            visibility.hidden.remove(&mapping.scene_id);
        }
        visibility.hidden.extend(
            linked_project
                .hidden_objects
                .iter()
                .filter_map(|id| id_map.get(id).copied()),
        );
    }
    linked.content_hash = hash;
    linked.source_to_scene_ids = id_map
        .iter()
        .map(|(source_id, scene_id)| LinkedModelIdMapping {
            source_id: *source_id,
            scene_id: *scene_id,
        })
        .collect();
    linked
        .source_to_scene_ids
        .sort_by_key(|mapping| (mapping.source_id.0, mapping.scene_id.0));
    let mut root_member_ids = root
        .member_ids
        .iter()
        .filter_map(|source_id| id_map.get(source_id).copied())
        .collect::<Vec<_>>();
    root_member_ids.extend(host_dependent_root_members);
    root_member_ids.sort_by_key(|id| id.0);
    root_member_ids.dedup();
    let group_after = linked_group_snapshot(world, group_id, root_member_ids, frame, Some(linked))?;
    group_after.apply_to(world);
    bump_allocator_past(world, &new_scene_ids);
    Ok(())
}

fn merge_linked_project_assets(
    world: &mut World,
    materials: Option<MaterialRegistry>,
    textures: Option<TextureRegistry>,
) {
    if let Some(textures) = textures {
        if world.get_resource::<TextureRegistry>().is_none() {
            world.insert_resource(TextureRegistry::default());
        }
        let mut target = world.resource_mut::<TextureRegistry>();
        for texture in textures.all() {
            target.insert(texture.clone());
        }
    }

    if let Some(materials) = materials {
        if world.get_resource::<MaterialRegistry>().is_none() {
            world.insert_resource(MaterialRegistry::default());
        }
        let mut target = world.resource_mut::<MaterialRegistry>();
        for material in materials.all() {
            target.upsert(material.clone());
        }
        ensure_builtin_materials(&mut target);
    }
}

fn build_linked_instance_id_map(
    world: &mut World,
    group_id: ElementId,
    linked: &LinkedModelRef,
    snapshots: &[BoxedEntity],
) -> HashMap<ElementId, ElementId> {
    let mut source_ids = snapshots
        .iter()
        .map(|snapshot| snapshot.element_id())
        .collect::<Vec<_>>();
    source_ids.sort_by_key(|id| id.0);
    source_ids.dedup();

    let existing_map = linked
        .source_to_scene_ids
        .iter()
        .map(|mapping| (mapping.source_id, mapping.scene_id))
        .collect::<HashMap<_, _>>();
    let owned_scene_ids = linked
        .source_to_scene_ids
        .iter()
        .map(|mapping| mapping.scene_id)
        .chain(std::iter::once(group_id))
        .collect::<HashSet<_>>();
    let mut reserved_scene_ids = all_scene_element_ids(world)
        .into_iter()
        .filter(|id| !owned_scene_ids.contains(id))
        .collect::<HashSet<_>>();
    reserved_scene_ids.insert(group_id);

    let mut map = HashMap::new();
    map.insert(linked.source_root_id, group_id);

    for source_id in source_ids {
        if source_id == linked.source_root_id {
            continue;
        }
        if let Some(scene_id) = existing_map.get(&source_id).copied() {
            if scene_id != group_id && !reserved_scene_ids.contains(&scene_id) {
                reserved_scene_ids.insert(scene_id);
                map.insert(source_id, scene_id);
                continue;
            }
        }

        let scene_id = allocate_unused_scene_id(world, &mut reserved_scene_ids);
        map.insert(source_id, scene_id);
    }

    map
}

fn all_scene_element_ids(world: &mut World) -> HashSet<ElementId> {
    let mut query = world.query::<&ElementId>();
    query.iter(world).copied().collect()
}

fn allocate_unused_scene_id(
    world: &mut World,
    reserved_scene_ids: &mut HashSet<ElementId>,
) -> ElementId {
    loop {
        let candidate = world.resource::<ElementIdAllocator>().next_id();
        if reserved_scene_ids.insert(candidate) {
            return candidate;
        }
    }
}

fn remap_linked_snapshot(
    world: &World,
    snapshot: BoxedEntity,
    id_map: &HashMap<ElementId, ElementId>,
) -> Result<BoxedEntity, String> {
    let type_name = snapshot.type_name();
    let mut data = snapshot.to_persisted_json();
    remap_element_ids_in_value(&mut data, None, id_map);
    let factory = world
        .resource::<CapabilityRegistry>()
        .factory_for(type_name)
        .ok_or_else(|| format!("No factory registered for '{type_name}'"))?;
    factory.from_persisted_json(&data)
}

fn remap_element_ids_in_value(
    value: &mut Value,
    key: Option<&str>,
    id_map: &HashMap<ElementId, ElementId>,
) {
    match value {
        Value::Number(number) if key.is_some_and(is_element_id_key) => {
            if let Some(source_id) = number.as_u64().map(ElementId) {
                if let Some(scene_id) = id_map.get(&source_id) {
                    *value = json!(scene_id.0);
                }
            }
        }
        Value::Array(values) if key.is_some_and(is_element_id_array_key) => {
            for item in values {
                if let Some(source_id) = item.as_u64().map(ElementId) {
                    if let Some(scene_id) = id_map.get(&source_id) {
                        *item = json!(scene_id.0);
                    }
                } else {
                    remap_element_ids_in_value(item, None, id_map);
                }
            }
        }
        Value::Array(values) => {
            for item in values {
                remap_element_ids_in_value(item, None, id_map);
            }
        }
        Value::Object(object) => {
            for (child_key, child_value) in object {
                remap_element_ids_in_value(child_value, Some(child_key.as_str()), id_map);
            }
        }
        _ => {}
    }
}

fn is_element_id_key(key: &str) -> bool {
    key.ends_with("_element_id")
        || matches!(
            key,
            "element_id"
                | "source"
                | "target"
                | "source_root_id"
                | "source_id"
                | "scene_id"
                | "host_ref"
                | "hosted_fill_ref"
        )
}

fn is_element_id_array_key(key: &str) -> bool {
    matches!(key, "member_ids")
}

fn compose_linked_snapshot(snapshot: BoxedEntity, frame: &GroupFrame) -> BoxedEntity {
    if let Some(group) = snapshot.0.as_any().downcast_ref::<GroupSnapshot>() {
        let mut composed = group.clone();
        composed.frame = frame.then(&group.frame);
        composed.linked_model = None;
        composed.cached_bounds = None;
        return composed.into();
    }
    compose_snapshot_into_frame(snapshot, frame)
}

fn linked_group_snapshot(
    world: &World,
    group_id: ElementId,
    member_ids: Vec<ElementId>,
    frame: GroupFrame,
    linked_model: Option<LinkedModelRef>,
) -> Result<BoxedEntity, String> {
    let members =
        group_members(world, group_id).ok_or_else(|| format!("Group {} not found", group_id.0))?;
    Ok(GroupSnapshot {
        element_id: group_id,
        name: members.name.clone(),
        member_ids,
        frame,
        composite: None,
        linked_model,
        cached_bounds: compute_group_bounds_from_world(world, &[group_id]),
    }
    .into())
}

fn capture_snapshot(world: &World, element_id: ElementId) -> Result<BoxedEntity, String> {
    let entity = find_entity(world, element_id)
        .ok_or_else(|| format!("Entity not found: {}", element_id.0))?;
    let entity_ref = world
        .get_entity(entity)
        .map_err(|error| error.to_string())?;
    world
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&entity_ref, world)
        .ok_or_else(|| format!("Entity {} has no authored snapshot", element_id.0))
}

fn ensure_group(world: &World, group_id: ElementId) -> Result<(), String> {
    group_members(world, group_id)
        .map(|_| ())
        .ok_or_else(|| format!("Entity {} is not a group", group_id.0))
}

fn group_members(world: &World, group_id: ElementId) -> Option<&GroupMembers> {
    let entity = find_entity(world, group_id)?;
    world.get::<GroupMembers>(entity)
}

fn current_group_frame(world: &World, group_id: ElementId) -> GroupFrame {
    group_members(world, group_id)
        .map(|members| members.frame)
        .unwrap_or_default()
}

fn linked_group_ids(world: &mut World) -> Vec<ElementId> {
    world
        .query::<EntityRef>()
        .iter(world)
        .filter_map(|entity_ref| {
            let element_id = *entity_ref.get::<ElementId>()?;
            entity_ref
                .get::<GroupMembers>()?
                .linked_model
                .is_some()
                .then_some(element_id)
        })
        .collect()
}

fn find_entity(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut query = world.try_query::<EntityRef>()?;
    query
        .iter(world)
        .find(|entity| entity.get::<ElementId>().copied() == Some(element_id))
        .map(|entity| entity.id())
}

fn select_only(world: &mut World, element_id: ElementId) {
    let selected = world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .collect::<Vec<_>>();
    for entity in selected {
        world.entity_mut(entity).remove::<Selected>();
    }
    if let Some(entity) = find_entity(world, element_id) {
        world.entity_mut(entity).insert(Selected);
    }
}

fn bump_allocator_past(world: &mut World, ids: &HashSet<ElementId>) {
    let Some(max_id) = ids.iter().map(|id| id.0).max() else {
        return;
    };
    let next = world.resource::<ElementIdAllocator>().next_value();
    if next <= max_id {
        world
            .resource_mut::<ElementIdAllocator>()
            .set_next(max_id + 1);
    }
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Import policy is part of the cache identity. Older links refresh once even
    // when their source bytes have not changed (both extracted and placed links).
    "linked-model-content-v2".hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn safe_file_stem(name: &str) -> String {
    let stem = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if stem.is_empty() {
        "linked-model".to_string()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authored_entity::EntityBounds,
        capability_registry::CapabilityRegistry,
        plugins::{
            commands::ApplyEntityChangesCommand,
            history::{apply_pending_history_commands_for_test, History},
            materials::{MaterialAssignment, MaterialDef, MaterialRegistry},
            modeling::{
                assembly::{
                    AssemblyFactory, AssemblyMemberRef, AssemblySnapshot, SemanticAssembly,
                },
                foundation::{Foundation, FoundationFactory, FoundationFootprint},
                generic_factory::PrimitiveFactory,
                generic_snapshot::PrimitiveSnapshot,
                group::GroupFactory,
                mesh_generation::DerivedGeometry,
                primitives::{BoxPrimitive, ShapeRotation},
            },
            storage::LocalFileBackend,
        },
    };
    use bevy::math::DVec2;

    fn test_world() -> World {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(GroupFactory);
        registry.register_factory(AssemblyFactory);
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        registry.register_factory(FoundationFactory);
        world.insert_resource(registry);
        let mut allocator = ElementIdAllocator::default();
        allocator.set_next(100);
        world.insert_resource(allocator);
        world.insert_resource(Storage(Box::new(LocalFileBackend)));
        world.insert_resource(Messages::<CreateEntityCommand>::default());
        world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(History::default());
        world
    }

    fn spawn_house_group(world: &mut World) -> ElementId {
        PrimitiveSnapshot {
            element_id: ElementId(1),
            primitive: BoxPrimitive {
                centre: Vec3::new(10.0, 2.25, 20.0),
                half_extents: Vec3::new(3.0, 0.25, 2.5),
            },
            rotation: ShapeRotation::default(),
            layer: None,
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .apply_to(world);
        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(10.0, 3.75, 20.0),
                half_extents: Vec3::new(3.0, 1.25, 2.5),
            },
            rotation: ShapeRotation::default(),
            layer: None,
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .apply_to(world);
        let group = GroupSnapshot {
            element_id: ElementId(3),
            name: "House".to_string(),
            member_ids: vec![ElementId(1), ElementId(2)],
            frame: GroupFrame::identity(),
            composite: None,
            linked_model: None,
            cached_bounds: None,
        };
        group.apply_to(world);
        let entity = find_entity(world, ElementId(3)).unwrap();
        world.entity_mut(entity).insert(Selected);
        ElementId(3)
    }

    fn placed_house(world: &mut World, path: &Path) -> ElementId {
        let result = execute_place_linked_model(
            world,
            &json!({
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be placed");
        ElementId(result.created[0])
    }

    fn save_source(world: &mut World, path: &Path) -> Vec<u8> {
        let mut ids = all_scene_element_ids(world).into_iter().collect::<Vec<_>>();
        ids.sort_unstable();
        let next = ids.iter().map(|id| id.0).max().unwrap_or(0) + 1;
        let records = ids
            .into_iter()
            .map(|id| {
                let snapshot = capture_snapshot(world, id).unwrap();
                PersistedEntityRecord {
                    type_name: snapshot.type_name().to_string(),
                    data: snapshot.to_persisted_json(),
                    semantic: find_entity(world, id)
                        .and_then(|entity| capture_semantic_sidecars(&world.entity(entity))),
                }
            })
            .collect();
        let bytes = serialize_entity_records_as_project(world, next, records).unwrap();
        std::fs::write(path, &bytes).unwrap();
        bytes
    }

    fn mapped_id(world: &World, root: ElementId, source: u64) -> ElementId {
        let relationship = linked_model_relationship(world, root).unwrap();
        ElementId(
            relationship
                .source_to_scene_ids
                .iter()
                .find(|mapping| mapping.source_id == source)
                .unwrap()
                .scene_id,
        )
    }

    #[test]
    fn extracted_group_links_preserve_visibility_and_upgrade_without_source_edits() {
        use crate::plugins::{
            layers::{LayerAssignment, LayerRegistry, ObjectVisibility},
            lighting::{SceneLightFactory, SceneLightNode, SceneLightSnapshot},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extracted.talos3d");
        let mut host = test_world();
        host.resource_mut::<CapabilityRegistry>()
            .register_factory(SceneLightFactory);
        let root = spawn_house_group(&mut host);
        let mut layers = LayerRegistry::default();
        layers.ensure_layer("Alternative");
        layers.layers.get_mut("Alternative").unwrap().visible = false;
        host.insert_resource(layers);
        let child = find_entity(&host, ElementId(2)).unwrap();
        host.entity_mut(child)
            .insert(LayerAssignment::new("Alternative"));
        host.insert_resource(ObjectVisibility {
            hidden: [ElementId(1)].into_iter().collect(),
        });
        SceneLightSnapshot {
            element_id: ElementId(4),
            node: SceneLightNode::default(),
            translation: Vec3::Y * 30.,
            rotation: Quat::IDENTITY,
        }
        .apply_to(&mut host);
        let root_entity = find_entity(&host, root).unwrap();
        host.get_mut::<GroupMembers>(root_entity)
            .unwrap()
            .member_ids
            .push(ElementId(4));
        execute_create_linked_model(&mut host, &json!({"group_id":root.0, "path":path})).unwrap();
        let original_bytes = std::fs::read(&path).unwrap();
        let records = deserialize_project_entity_records_with_assets(&original_bytes).unwrap();
        assert!(records
            .entities
            .iter()
            .all(|r| r.type_name != "scene_light"));
        assert!(!records.layers.unwrap().layers["Alternative"].visible);
        assert!(records.hidden_objects.contains(&ElementId(1)));
        assert!(
            find_entity(&host, ElementId(4)).is_some(),
            "host environment is retained"
        );
        assert!(!group_members(&host, root)
            .unwrap()
            .member_ids
            .contains(&ElementId(4)));

        // Simulate a link cached by the old importer, with unchanged source bytes.
        let mut legacy_hash = DefaultHasher::new();
        original_bytes.hash(&mut legacy_hash);
        host.get_mut::<GroupMembers>(root_entity)
            .unwrap()
            .linked_model
            .as_mut()
            .unwrap()
            .content_hash = legacy_hash.finish();
        assert!(refresh_linked_group(&mut host, root, false)
            .unwrap()
            .is_some());
        assert!(refresh_linked_group(&mut host, root, false)
            .unwrap()
            .is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
        let layer = &host.get::<LayerAssignment>(child).unwrap().layer;
        assert!(layer.starts_with("Linked "));
        assert!(!host.resource::<LayerRegistry>().layers[layer].visible);
        assert!(host
            .resource::<ObjectVisibility>()
            .hidden
            .contains(&ElementId(1)));
        assert_eq!(host.query::<&SceneLightNode>().iter(&host).count(), 1);

        // The exact same exported file also works when placed into another host.
        let mut other = test_world();
        let placed = placed_house(&mut other, &path);
        let mapped = mapped_id(&other, placed, 2);
        let layer = &other
            .get::<LayerAssignment>(find_entity(&other, mapped).unwrap())
            .unwrap()
            .layer;
        assert!(!other.resource::<LayerRegistry>().layers[layer].visible);
        assert!(other
            .resource::<ObjectVisibility>()
            .hidden
            .contains(&mapped_id(&other, placed, 1)));
    }

    #[test]
    fn linked_import_preserves_source_visibility_without_environment_lights() {
        use crate::plugins::{
            layers::{LayerAssignment, LayerRegistry, ObjectVisibility},
            lighting::{SceneLightFactory, SceneLightNode, SceneLightSnapshot},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.talos3d");
        let mut source = test_world();
        source
            .resource_mut::<CapabilityRegistry>()
            .register_factory(SceneLightFactory);
        let source_root = spawn_house_group(&mut source);
        let mut layers = LayerRegistry::default();
        layers.ensure_layer("Alternative");
        layers.layers.get_mut("Alternative").unwrap().visible = false;
        source.insert_resource(layers);
        let group = find_entity(&source, source_root).unwrap();
        source.entity_mut(group).insert(LayerAssignment {
            layer: "Alternative".into(),
        });
        source
            .get_mut::<GroupMembers>(group)
            .unwrap()
            .member_ids
            .push(ElementId(4));
        let light = SceneLightSnapshot {
            element_id: ElementId(4),
            node: SceneLightNode::default(),
            translation: Vec3::new(30., 40., 50.),
            rotation: Quat::IDENTITY,
        };
        light.apply_to(&mut source);
        source.insert_resource(ObjectVisibility {
            hidden: [ElementId(2)].into_iter().collect(),
        });
        save_source(&mut source, &path);

        let mut host = test_world();
        host.resource_mut::<CapabilityRegistry>()
            .register_factory(SceneLightFactory);
        let host_light = SceneLightSnapshot {
            element_id: ElementId(80),
            ..light.clone()
        };
        host_light.apply_to(&mut host);
        let mut host_layers = LayerRegistry::default();
        host_layers.ensure_layer("Alternative");
        host.insert_resource(host_layers);
        let root = placed_house(&mut host, &path);
        apply_pending_history_commands_for_test(&mut host);
        let hidden_id = mapped_id(&host, root, 2);
        let root_entity = find_entity(&host, root).unwrap();
        let layer_name = host
            .get::<LayerAssignment>(root_entity)
            .unwrap()
            .layer
            .clone();
        assert_ne!(layer_name, "Alternative");
        assert!(!host.resource::<LayerRegistry>().layers[&layer_name].visible);
        assert!(host.resource::<LayerRegistry>().layers["Alternative"].visible);
        assert!(host
            .resource::<ObjectVisibility>()
            .hidden
            .contains(&hidden_id));
        assert!(!linked_model_relationship(&host, root)
            .unwrap()
            .source_to_scene_ids
            .iter()
            .any(|mapping| mapping.source_id == 4));
        assert_eq!(host.query::<&SceneLightNode>().iter(&host).count(), 1);

        host.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut host);
        assert!(!host
            .resource::<ObjectVisibility>()
            .hidden
            .contains(&hidden_id));
        host.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands_for_test(&mut host);
        assert!(host
            .resource::<ObjectVisibility>()
            .hidden
            .contains(&hidden_id));
        assert_eq!(
            host.get::<LayerAssignment>(find_entity(&host, root).unwrap())
                .unwrap()
                .layer,
            layer_name
        );

        // A refresh also removes environment content brought in by an older app.
        let legacy = SceneLightSnapshot {
            element_id: ElementId(500),
            ..light
        };
        legacy.apply_to(&mut host);
        let entity = find_entity(&host, root).unwrap();
        let mut members = host.get_mut::<GroupMembers>(entity).unwrap();
        members.member_ids.push(ElementId(500));
        members
            .linked_model
            .as_mut()
            .unwrap()
            .source_to_scene_ids
            .push(LinkedModelIdMapping {
                source_id: ElementId(4),
                scene_id: ElementId(500),
            });
        execute_refresh_linked_models(&mut host, &json!({"group_ids": [root.0], "force": true}))
            .unwrap();
        assert!(find_entity(&host, ElementId(500)).is_none());
        assert!(find_entity(&host, ElementId(80)).is_some());
        assert_eq!(host.query::<&SceneLightNode>().iter(&host).count(), 1);
        assert!(!group_members(&host, root)
            .unwrap()
            .member_ids
            .contains(&ElementId(500)));
        let saved = save_source(&mut host, &dir.path().join("placed.talos3d"));
        let records = deserialize_project_entity_records_with_assets(&saved).unwrap();
        assert!(!records.layers.unwrap().layers[&layer_name].visible);
        assert!(records.hidden_objects.contains(&hidden_id));
        assert!(records
            .entities
            .iter()
            .filter_map(|r| r.semantic.as_ref())
            .any(|s| s
                .layer_assignment
                .as_ref()
                .is_some_and(|a| a.layer == layer_name)));
    }

    #[test]
    fn ordinary_ungrouped_file_is_a_complete_undoable_persisted_link() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing-model.talos3d");
        let mut source = test_world();
        let group = spawn_house_group(&mut source);
        capture_snapshot(&source, group)
            .unwrap()
            .remove_from(&mut source);
        let bytes = save_source(&mut source, &path);
        let source_bounds =
            compute_group_bounds_from_world(&source, &[ElementId(1), ElementId(2)]).unwrap();

        let mut host = test_world();
        let result = execute_place_linked_model(&mut host, &json!({"path": path})).unwrap();
        let root = ElementId(result.output.unwrap()["root_group_id"].as_u64().unwrap());
        let relationship = linked_model_relationship(&host, root).unwrap();
        assert_eq!(relationship.source_kind, "document");
        assert_eq!(relationship.instance_name, "existing-model");
        assert_eq!(relationship.source_to_scene_ids.len(), 3);
        assert_eq!(group_members(&host, root).unwrap().member_ids.len(), 2);
        let bounds = compute_group_bounds_from_world(&host, &[root]).unwrap();
        assert_eq!(bounds.min, source_bounds.min);
        assert_eq!(bounds.max, source_bounds.max);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        apply_pending_history_commands_for_test(&mut host);
        assert_eq!(host.resource::<History>().undo_stack_len(), 1);
        host.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut host);
        assert!(all_scene_element_ids(&mut host).is_empty());
        host.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands_for_test(&mut host);
        assert_eq!(
            linked_model_relationship(&host, root).unwrap(),
            relationship
        );

        let host_path = temp.path().join("host.talos3d");
        let host_bytes = save_source(&mut host, &host_path);
        let project = load_linked_project(&host, &host_bytes, "test-house.talos3d").unwrap();
        let mut restored = test_world();
        for snapshot in project.snapshots {
            snapshot.apply_to(&mut restored);
        }
        assert_eq!(
            linked_model_relationship(&restored, root).unwrap(),
            relationship
        );
        execute_refresh_linked_models(&mut restored, &json!({"force": true})).unwrap();
        assert_eq!(
            linked_model_relationship(&restored, root).unwrap(),
            relationship
        );
    }

    #[test]
    fn mixed_file_roots_refresh_as_one_moved_instance_without_losing_hierarchy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mixed.talos3d");
        let mut source = test_world();
        let group = spawn_house_group(&mut source);
        // One group and one loose physical object used to lose the loose object
        // from root membership, even though it was imported into the scene.
        let mut loose = capture_snapshot(&source, ElementId(2))
            .unwrap()
            .to_persisted_json();
        loose["element_id"] = json!(4);
        source
            .resource::<CapabilityRegistry>()
            .factory_for("box")
            .unwrap()
            .from_persisted_json(&loose)
            .unwrap()
            .apply_to(&mut source);
        save_source(&mut source, &path);
        let mut host = test_world();
        let root = placed_house(&mut host, &path);
        let other = placed_house(&mut host, &path);
        let before = linked_model_relationship(&host, root).unwrap();
        assert_eq!(before.source_kind, "document");
        assert_eq!(
            group_members(&host, root).unwrap().member_ids,
            vec![mapped_id(&host, root, group.0), mapped_id(&host, root, 4)]
        );
        assert_eq!(
            group_members(&host, mapped_id(&host, root, group.0))
                .unwrap()
                .member_ids,
            vec![mapped_id(&host, root, 1), mapped_id(&host, root, 2)]
        );
        apply_pending_history_commands_for_test(&mut host);
        let bounds = compute_group_bounds_from_world(&host, &[root]).unwrap();
        let delta = Vec3::new(7.0, 2.0, -4.0);
        let plan = plan_linked_model_placement(
            &host,
            root,
            LinkedModelPlacementOperation::Translate { delta },
        )
        .unwrap();
        crate::plugins::commands::enqueue_apply_entity_changes(
            &mut host,
            plan.into_apply_command("Move linked file"),
        );
        apply_pending_history_commands_for_test(&mut host);
        let moved = compute_group_bounds_from_world(&host, &[root]).unwrap();
        assert!((moved.min - bounds.min - delta).length() < 1e-4);
        assert!(
            (compute_group_bounds_from_world(&host, &[other])
                .unwrap()
                .min
                - bounds.min)
                .length()
                < 1e-4
        );

        // The source now has only one group. Refresh must keep the original
        // document scope and instance frame rather than switching root identity.
        capture_snapshot(&source, ElementId(4))
            .unwrap()
            .remove_from(&mut source);
        save_source(&mut source, &path);
        execute_refresh_linked_models(&mut host, &json!({"group_ids": [root.0]})).unwrap();
        let refreshed = linked_model_relationship(&host, root).unwrap();
        assert_eq!(refreshed.source_root_id, before.source_root_id);
        assert_eq!(refreshed.source_kind, "document");
        assert_eq!(refreshed.frame.translation, delta.to_array());
        assert_eq!(
            mapped_id(&host, root, group.0),
            ElementId(
                before
                    .source_to_scene_ids
                    .iter()
                    .find(|m| m.source_id == group.0)
                    .unwrap()
                    .scene_id
            )
        );
        assert!(!refreshed
            .source_to_scene_ids
            .iter()
            .any(|m| m.source_id == 4));
        assert!(find_entity(&host, mapped_id(&host, other, 4)).is_some());
        assert_eq!(
            group_members(&host, root).unwrap().member_ids,
            vec![mapped_id(&host, root, group.0)]
        );
    }

    #[test]
    fn nested_and_multiple_group_files_resolve_the_actual_root() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested.talos3d");
        let mut source = test_world();
        let group = spawn_house_group(&mut source);
        GroupSnapshot {
            element_id: ElementId(10),
            name: "Outer".into(),
            member_ids: vec![group],
            frame: GroupFrame::identity(),
            composite: None,
            linked_model: None,
            cached_bounds: None,
        }
        .apply_to(&mut source);
        save_source(&mut source, &path);
        let mut host = test_world();
        let root = placed_house(&mut host, &path);
        assert_eq!(
            linked_model_relationship(&host, root)
                .unwrap()
                .source_root_id,
            10
        );
        assert_eq!(
            group_members(&host, root).unwrap().member_ids,
            vec![mapped_id(&host, root, group.0)]
        );
        execute_refresh_linked_models(&mut host, &json!({"force": true})).unwrap();

        // Two independent top-level groups must both belong to the instance.
        GroupSnapshot {
            element_id: ElementId(11),
            name: "Second".into(),
            member_ids: vec![],
            frame: GroupFrame::identity(),
            composite: None,
            linked_model: None,
            cached_bounds: None,
        }
        .apply_to(&mut source);
        save_source(&mut source, &path);
        let multi = placed_house(&mut host, &path);
        assert_eq!(
            linked_model_relationship(&host, multi).unwrap().source_kind,
            "document"
        );
        assert_eq!(group_members(&host, multi).unwrap().member_ids.len(), 2);
        assert!(group_members(&host, multi)
            .unwrap()
            .member_ids
            .contains(&mapped_id(&host, multi, 10)));
        assert!(group_members(&host, multi)
            .unwrap()
            .member_ids
            .contains(&mapped_id(&host, multi, 11)));
    }

    #[test]
    fn empty_file_refuses_before_mutating_host() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("empty.talos3d");
        let mut source = test_world();
        save_source(&mut source, &path);
        let mut host = test_world();
        let error = execute_place_linked_model(&mut host, &json!({"path": path})).unwrap_err();
        assert!(error.contains("empty"));
        assert!(all_scene_element_ids(&mut host).is_empty());
        assert_eq!(host.resource::<History>().undo_stack_len(), 0);
    }

    #[test]
    fn create_linked_model_normalizes_foundation_minimum_to_xz_plane() {
        let mut world = test_world();
        let group_id = spawn_house_group(&mut world);
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        execute_create_linked_model(
            &mut world,
            &json!({
                "group_id": group_id.0,
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be created");

        let bytes = std::fs::read(&path).unwrap();
        let (_next_id, records) = deserialize_project_entity_records(&bytes).unwrap();
        let foundation = records
            .iter()
            .find(|record| record.data.get("element_id") == Some(&json!(1)))
            .expect("foundation record should exist");
        let centre = foundation.data["centre"].as_array().unwrap();
        let half_extents = foundation.data["half_extents"].as_array().unwrap();
        let centre_y = centre[1].as_f64().unwrap() as f32;
        let half_y = half_extents[1].as_f64().unwrap() as f32;
        assert!(
            (centre_y - half_y).abs() < 1e-5,
            "linked file should place the selected foundation minimum on y=0"
        );

        let linked = group_members(&world, group_id)
            .unwrap()
            .linked_model
            .as_ref()
            .expect("scene group should carry a linked model ref");
        assert_eq!(linked.path, path.to_string_lossy());
    }

    #[test]
    fn place_linked_model_imports_file_as_live_group() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({
                "group_id": source_group.0,
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be created");

        let mut target = test_world();
        let result = execute_place_linked_model(
            &mut target,
            &json!({
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be placed");

        assert_eq!(result.created[0], 100);
        let placed = group_members(&target, ElementId(100)).expect("placed group should exist");
        let linked = placed
            .linked_model
            .as_ref()
            .expect("placed group should remain live-linked");
        assert_eq!(linked.path, path.to_string_lossy());
        assert_eq!(linked.source_root_id, ElementId(3));
        assert_eq!(placed.member_ids.len(), 2);
        assert!(find_entity(&target, ElementId(101)).is_some());
        assert!(find_entity(&target, ElementId(102)).is_some());
        assert!(
            target
                .get::<Selected>(find_entity(&target, ElementId(100)).unwrap())
                .is_some(),
            "placed linked model group should be selected"
        );
    }

    #[test]
    fn inspect_linked_model_reports_relationship_and_source_freshness() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({
                "group_id": source_group.0,
                "path": path.to_string_lossy()
            }),
        )
        .unwrap();
        let mut target = test_world();
        let placed =
            execute_place_linked_model(&mut target, &json!({ "path": path.to_string_lossy() }))
                .unwrap();
        let root_id = ElementId(placed.created[0]);
        let inspected =
            execute_inspect_linked_models(&mut target, &json!({ "group_id": root_id.0 }))
                .unwrap()
                .output
                .unwrap();
        assert_eq!(inspected["count"], 1);
        assert_eq!(
            inspected["relationships"][0]["relationship"]["relationship_kind"],
            "linked_model_instance"
        );
        assert_eq!(
            inspected["relationships"][0]["relationship"]["instance_root_id"],
            root_id.0
        );
        assert_eq!(
            inspected["relationships"][0]["source_state"]["status"],
            "current"
        );
        assert_eq!(
            inspected["relationships"][0]["relationship"]["source_to_scene_ids"]
                .as_array()
                .unwrap()
                .len(),
            3
        );

        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        let changed = execute_inspect_linked_models(&mut target, &json!({ "group_id": root_id.0 }))
            .unwrap()
            .output
            .unwrap();
        assert_eq!(
            changed["relationships"][0]["source_state"]["status"],
            "changed"
        );
    }

    #[test]
    fn placement_subject_moves_non_flat_delta_atomically_with_history() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({ "group_id": source_group.0, "path": path.to_string_lossy() }),
        )
        .unwrap();

        let mut target = test_world();
        let root_id = placed_house(&mut target, &path);
        apply_pending_history_commands_for_test(&mut target);
        let subject = linked_model_placement_subject(&target, root_id).unwrap();
        assert_eq!(subject.subject_kind, "linked_model_placement_subject");
        assert_eq!(subject.source_owned_scene_ids.len(), 3);
        assert!(subject.host_dependent_scene_ids.is_empty());
        assert_eq!(subject.dependency_anchor.source_id, 3);

        let initial_bounds = compute_group_bounds_from_world(&target, &[root_id]).unwrap();
        let delta = Vec3::new(4.0, 2.5, -3.0);
        let plan = plan_linked_model_placement(
            &target,
            root_id,
            LinkedModelPlacementOperation::Translate { delta },
        )
        .unwrap();
        assert_eq!(
            plan.subject.mapping_fingerprint,
            subject.mapping_fingerprint
        );
        assert!(plan
            .after()
            .iter()
            .any(|snapshot| snapshot.element_id() == root_id));
        crate::plugins::commands::enqueue_apply_entity_changes(
            &mut target,
            plan.into_apply_command("Place linked instance"),
        );
        apply_pending_history_commands_for_test(&mut target);

        let moved_bounds = compute_group_bounds_from_world(&target, &[root_id]).unwrap();
        assert!((moved_bounds.min - (initial_bounds.min + delta)).length() < 1e-4);
        assert!((moved_bounds.max - (initial_bounds.max + delta)).length() < 1e-4);
        assert_eq!(
            linked_model_placement_subject(&target, root_id)
                .unwrap()
                .relationship
                .frame
                .translation,
            delta.to_array()
        );

        target.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut target);
        let undone = compute_group_bounds_from_world(&target, &[root_id]).unwrap();
        assert!((undone.min - initial_bounds.min).length() < 1e-4);
        target.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands_for_test(&mut target);
        let redone = compute_group_bounds_from_world(&target, &[root_id]).unwrap();
        assert!((redone.min - moved_bounds.min).length() < 1e-4);
        assert!(linked_model_relationship(&target, root_id).is_ok());
    }

    #[test]
    fn placement_subject_rotates_about_explicit_pivot_without_breaking_link() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({ "group_id": source_group.0, "path": path.to_string_lossy() }),
        )
        .unwrap();

        let mut target = test_world();
        let root_id = placed_house(&mut target, &path);
        let rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let plan = plan_linked_model_placement(
            &target,
            root_id,
            LinkedModelPlacementOperation::Rotate {
                rotation,
                pivot: Some(Vec3::ZERO),
            },
        )
        .unwrap();
        for snapshot in plan.after() {
            snapshot.apply_to(&mut target);
        }
        let relationship = linked_model_relationship(&target, root_id).unwrap();
        let actual_rotation = Quat::from_array(relationship.frame.rotation);
        assert!(
            actual_rotation.dot(rotation).abs() > 0.99999,
            "expected {rotation:?}, got {actual_rotation:?}"
        );
        assert_eq!(relationship.source_to_scene_ids.len(), 3);
        assert!(linked_model_placement_subject(&target, root_id).is_ok());
    }

    #[test]
    fn stale_placement_mapping_refuses_without_partial_mutation() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({ "group_id": source_group.0, "path": path.to_string_lossy() }),
        )
        .unwrap();

        let mut target = test_world();
        let root_id = placed_house(&mut target, &path);
        let relationship = linked_model_relationship(&target, root_id).unwrap();
        let missing_id = relationship
            .source_to_scene_ids
            .iter()
            .find(|mapping| mapping.scene_id != root_id.0)
            .map(|mapping| ElementId(mapping.scene_id))
            .unwrap();
        capture_snapshot(&target, missing_id)
            .unwrap()
            .remove_from(&mut target);
        let frame_before = group_frame(&target, root_id).unwrap();

        let error = plan_linked_model_placement(
            &target,
            root_id,
            LinkedModelPlacementOperation::Translate { delta: Vec3::Y },
        )
        .err()
        .expect("stale mapping should refuse");
        assert_eq!(error.code, "stale_mapping");
        assert_eq!(group_frame(&target, root_id).unwrap(), frame_before);

        let inspected = execute_inspect_linked_model_placement_subject(
            &mut target,
            &json!({ "instance_root_id": root_id.0 }),
        )
        .unwrap()
        .output
        .unwrap();
        assert_eq!(inspected["status"], "stale_mapping");
    }

    #[test]
    fn refresh_preserves_host_dependents_and_rebases_source_anchor() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({ "group_id": source_group.0, "path": path.to_string_lossy() }),
        )
        .unwrap();

        let mut target = test_world();
        let root_id = placed_house(&mut target, &path);
        let host_id = ElementId(500);
        PrimitiveSnapshot {
            element_id: host_id,
            primitive: BoxPrimitive {
                centre: Vec3::new(0.0, -0.1, 0.0),
                half_extents: Vec3::new(2.0, 0.1, 2.0),
            },
            rotation: ShapeRotation::default(),
            layer: None,
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .apply_to(&mut target);
        let (_, membership_after) =
            crate::plugins::modeling::group::group_membership_add_snapshots(
                &target, root_id, host_id,
            )
            .unwrap();
        membership_after.apply_to(&mut target);
        let anchor = linked_model_placement_subject(&target, root_id)
            .unwrap()
            .dependency_anchor;

        let mut source_bytes = std::fs::read(&path).unwrap();
        source_bytes.push(b'\n');
        std::fs::write(&path, source_bytes).unwrap();
        execute_refresh_linked_models(
            &mut target,
            &json!({ "group_ids": [root_id.0], "force": true }),
        )
        .unwrap();

        let refreshed = linked_model_placement_subject(&target, root_id).unwrap();
        assert_eq!(refreshed.host_dependent_scene_ids, vec![host_id.0]);
        assert!(group_members(&target, root_id)
            .unwrap()
            .member_ids
            .contains(&host_id));
        assert!(find_entity(&target, host_id).is_some());
        let resolved = resolve_linked_model_dependency_anchor(&target, anchor).unwrap();
        assert_eq!(resolved.scene_id, root_id.0);
        assert_eq!(resolved.status, "rebased");
    }

    #[test]
    fn placing_linked_model_is_one_undoable_history_operation() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({
                "group_id": source_group.0,
                "path": path.to_string_lossy()
            }),
        )
        .unwrap();
        let (next_id, mut records) =
            deserialize_project_entity_records(&std::fs::read(&path).unwrap()).unwrap();
        records.push(PersistedEntityRecord {
            type_name: "semantic_assembly".to_string(),
            data: AssemblySnapshot {
                element_id: ElementId(10),
                assembly: SemanticAssembly {
                    assembly_type: "source_metadata".to_string(),
                    label: "Source-owned relationship metadata".to_string(),
                    members: vec![AssemblyMemberRef {
                        target: source_group,
                        role: "describes_linked_root".to_string(),
                    }],
                    parameters: Value::Null,
                    metadata: Value::Null,
                },
                refinement_state: None,
                obligations: None,
                claim_grounding: None,
                authoring_provenance: None,
            }
            .to_persisted_json(),
            semantic: None,
        });
        std::fs::write(
            &path,
            serialize_entity_records_as_project(&source, next_id.max(11), records).unwrap(),
        )
        .unwrap();

        let mut target = test_world();
        let result =
            execute_place_linked_model(&mut target, &json!({ "path": path.to_string_lossy() }))
                .unwrap();
        let created = result
            .created
            .iter()
            .copied()
            .map(ElementId)
            .collect::<Vec<_>>();
        assert_eq!(
            created.len(),
            5,
            "all source-owned records and the document root enter history"
        );

        apply_pending_history_commands_for_test(&mut target);
        assert_eq!(target.resource::<History>().undo_stack_len(), 1);
        target.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut target);
        assert!(created.iter().all(|id| find_entity(&target, *id).is_none()));

        target.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands_for_test(&mut target);
        assert!(created.iter().all(|id| find_entity(&target, *id).is_some()));
        assert!(linked_model_relationship(&target, created[0]).is_ok());
    }

    #[test]
    fn multiple_instances_have_distinct_scene_identity_maps() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({
                "group_id": source_group.0,
                "path": path.to_string_lossy()
            }),
        )
        .unwrap();

        let mut target = test_world();
        let first = execute_place_linked_model(
            &mut target,
            &json!({ "path": path.to_string_lossy(), "name": "House A" }),
        )
        .unwrap();
        let second = execute_place_linked_model(
            &mut target,
            &json!({ "path": path.to_string_lossy(), "name": "House B" }),
        )
        .unwrap();
        let first_relationship =
            linked_model_relationship(&target, ElementId(first.created[0])).unwrap();
        let second_relationship =
            linked_model_relationship(&target, ElementId(second.created[0])).unwrap();

        assert_eq!(
            first_relationship.source_path,
            second_relationship.source_path
        );
        assert_eq!(
            first_relationship.source_root_id,
            second_relationship.source_root_id
        );
        assert_ne!(
            first_relationship.instance_root_id,
            second_relationship.instance_root_id
        );
        let first_scene_ids = first_relationship
            .source_to_scene_ids
            .iter()
            .map(|mapping| mapping.scene_id)
            .collect::<HashSet<_>>();
        let second_scene_ids = second_relationship
            .source_to_scene_ids
            .iter()
            .map(|mapping| mapping.scene_id)
            .collect::<HashSet<_>>();
        assert!(first_scene_ids.is_disjoint(&second_scene_ids));

        let inspected = execute_inspect_linked_models(
            &mut target,
            &json!({
                "group_ids": [
                    first_relationship.instance_root_id,
                    second_relationship.instance_root_id
                ]
            }),
        )
        .unwrap()
        .output
        .unwrap();
        assert_eq!(inspected["count"], 2);
    }

    #[test]
    fn linked_relationship_round_trips_through_group_persistence() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        let mut source = test_world();
        let source_group = spawn_house_group(&mut source);
        execute_create_linked_model(
            &mut source,
            &json!({
                "group_id": source_group.0,
                "path": path.to_string_lossy()
            }),
        )
        .unwrap();

        let mut target = test_world();
        let placed =
            execute_place_linked_model(&mut target, &json!({ "path": path.to_string_lossy() }))
                .unwrap();
        let root_id = ElementId(placed.created[0]);
        let expected = linked_model_relationship(&target, root_id).unwrap();
        let persisted = capture_snapshot(&target, root_id)
            .unwrap()
            .to_persisted_json();

        let mut restored = test_world();
        restored
            .resource::<CapabilityRegistry>()
            .factory_for("group")
            .unwrap()
            .from_persisted_json(&persisted)
            .unwrap()
            .apply_to(&mut restored);
        assert_eq!(
            linked_model_relationship(&restored, root_id).unwrap(),
            expected
        );
    }

    #[test]
    fn create_linked_model_captures_authored_foundation_not_derived_mesh() {
        let mut world = test_world();
        world.spawn((
            ElementId(10),
            Foundation {
                footprint: FoundationFootprint::Polyline(vec![
                    DVec2::new(-3.0, -2.5),
                    DVec2::new(3.0, -2.5),
                    DVec2::new(3.0, 2.5),
                    DVec2::new(-3.0, 2.5),
                ]),
                floor_datum: 0.0,
                below_grade_margin: 0.3,
                sample_spacing: 1.0,
            },
            crate::plugins::modeling::primitives::TriangleMesh {
                vertices: vec![
                    Vec3::new(-3.0, 0.0, -2.5),
                    Vec3::new(3.0, 0.0, -2.5),
                    Vec3::new(3.0, 0.0, 2.5),
                ],
                faces: vec![[0, 1, 2]],
                normals: None,
                name: Some("generated foundation mesh".to_string()),
            },
            DerivedGeometry,
        ));
        GroupSnapshot {
            element_id: ElementId(11),
            name: "House".to_string(),
            member_ids: vec![ElementId(10)],
            frame: GroupFrame::identity(),
            composite: None,
            linked_model: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        execute_create_linked_model(
            &mut world,
            &json!({
                "group_id": 11,
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be created");

        let (_next_id, records) =
            deserialize_project_entity_records(&std::fs::read(&path).unwrap()).unwrap();
        assert!(records.iter().any(|record| record.type_name == "foundation"
            && record.data.get("element_id") == Some(&json!(10))));
        assert!(!records
            .iter()
            .any(|record| record.type_name == "triangle_mesh"
                && record.data.get("element_id") == Some(&json!(10))));
    }

    #[test]
    fn create_linked_model_extracts_plain_rotated_group_to_link_frame() {
        let mut world = test_world();
        let rotation = Quat::from_rotation_y(0.6375483);
        PrimitiveSnapshot {
            element_id: ElementId(20),
            primitive: BoxPrimitive {
                centre: Vec3::new(40.762966, 4.2286386, -27.114769),
                half_extents: Vec3::new(2.5, 1.35, 3.0),
            },
            rotation: ShapeRotation(rotation),
            layer: None,
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .apply_to(&mut world);
        GroupSnapshot {
            element_id: ElementId(21),
            name: "Snickis".to_string(),
            member_ids: vec![ElementId(20)],
            frame: GroupFrame::identity(),
            composite: None,
            linked_model: None,
            cached_bounds: None,
        }
        .apply_to(&mut world);

        let before = compute_group_bounds_from_world(&world, &[ElementId(21)]).unwrap();
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("snickis.talos3d");
        execute_create_linked_model(
            &mut world,
            &json!({
                "group_id": 21,
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be created");

        let frame = current_group_frame(&world, ElementId(21));
        assert!(
            frame.rotation.dot(rotation).abs() > 1.0 - 1e-5,
            "site link frame should carry the extracted house yaw: {:?}",
            frame.rotation
        );
        let (_next_id, records) =
            deserialize_project_entity_records(&std::fs::read(&path).unwrap()).unwrap();
        let linked_box = records
            .iter()
            .find(|record| record.data.get("element_id") == Some(&json!(20)))
            .expect("linked box should exist");
        let linked_rotation: Quat =
            serde_json::from_value(linked_box.data["rotation"].clone()).unwrap();
        assert!(
            linked_rotation.abs_diff_eq(Quat::IDENTITY, 1e-5),
            "linked file should be axis-aligned; got {linked_rotation:?}"
        );

        execute_refresh_linked_models(&mut world, &json!({ "group_ids": [21], "force": true }))
            .expect("refresh should succeed");
        let after = compute_group_bounds_from_world(&world, &[ElementId(21)]).unwrap();
        assert_bounds_close(before, after);
    }

    #[test]
    fn refresh_linked_model_updates_scene_geometry_from_file() {
        let mut world = test_world();
        let group_id = spawn_house_group(&mut world);
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        execute_create_linked_model(
            &mut world,
            &json!({
                "group_id": group_id.0,
                "path": path.to_string_lossy()
            }),
        )
        .expect("linked model should be created");

        let (next_id, mut records) =
            deserialize_project_entity_records(&std::fs::read(&path).unwrap()).unwrap();
        let body = records
            .iter_mut()
            .find(|record| record.data.get("element_id") == Some(&json!(2)))
            .expect("body record should exist");
        body.data["centre"] = json!([0.0, 3.0, 0.0]);
        std::fs::write(
            &path,
            serialize_entity_records_as_project(&world, next_id, records).unwrap(),
        )
        .unwrap();

        execute_refresh_linked_models(&mut world, &json!({ "group_ids": [group_id.0] }))
            .expect("refresh should succeed");

        let body_entity = find_entity(&world, ElementId(2)).unwrap();
        let body = world.get::<BoxPrimitive>(body_entity).unwrap();
        assert!(
            (body.centre.y - 5.0).abs() < 1e-5,
            "local body y=3 should compose with scene origin y=2"
        );
    }

    #[test]
    fn refresh_linked_model_allocates_scene_ids_for_new_source_ids() {
        let mut world = test_world();
        let group_id = spawn_house_group(&mut world);
        PrimitiveSnapshot {
            element_id: ElementId(10),
            primitive: BoxPrimitive {
                centre: Vec3::new(99.0, 0.5, 99.0),
                half_extents: Vec3::splat(0.5),
            },
            rotation: ShapeRotation::default(),
            layer: None,
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .apply_to(&mut world);

        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        let linked = LinkedModelRef {
            source_root_is_document: false,
            path: path.to_string_lossy().into_owned(),
            source_root_id: group_id,
            source_to_scene_ids: vec![
                LinkedModelIdMapping {
                    source_id: ElementId(1),
                    scene_id: ElementId(1),
                },
                LinkedModelIdMapping {
                    source_id: ElementId(2),
                    scene_id: ElementId(2),
                },
                LinkedModelIdMapping {
                    source_id: group_id,
                    scene_id: group_id,
                },
            ],
            content_hash: 0,
        };
        linked_group_snapshot(
            &world,
            group_id,
            vec![ElementId(1), ElementId(2)],
            GroupFrame::identity(),
            Some(linked),
        )
        .unwrap()
        .apply_to(&mut world);

        let records = vec![
            persisted_box_record(1, Vec3::new(10.0, 2.25, 20.0)),
            persisted_box_record(2, Vec3::new(10.0, 3.75, 20.0)),
            persisted_box_record(10, Vec3::new(0.0, 1.0, 0.0)),
            PersistedEntityRecord {
                type_name: "group".to_string(),
                data: GroupSnapshot {
                    element_id: group_id,
                    name: "House".to_string(),
                    member_ids: vec![ElementId(1), ElementId(2), ElementId(10)],
                    frame: GroupFrame::identity(),
                    composite: None,
                    linked_model: None,
                    cached_bounds: None,
                }
                .to_persisted_json(),
                semantic: None,
            },
        ];
        std::fs::write(
            &path,
            serialize_entity_records_as_project(&world, 11, records).unwrap(),
        )
        .unwrap();

        execute_refresh_linked_models(&mut world, &json!({ "group_ids": [group_id.0] }))
            .expect("refresh should succeed");

        let unrelated_entity = find_entity(&world, ElementId(10)).expect("unrelated id survives");
        let unrelated = world.get::<BoxPrimitive>(unrelated_entity).unwrap();
        assert!(
            unrelated
                .centre
                .abs_diff_eq(Vec3::new(99.0, 0.5, 99.0), 1e-5),
            "refresh must not overwrite unrelated scene id 10"
        );

        let members = group_members(&world, group_id).unwrap();
        assert!(
            !members.member_ids.contains(&ElementId(10)),
            "linked source id 10 must be remapped before becoming a group member"
        );
        let mapped = members
            .linked_model
            .as_ref()
            .unwrap()
            .source_to_scene_ids
            .iter()
            .find(|mapping| mapping.source_id == ElementId(10))
            .expect("new source id should be mapped")
            .scene_id;
        assert!(members.member_ids.contains(&mapped));
        assert_ne!(mapped, ElementId(10));
    }

    #[test]
    fn cancelling_native_placement_undoes_only_its_own_creation() {
        let mut world = test_world();
        let group = spawn_house_group(&mut world);
        let snapshots = [ElementId(1), ElementId(2), group]
            .into_iter()
            .map(|id| capture_snapshot(&world, id).unwrap())
            .collect();
        world
            .resource_mut::<PendingCommandQueue>()
            .push_command(Box::new(CreateLinkedModelInstanceHistoryCommand {
                sidecars: vec![],
                hidden: vec![],
                snapshots,
                instance_root_id: group,
            }));
        apply_pending_history_commands_for_test(&mut world);
        let creation_revision = world.resource::<History>().model_revision();
        world.insert_resource(LinkedPlacementGesture { creation_revision });
        finish_linked_placement_gesture(&mut world, false);
        apply_pending_history_commands_for_test(&mut world);
        assert!(find_entity(&world, group).is_none());
        assert!(find_entity(&world, ElementId(1)).is_none());
        assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    }

    #[test]
    fn linked_opening_and_fill_references_follow_the_instance_mapping() {
        let mut data = json!({
            "element_id": 3, "parent_wall_element_id": 2,
            "opening_feature": { "host_ref": 2, "hosted_fill_ref": 4 },
            "parameters": {"opening_element_id": 3, "host_element_id": 2},
            "width": 2.0, "definition_id": "def-source"
        });
        let ids = [
            (ElementId(2), ElementId(102)),
            (ElementId(3), ElementId(103)),
            (ElementId(4), ElementId(104)),
        ]
        .into_iter()
        .collect();
        remap_element_ids_in_value(&mut data, None, &ids);
        assert_eq!(data["element_id"], 103);
        assert_eq!(data["parent_wall_element_id"], 102);
        assert_eq!(data["opening_feature"]["host_ref"], 102);
        assert_eq!(data["opening_feature"]["hosted_fill_ref"], 104);
        assert_eq!(data["parameters"]["opening_element_id"], 103);
        assert_eq!(data["parameters"]["host_element_id"], 102);
        assert_eq!(data["width"], 2.0);
        assert_eq!(data["definition_id"], "def-source");
    }

    #[test]
    fn linked_definition_scope_remaps_inheritance_occurrences_and_map_keys() {
        let ids = [(
            "def-source".to_string(),
            "linked-site-def-source".to_string(),
        )]
        .into_iter()
        .collect();
        let mut data = json!({"id": "def-source", "base": "def-source",
            "occurrences": [{"definition_id": "def-source"}],
            "by_definition": {"def-source": "unchanged"}, "version": 2});
        remap_definition_ids(&mut data, &ids);
        assert_eq!(data["id"], "linked-site-def-source");
        assert_eq!(data["base"], "linked-site-def-source");
        assert_eq!(
            data["occurrences"][0]["definition_id"],
            "linked-site-def-source"
        );
        assert_eq!(data["by_definition"]["linked-site-def-source"], "unchanged");
        assert_eq!(data["version"], 2);
    }

    #[test]
    fn refresh_linked_model_remaps_internal_semantic_references() {
        let mut world = test_world();
        let group_id = spawn_house_group(&mut world);
        PrimitiveSnapshot {
            element_id: ElementId(10),
            primitive: BoxPrimitive {
                centre: Vec3::new(99.0, 0.5, 99.0),
                half_extents: Vec3::splat(0.5),
            },
            rotation: ShapeRotation::default(),
            layer: None,
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .apply_to(&mut world);

        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        linked_group_snapshot(
            &world,
            group_id,
            vec![ElementId(1), ElementId(2)],
            GroupFrame::identity(),
            Some(LinkedModelRef {
                source_root_is_document: false,
                path: path.to_string_lossy().into_owned(),
                source_root_id: group_id,
                source_to_scene_ids: vec![
                    LinkedModelIdMapping {
                        source_id: ElementId(1),
                        scene_id: ElementId(1),
                    },
                    LinkedModelIdMapping {
                        source_id: ElementId(2),
                        scene_id: ElementId(2),
                    },
                    LinkedModelIdMapping {
                        source_id: group_id,
                        scene_id: group_id,
                    },
                ],
                content_hash: 0,
            }),
        )
        .unwrap()
        .apply_to(&mut world);

        let records = vec![
            persisted_box_record(1, Vec3::new(10.0, 2.25, 20.0)),
            persisted_box_record(2, Vec3::new(10.0, 3.75, 20.0)),
            persisted_box_record(10, Vec3::new(0.0, 1.0, 0.0)),
            PersistedEntityRecord {
                type_name: "group".to_string(),
                data: GroupSnapshot {
                    element_id: group_id,
                    name: "House".to_string(),
                    member_ids: vec![ElementId(1), ElementId(2), ElementId(10)],
                    frame: GroupFrame::identity(),
                    composite: None,
                    linked_model: None,
                    cached_bounds: None,
                }
                .to_persisted_json(),
                semantic: None,
            },
            PersistedEntityRecord {
                type_name: "semantic_assembly".to_string(),
                data: AssemblySnapshot {
                    element_id: ElementId(11),
                    assembly: SemanticAssembly {
                        assembly_type: "house".to_string(),
                        label: "Linked house assembly".to_string(),
                        members: vec![
                            AssemblyMemberRef {
                                target: group_id,
                                role: "linked_model_root".to_string(),
                            },
                            AssemblyMemberRef {
                                target: ElementId(10),
                                role: "new_detail".to_string(),
                            },
                        ],
                        parameters: Value::Null,
                        metadata: Value::Null,
                    },
                    refinement_state: None,
                    obligations: None,
                    claim_grounding: None,
                    authoring_provenance: None,
                }
                .to_persisted_json(),
                semantic: None,
            },
        ];
        std::fs::write(
            &path,
            serialize_entity_records_as_project(&world, 12, records).unwrap(),
        )
        .unwrap();

        execute_refresh_linked_models(&mut world, &json!({ "group_ids": [group_id.0] }))
            .expect("refresh should succeed");

        let linked = group_members(&world, group_id)
            .unwrap()
            .linked_model
            .as_ref()
            .unwrap();
        let mapped_detail = linked
            .source_to_scene_ids
            .iter()
            .find(|mapping| mapping.source_id == ElementId(10))
            .unwrap()
            .scene_id;
        let mapped_assembly = linked
            .source_to_scene_ids
            .iter()
            .find(|mapping| mapping.source_id == ElementId(11))
            .unwrap()
            .scene_id;
        let assembly_entity = find_entity(&world, mapped_assembly).unwrap();
        let assembly = world.get::<SemanticAssembly>(assembly_entity).unwrap();
        assert!(assembly
            .members
            .iter()
            .any(|member| member.target == group_id && member.role == "linked_model_root"));
        assert!(assembly
            .members
            .iter()
            .any(|member| member.target == mapped_detail && member.role == "new_detail"));
    }

    #[test]
    fn refresh_linked_model_imports_linked_project_materials() {
        let mut world = test_world();
        let group_id = spawn_house_group(&mut world);
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");

        linked_group_snapshot(
            &world,
            group_id,
            vec![ElementId(1), ElementId(2)],
            GroupFrame::identity(),
            Some(LinkedModelRef {
                source_root_is_document: false,
                path: path.to_string_lossy().into_owned(),
                source_root_id: group_id,
                source_to_scene_ids: vec![
                    LinkedModelIdMapping {
                        source_id: ElementId(1),
                        scene_id: ElementId(1),
                    },
                    LinkedModelIdMapping {
                        source_id: ElementId(2),
                        scene_id: ElementId(2),
                    },
                    LinkedModelIdMapping {
                        source_id: group_id,
                        scene_id: group_id,
                    },
                ],
                content_hash: 0,
            }),
        )
        .unwrap()
        .apply_to(&mut world);

        let mut linked_world = test_world();
        let mut material = MaterialDef::new("Linked falu red");
        material.id = "mat-linked-falu-red".to_string();
        let mut materials = MaterialRegistry::default();
        materials.upsert(material);
        linked_world.insert_resource(materials);

        let records = vec![
            persisted_box_record_with_material(
                1,
                Vec3::new(10.0, 2.25, 20.0),
                Some(MaterialAssignment::new("mat-linked-falu-red")),
            ),
            persisted_box_record(2, Vec3::new(10.0, 3.75, 20.0)),
            PersistedEntityRecord {
                type_name: "group".to_string(),
                data: GroupSnapshot {
                    element_id: group_id,
                    name: "House".to_string(),
                    member_ids: vec![ElementId(1), ElementId(2)],
                    frame: GroupFrame::identity(),
                    composite: None,
                    linked_model: None,
                    cached_bounds: None,
                }
                .to_persisted_json(),
                semantic: None,
            },
        ];
        std::fs::write(
            &path,
            serialize_entity_records_as_project(&linked_world, 11, records).unwrap(),
        )
        .unwrap();

        assert!(world.get_resource::<MaterialRegistry>().is_none());
        execute_refresh_linked_models(&mut world, &json!({ "group_ids": [group_id.0] }))
            .expect("refresh should succeed");
        assert!(world
            .resource::<MaterialRegistry>()
            .contains("mat-linked-falu-red"));
    }

    fn assert_bounds_close(left: EntityBounds, right: EntityBounds) {
        assert!(
            left.min.abs_diff_eq(right.min, 1e-4) && left.max.abs_diff_eq(right.max, 1e-4),
            "bounds changed: left={left:?}, right={right:?}"
        );
    }

    fn persisted_box_record(element_id: u64, centre: Vec3) -> PersistedEntityRecord {
        persisted_box_record_with_material(element_id, centre, None)
    }

    fn persisted_box_record_with_material(
        element_id: u64,
        centre: Vec3,
        material_assignment: Option<MaterialAssignment>,
    ) -> PersistedEntityRecord {
        PersistedEntityRecord {
            type_name: "box".to_string(),
            data: PrimitiveSnapshot {
                element_id: ElementId(element_id),
                primitive: BoxPrimitive {
                    centre,
                    half_extents: Vec3::splat(0.5),
                },
                rotation: ShapeRotation::default(),
                layer: None,
                material_assignment,
                opening_context: None,
                subobject_display_overrides: None,
            }
            .to_persisted_json(),
            semantic: None,
        }
    }
}
