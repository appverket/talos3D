use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use bevy::{ecs::world::EntityRef, prelude::*};
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
        identity::{ElementId, ElementIdAllocator},
        materials::{ensure_builtin_materials, MaterialRegistry, TextureRegistry},
        modeling::{
            dependency_graph::stamp_authored_entity_dependencies,
            group::{
                collect_group_members_recursive, compose_snapshot_into_frame,
                compute_group_bounds_from_world, compute_group_bounds_in_frame_from_world,
                GroupFrame, GroupMembers, GroupSnapshot, LinkedModelIdMapping, LinkedModelRef,
            },
            primitives::TriangleMesh,
            snapshots::{EditableMeshSnapshot, TriangleMeshSnapshot},
        },
        persistence::{
            deserialize_project_entity_records_with_assets, load_project_from_path,
            serialize_entity_records_as_project, PersistedEntityRecord, ProjectEntityRecords,
        },
        selection::Selected,
        storage::Storage,
    },
};

const RELOAD_INTERVAL_SECONDS: f32 = 1.0;

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
    after.apply_to(world);
    world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .write(ApplyEntityChangesCommand {
            label: "Create linked model",
            before: vec![before],
            after: vec![after.clone()],
        });

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

pub fn execute_place_linked_model(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let Some(path) = place_linked_model_path(world, params)? else {
        return Ok(CommandResult::empty());
    };
    let key = path.to_string_lossy().into_owned();
    let bytes = world.resource::<Storage>().0.load(&key)?;
    let hash = content_hash(&bytes);
    let linked_project = load_linked_project(world, &bytes)?;
    let root = linked_project_root_group(&linked_project)?;
    let group_id = world.resource::<ElementIdAllocator>().next_id();
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(root.name.as_str())
        .to_string();
    let linked = LinkedModelRef {
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

    let mut created = collect_group_members_recursive(world, group_id);
    created.push(group_id);
    created.sort_by_key(|id| id.0);
    created.dedup();

    Ok(CommandResult {
        created: created.iter().map(|id| id.0).collect(),
        modified: Vec::new(),
        deleted: Vec::new(),
        output: Some(json!({
            "path": key,
            "root_group_id": group_id.0,
            "source_root_id": root.element_id.0,
            "content_hash": hash,
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
    for group_id in group_ids {
        if refresh_linked_group(world, group_id, force)?.is_some() {
            modified.push(group_id.0);
        }
    }
    Ok(CommandResult {
        modified,
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

fn place_linked_model_path(world: &World, params: &Value) -> Result<Option<PathBuf>, String> {
    if let Some(path) = params.get("path").and_then(Value::as_str) {
        if path.trim().is_empty() {
            return Err("path must not be empty".into());
        }
        return Ok(Some(PathBuf::from(path)));
    }
    pick_linked_model_file(world)
}

#[cfg(target_arch = "wasm32")]
fn pick_linked_model_file(_world: &World) -> Result<Option<PathBuf>, String> {
    Err("Native open dialogs are not available in the browser shell".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn pick_linked_model_file(world: &World) -> Result<Option<PathBuf>, String> {
    let mut dialog = rfd::FileDialog::new().add_filter("Talos3D Project", &["talos3d"]);
    if let Some(parent) = world
        .get_resource::<DocumentState>()
        .and_then(|state| state.current_path.as_ref())
        .and_then(|path| path.parent())
    {
        dialog = dialog.set_directory(parent);
    }
    Ok(dialog.pick_file())
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

    let source_ids = ids.clone();
    let mut snapshots = ids
        .into_iter()
        .map(|id| capture_snapshot(world, id))
        .collect::<Result<Vec<_>, _>>()?;
    snapshots.sort_by_key(snapshot_dependency_order);

    let mut records = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        let normalized = normalize_snapshot_for_linked_file(snapshot, group_id, &frame)?;
        records.push(PersistedEntityRecord {
            type_name: normalized.type_name().to_string(),
            data: normalized.to_persisted_json(),
            semantic: None,
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
    let linked_project = load_linked_project(world, &bytes)?;
    apply_linked_snapshots(world, group_id, linked, hash, linked_project)?;
    Ok(Some(hash))
}

fn load_linked_project(world: &World, bytes: &[u8]) -> Result<LinkedProject, String> {
    let ProjectEntityRecords {
        entities: records,
        materials,
        textures,
    } = deserialize_project_entity_records_with_assets(bytes)?;
    let registry = world.resource::<CapabilityRegistry>();
    let mut snapshots = Vec::with_capacity(records.len());
    for record in records {
        let factory = registry
            .factory_for(&record.type_name)
            .ok_or_else(|| format!("No factory registered for '{}'", record.type_name))?;
        snapshots.push(factory.from_persisted_json(&record.data)?);
    }
    snapshots.sort_by_key(snapshot_dependency_order);
    Ok(LinkedProject {
        snapshots,
        materials,
        textures,
    })
}

struct LinkedProject {
    snapshots: Vec<BoxedEntity>,
    materials: Option<MaterialRegistry>,
    textures: Option<TextureRegistry>,
}

fn linked_project_root_group(linked_project: &LinkedProject) -> Result<GroupSnapshot, String> {
    let groups = linked_project
        .snapshots
        .iter()
        .filter_map(|snapshot| snapshot.0.as_any().downcast_ref::<GroupSnapshot>())
        .cloned()
        .collect::<Vec<_>>();
    if groups.is_empty() {
        return Err("Linked model file does not contain a group to place".to_string());
    }

    let group_ids = groups
        .iter()
        .map(|group| group.element_id)
        .collect::<HashSet<_>>();
    let child_group_ids = groups
        .iter()
        .flat_map(|group| group.member_ids.iter().copied())
        .filter(|id| group_ids.contains(id))
        .collect::<HashSet<_>>();
    let mut root_candidates = groups
        .iter()
        .filter(|group| !child_group_ids.contains(&group.element_id))
        .cloned()
        .collect::<Vec<_>>();
    if root_candidates.is_empty() {
        root_candidates = groups;
    }
    root_candidates.sort_by_key(|group| group.element_id.0);
    root_candidates
        .into_iter()
        .next()
        .ok_or_else(|| "Linked model file does not contain a group to place".to_string())
}

fn apply_linked_snapshots(
    world: &mut World,
    group_id: ElementId,
    mut linked: LinkedModelRef,
    hash: u64,
    linked_project: LinkedProject,
) -> Result<(), String> {
    merge_linked_project_assets(world, linked_project.materials, linked_project.textures);
    let snapshots = linked_project.snapshots;
    let root = snapshots
        .iter()
        .find_map(|snapshot| snapshot.0.as_any().downcast_ref::<GroupSnapshot>())
        .filter(|group| group.element_id == linked.source_root_id)
        .cloned()
        .ok_or_else(|| "Linked model file is missing its root group".to_string())?;
    let frame = current_group_frame(world, group_id);
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
        let composed = compose_linked_snapshot(remapped, &frame);
        composed.apply_to(world);
        stamp_authored_entity_dependencies(world, &composed);
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
    let root_member_ids = root
        .member_ids
        .iter()
        .filter_map(|source_id| id_map.get(source_id).copied())
        .collect();
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
    matches!(
        key,
        "element_id" | "source" | "target" | "source_root_id" | "source_id" | "scene_id"
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
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(world);
        PrimitiveSnapshot {
            element_id: ElementId(2),
            primitive: BoxPrimitive {
                centre: Vec3::new(10.0, 3.75, 20.0),
                half_extents: Vec3::new(3.0, 1.25, 2.5),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
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
            material_assignment: None,
            opening_context: None,
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
            material_assignment: None,
            opening_context: None,
        }
        .apply_to(&mut world);

        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("house.talos3d");
        let linked = LinkedModelRef {
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
            material_assignment: None,
            opening_context: None,
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
                material_assignment,
                opening_context: None,
            }
            .to_persisted_json(),
            semantic: None,
        }
    }
}
