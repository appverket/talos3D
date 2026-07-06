use crate::plugins::commands::find_entity_by_element_id_readonly;
use std::any::Any;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field, read_only_property_field, AuthoredEntity,
        BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityRegistry, HitCandidate, ModelSummaryAccumulator,
    },
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
        modeling::composite_solid::CompositeSolid,
    },
};

// --- Local coordinate frame ---

/// A group's local-to-world coordinate frame: an origin (translation) plus an
/// orientation (rotation). Geometry authored *inside* the group is expressed in
/// this rectified local frame; the frame composes it to world. This is the
/// scene-graph / SketchUp-component model — see ADR-058. Default is identity, so
/// a group with no frame behaves exactly as a flat membership list.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GroupFrame {
    #[serde(default)]
    pub translation: Vec3,
    #[serde(default = "quat_identity")]
    pub rotation: Quat,
}

fn quat_identity() -> Quat {
    Quat::IDENTITY
}

impl Default for GroupFrame {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
        }
    }
}

impl GroupFrame {
    pub fn identity() -> Self {
        Self::default()
    }

    /// True when this frame applies no transform (the common, zero-cost case).
    pub fn is_identity(&self) -> bool {
        self.translation.abs_diff_eq(Vec3::ZERO, 1e-6)
            && self.rotation.abs_diff_eq(Quat::IDENTITY, 1e-6)
    }

    /// Compose another frame expressed in *this* frame's local space onto this
    /// one, yielding a single world-space frame. Used to fold a stack of nested
    /// group frames (root → leaf) into one effective authoring frame.
    pub fn then(&self, local: &GroupFrame) -> GroupFrame {
        GroupFrame {
            translation: self.translation + self.rotation * local.translation,
            rotation: self.rotation * local.rotation,
        }
    }

    /// Map a point given in this frame's local coordinates to world coordinates.
    pub fn point_to_world(&self, local: Vec3) -> Vec3 {
        self.translation + self.rotation * local
    }
}

/// Transform a freshly-authored snapshot from a group's local frame into world
/// space, generically, via the `AuthoredEntity` trait — so every primitive
/// (box, mesh, wall, occurrence, …) composes the same way with no per-type code.
/// The snapshot's local centre `c` maps to `frame.point_to_world(c)` and the
/// whole body is rotated by `frame.rotation`.
pub fn compose_snapshot_into_frame(snapshot: BoxedEntity, frame: &GroupFrame) -> BoxedEntity {
    if frame.is_identity() {
        return snapshot;
    }
    let local_centre = snapshot.center();
    let world_centre = frame.point_to_world(local_centre);
    let rotated = snapshot.rotate_by(frame.rotation);
    rotated.translate_by(world_centre - rotated.center())
}

// --- ECS component ---

/// Marker for entities that should be rendered muted during group editing.
#[derive(Component)]
pub struct GroupEditMuted;

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMembers {
    pub name: String,
    pub member_ids: Vec<ElementId>,
    /// Local-to-world coordinate frame for geometry authored inside this group.
    /// Identity for a plain (flat) group. See ADR-058.
    #[serde(default)]
    pub frame: GroupFrame,
    /// External model file backing this group, when the group is a linked model
    /// instance. The group's frame is the linked model's local-to-scene
    /// transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_model: Option<LinkedModelRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedModelRef {
    pub path: String,
    pub source_root_id: ElementId,
    /// Mapping from element ids in the linked model document to element ids in
    /// this scene instance. The initial conversion preserves ids, but the link
    /// contract is explicit so later placement paths can allocate independent
    /// scene ids without changing the linked document format.
    #[serde(default)]
    pub source_to_scene_ids: Vec<LinkedModelIdMapping>,
    /// Content hash from the last successful load/write. Used by the live
    /// refresh system to skip unchanged files.
    #[serde(default)]
    pub content_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedModelIdMapping {
    pub source_id: ElementId,
    pub scene_id: ElementId,
}

// --- Snapshot ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupSnapshot {
    pub element_id: ElementId,
    pub name: String,
    pub member_ids: Vec<ElementId>,
    /// Local-to-world coordinate frame for geometry authored inside this group
    /// (identity for a plain group). See ADR-058.
    #[serde(default)]
    pub frame: GroupFrame,
    /// When present, this group represents a single composite solid shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite: Option<CompositeSolid>,
    /// External model file backing this group, when the group is a linked model
    /// instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_model: Option<LinkedModelRef>,
    /// Cached aggregate bounding box, computed during snapshot capture.
    #[serde(skip)]
    pub cached_bounds: Option<EntityBounds>,
}

impl From<GroupSnapshot> for BoxedEntity {
    fn from(snapshot: GroupSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl AuthoredEntity for GroupSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "group"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.name.clone()
    }

    fn center(&self) -> Vec3 {
        self.cached_bounds.map(|b| b.center()).unwrap_or(Vec3::ZERO)
    }

    fn bounds(&self) -> Option<EntityBounds> {
        self.cached_bounds
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        // Group translate is handled by transforming members individually
        self.clone().into()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.clone().into()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field(
                "name",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.name.clone())),
            ),
            read_only_property_field(
                "member_count",
                "members",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.member_ids.len() as f32)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "name" => {
                snapshot.name = value
                    .as_str()
                    .ok_or_else(|| "Expected string value for name".to_string())?
                    .to_string();
            }
            _ => return Err(invalid_property_error("group", &["name"])),
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(GroupSnapshotJson::Group(self.clone())).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            // Update existing
            if let Some(mut members) = world.get_mut::<GroupMembers>(entity) {
                members.name = self.name.clone();
                members.member_ids = self.member_ids.clone();
                members.frame = self.frame;
                members.linked_model = self.linked_model.clone();
            }
            let mut entity_mut = world.entity_mut(entity);
            if let Some(composite) = &self.composite {
                entity_mut.insert(composite.clone());
            } else {
                entity_mut.remove::<CompositeSolid>();
            }
        } else {
            // Create new
            let mut entity = world.spawn((
                self.element_id,
                GroupMembers {
                    name: self.name.clone(),
                    member_ids: self.member_ids.clone(),
                    frame: self.frame,
                    linked_model: self.linked_model.clone(),
                },
            ));
            if let Some(composite) = &self.composite {
                entity.insert(composite.clone());
            }
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {
        // Groups have no geometry preview
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }
}

// --- JSON wrapper for serde dispatch ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum GroupSnapshotJson {
    Group(GroupSnapshot),
}

// --- Factory ---

pub struct GroupFactory;

impl AuthoredEntityFactory for GroupFactory {
    fn type_name(&self) -> &'static str {
        "group"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let members = entity_ref.get::<GroupMembers>()?;
        let composite = entity_ref.get::<CompositeSolid>().cloned();
        let cached_bounds = compute_group_bounds_from_world(world, &members.member_ids);
        Some(
            GroupSnapshot {
                element_id,
                name: members.name.clone(),
                member_ids: members.member_ids.clone(),
                frame: members.frame,
                composite,
                linked_model: members.linked_model.clone(),
                cached_bounds,
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<GroupSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            GroupSnapshotJson::Group(snapshot) => Ok(snapshot.into()),
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let name = object
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Group")
            .to_string();
        let member_ids: Vec<ElementId> = object
            .get("member_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64().map(ElementId))
                    .collect()
            })
            .unwrap_or_default();

        let composite = object
            .get("composite")
            .and_then(|v| serde_json::from_value::<CompositeSolid>(v.clone()).ok());
        // Optional initial local frame: { "frame": { "translation": [x,y,z],
        // "rotation": [x,y,z,w] } } or { "frame_origin": [..], "frame_rotate_euler_deg": [..] }.
        let frame = parse_group_frame(object);
        let cached_bounds = compute_group_bounds_from_world(world, &member_ids);
        Ok(GroupSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            name,
            member_ids,
            frame,
            composite,
            linked_model: None,
            cached_bounds,
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(members) = entity_ref.get::<GroupMembers>() else {
            return;
        };

        if !members.frame.is_identity() {
            if let Some(bounds) =
                compute_group_bounds_in_frame_from_world(world, &members.member_ids, &members.frame)
            {
                draw_oriented_bounds_wireframe(gizmos, &bounds, &members.frame, color);
                return;
            }
        }

        if let Some(bounds) = compute_group_bounds_from_world(world, &members.member_ids) {
            draw_axis_aligned_bounds_wireframe(gizmos, &bounds, color);
        }
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        let Some(members) = entity_ref.get::<GroupMembers>() else {
            return 0;
        };
        if compute_group_bounds_from_world(world, &members.member_ids).is_some() {
            12 // wireframe box = 12 edges
        } else {
            0
        }
    }

    fn hit_test(&self, _world: &World, _ray: Ray3d) -> Option<HitCandidate> {
        // Group hit testing is handled by member hit testing in selection.rs
        None
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in q.iter(world) {
            if entity_ref.get::<GroupMembers>().is_some() && entity_ref.get::<ElementId>().is_some()
            {
                *summary
                    .entity_counts
                    .entry("group".to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    fn collect_delete_dependencies(
        &self,
        world: &World,
        requested_ids: &[ElementId],
        out: &mut Vec<ElementId>,
    ) {
        // When a group is deleted, also delete its members
        let mut q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in q.iter(world) {
            let (Some(element_id), Some(members)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<GroupMembers>(),
            ) else {
                continue;
            };
            if requested_ids.contains(element_id) {
                for member_id in &members.member_ids {
                    if !out.contains(member_id) {
                        out.push(*member_id);
                    }
                }
            }
        }
    }
}

/// Parse an optional local frame from a create/update request object. Accepts
/// either a nested `frame` object or top-level `frame_origin` /
/// `frame_rotate_euler_deg` (degrees, XYZ order) for ergonomic agent authoring.
pub fn parse_group_frame(object: &serde_json::Map<String, Value>) -> GroupFrame {
    if let Some(frame) = object
        .get("frame")
        .and_then(|v| serde_json::from_value::<GroupFrame>(v.clone()).ok())
    {
        return frame;
    }
    let translation = object
        .get("frame_origin")
        .and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok())
        .map(Vec3::from)
        .unwrap_or(Vec3::ZERO);
    let rotation = object
        .get("frame_rotate_euler_deg")
        .and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok())
        .map(|[x, y, z]| {
            Quat::from_euler(
                EulerRot::XYZ,
                x.to_radians(),
                y.to_radians(),
                z.to_radians(),
            )
        })
        .unwrap_or(Quat::IDENTITY);
    GroupFrame {
        translation,
        rotation,
    }
}

// --- Editing context ---

#[derive(Resource, Debug, Clone, Default)]
pub struct GroupEditContext {
    /// Stack of group ElementIds being edited. Empty = root level.
    pub stack: Vec<ElementId>,
}

impl GroupEditContext {
    pub fn is_root(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn current_group(&self) -> Option<ElementId> {
        self.stack.last().copied()
    }

    /// The effective authoring frame: the product of every group frame on the
    /// edit stack, folded root → leaf. Identity when at root or when no entered
    /// group carries a non-identity frame. Geometry authored now is expressed in
    /// this frame's local coordinates and composed to world by it.
    pub fn active_frame(&self, world: &World) -> GroupFrame {
        let mut frame = GroupFrame::identity();
        for id in &self.stack {
            if let Some(entity) = find_entity_by_element_id_readonly(world, *id) {
                if let Some(members) = world.get::<GroupMembers>(entity) {
                    frame = frame.then(&members.frame);
                }
            }
        }
        frame
    }

    pub fn enter(&mut self, group_id: ElementId) {
        self.stack.push(group_id);
    }

    pub fn exit(&mut self) {
        self.stack.pop();
    }

    pub fn reset(&mut self) {
        self.stack.clear();
    }

    pub fn breadcrumb(&self, world: &World) -> String {
        if self.stack.is_empty() {
            return String::new();
        }
        self.stack
            .iter()
            .filter_map(|id| {
                find_entity_by_element_id_readonly(world, *id)
                    .and_then(|e| world.get::<GroupMembers>(e))
                    .map(|m| m.name.as_str())
            })
            .collect::<Vec<_>>()
            .join(" > ")
    }
}

// --- Helper: find entity without &mut World ---

// --- Utility: find which group owns a member ---

pub fn find_group_for_member(world: &World, member_id: ElementId) -> Option<ElementId> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world).find_map(|e| {
        let group_id = e.get::<ElementId>()?;
        let members = e.get::<GroupMembers>()?;
        members.member_ids.contains(&member_id).then_some(*group_id)
    })
}

/// Collect all member IDs of a group recursively (including nested group members).
pub fn collect_group_members_recursive(world: &World, group_id: ElementId) -> Vec<ElementId> {
    let mut result = Vec::new();
    let mut stack = vec![group_id];
    while let Some(id) = stack.pop() {
        if let Some(entity) = find_entity_by_element_id_readonly(world, id) {
            if let Some(members) = world.get::<GroupMembers>(entity) {
                for member_id in &members.member_ids {
                    result.push(*member_id);
                    stack.push(*member_id);
                }
            }
        }
    }
    result
}

/// Compute the aggregate bounding box for a set of member IDs (recursively).
pub fn compute_group_bounds_from_world(
    world: &World,
    member_ids: &[ElementId],
) -> Option<EntityBounds> {
    let registry = world.resource::<CapabilityRegistry>();
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;

    let mut stack: Vec<ElementId> = member_ids.to_vec();
    while let Some(id) = stack.pop() {
        let mut q = world.try_query::<EntityRef>().unwrap();
        let Some(entity_ref) = q
            .iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(id))
        else {
            continue;
        };
        if let Some(members) = entity_ref.get::<GroupMembers>() {
            // Recurse into nested groups
            stack.extend_from_slice(&members.member_ids);
        } else if let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) {
            if let Some(bounds) = snapshot.bounds() {
                min = min.min(bounds.min);
                max = max.max(bounds.max);
                any = true;
            } else if let Some(aabb) = entity_ref.get::<bevy::camera::primitives::Aabb>() {
                // Members that expose no authored bounds (e.g. terrain-conforming
                // solids, whose top is terrain-derived) still contribute to the group
                // box via their rendered mesh AABB. Their Transform is identity (mesh
                // is built in world space), so the AABB is already world-space.
                let center = Vec3::from(aabb.center);
                let half = Vec3::from(aabb.half_extents);
                min = min.min(center - half);
                max = max.max(center + half);
                any = true;
            }
        }
    }

    any.then_some(EntityBounds { min, max })
}

/// Compute aggregate member bounds in a group's local frame from world-authored
/// member geometry. Prefer snap/wireframe points where entities expose them so
/// a rotated linked model gets an object-aligned box instead of an inflated
/// inverse-transform of each member's world AABB.
pub fn compute_group_bounds_in_frame_from_world(
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

    let mut stack: Vec<ElementId> = member_ids.to_vec();
    while let Some(id) = stack.pop() {
        let mut q = world.try_query::<EntityRef>().unwrap();
        let Some(entity_ref) = q
            .iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(id))
        else {
            continue;
        };
        if let Some(members) = entity_ref.get::<GroupMembers>() {
            stack.extend_from_slice(&members.member_ids);
            continue;
        }

        if let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) {
            let segments = snapshot.0.snap_segments();
            if segments.is_empty() {
                if let Some(bounds) = snapshot.bounds() {
                    for corner in bounds.corners() {
                        let local = to_local(corner);
                        min = min.min(local);
                        max = max.max(local);
                        any = true;
                    }
                }
            } else {
                for (start, end) in segments {
                    for point in [start, end] {
                        let local = to_local(point);
                        min = min.min(local);
                        max = max.max(local);
                        any = true;
                    }
                }
            }
        } else if let Some(aabb) = entity_ref.get::<bevy::camera::primitives::Aabb>() {
            let center = Vec3::from(aabb.center);
            let half = Vec3::from(aabb.half_extents);
            let bounds = EntityBounds {
                min: center - half,
                max: center + half,
            };
            for corner in bounds.corners() {
                let local = to_local(corner);
                min = min.min(local);
                max = max.max(local);
                any = true;
            }
        }
    }

    any.then_some(EntityBounds { min, max })
}

fn draw_axis_aligned_bounds_wireframe(gizmos: &mut Gizmos, bounds: &EntityBounds, color: Color) {
    draw_bounds_corners_wireframe(gizmos, &bounds.corners(), color);
}

fn draw_oriented_bounds_wireframe(
    gizmos: &mut Gizmos,
    bounds: &EntityBounds,
    frame: &GroupFrame,
    color: Color,
) {
    let corners = bounds.corners().map(|corner| frame.point_to_world(corner));
    draw_bounds_corners_wireframe(gizmos, &corners, color);
}

fn draw_bounds_corners_wireframe(gizmos: &mut Gizmos, corners: &[Vec3; 8], color: Color) {
    // Bottom face edges (0-1-2-3)
    for i in 0..4 {
        gizmos.line(corners[i], corners[(i + 1) % 4], color);
    }
    // Top face edges (4-5-6-7)
    for i in 4..8 {
        gizmos.line(corners[i], corners[4 + (i - 4 + 1) % 4], color);
    }
    // Vertical edges
    for i in 0..4 {
        gizmos.line(corners[i], corners[i + 4], color);
    }
}

/// True when the element is a group.
pub fn is_group(world: &World, element_id: ElementId) -> bool {
    find_entity_by_element_id_readonly(world, element_id)
        .and_then(|e| world.get::<GroupMembers>(e))
        .is_some()
}

/// Read a group's local frame, if it is a group.
pub fn group_frame(world: &World, element_id: ElementId) -> Option<GroupFrame> {
    let entity = find_entity_by_element_id_readonly(world, element_id)?;
    world.get::<GroupMembers>(entity).map(|m| m.frame)
}

/// Build before/after group snapshots that set `group_id`'s frame to `new_frame`,
/// so a frame change flows through command/history (ADR-002).
pub fn group_frame_change_snapshots(
    world: &World,
    group_id: ElementId,
    new_frame: GroupFrame,
) -> Option<(BoxedEntity, BoxedEntity)> {
    let entity = find_entity_by_element_id_readonly(world, group_id)?;
    let members = world.get::<GroupMembers>(entity)?;
    let composite = world.get::<CompositeSolid>(entity).cloned();
    let mk = |frame: GroupFrame| -> BoxedEntity {
        GroupSnapshot {
            element_id: group_id,
            name: members.name.clone(),
            member_ids: members.member_ids.clone(),
            frame,
            composite: composite.clone(),
            linked_model: members.linked_model.clone(),
            cached_bounds: None,
        }
        .into()
    };
    Some((mk(members.frame), mk(new_frame)))
}

/// Build before/after group snapshots that add `new_member` to `group_id`, so a
/// membership change can flow through the command/history pipeline (ADR-002)
/// rather than mutating ECS state directly. Returns `None` if the group is
/// missing or already contains the member.
pub fn group_membership_add_snapshots(
    world: &World,
    group_id: ElementId,
    new_member: ElementId,
) -> Option<(BoxedEntity, BoxedEntity)> {
    let entity = find_entity_by_element_id_readonly(world, group_id)?;
    let members = world.get::<GroupMembers>(entity)?;
    if members.member_ids.contains(&new_member) {
        return None;
    }
    let composite = world.get::<CompositeSolid>(entity).cloned();
    let mk = |ids: Vec<ElementId>| -> BoxedEntity {
        GroupSnapshot {
            element_id: group_id,
            name: members.name.clone(),
            member_ids: ids,
            frame: members.frame,
            composite: composite.clone(),
            linked_model: members.linked_model.clone(),
            cached_bounds: None,
        }
        .into()
    };
    let before = mk(members.member_ids.clone());
    let mut after_ids = members.member_ids.clone();
    after_ids.push(new_member);
    let after = mk(after_ids);
    Some((before, after))
}

/// Remove a member from any group that contains it.
pub fn remove_member_from_groups(world: &mut World, member_id: ElementId) {
    let mut updates = Vec::new();
    {
        let mut query = world.query::<(Entity, &GroupMembers)>();
        for (entity, members) in query.iter(world) {
            if members.member_ids.contains(&member_id) {
                let mut new_ids = members.member_ids.clone();
                new_ids.retain(|id| *id != member_id);
                updates.push((entity, new_ids));
            }
        }
    }
    for (entity, new_ids) in updates {
        if let Some(mut members) = world.get_mut::<GroupMembers>(entity) {
            members.member_ids = new_ids;
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    fn approx(a: Vec3, b: Vec3) -> bool {
        a.abs_diff_eq(b, 1e-4)
    }

    #[test]
    fn identity_frame_is_a_noop() {
        let f = GroupFrame::identity();
        assert!(f.is_identity());
        assert!(approx(
            f.point_to_world(Vec3::new(3.0, 1.0, 2.0)),
            Vec3::new(3.0, 1.0, 2.0)
        ));
    }

    #[test]
    fn point_to_world_applies_rotation_then_translation() {
        // 90° about Y maps local +X to world -Z, then offset by the origin.
        let f = GroupFrame {
            translation: Vec3::new(10.0, 0.0, 5.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        };
        let world = f.point_to_world(Vec3::X);
        assert!(approx(world, Vec3::new(10.0, 0.0, 4.0)), "got {world:?}");
        assert!(!f.is_identity());
    }

    #[test]
    fn then_composes_nested_frames_root_to_leaf() {
        // Parent: +90° about Y at origin. Child (in parent's space): translate +X by 2.
        let parent = GroupFrame {
            translation: Vec3::ZERO,
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        };
        let child = GroupFrame {
            translation: Vec3::new(2.0, 0.0, 0.0),
            rotation: Quat::IDENTITY,
        };
        let composed = parent.then(&child);
        // Child origin (2,0,0) in parent space rotates to (0,0,-2) in world.
        assert!(
            approx(composed.translation, Vec3::new(0.0, 0.0, -2.0)),
            "got {:?}",
            composed.translation
        );
        // A point at child-local +X = world: parent rot applied to (2+1,0,0)=(3,0,0) -> (0,0,-3).
        assert!(approx(
            composed.point_to_world(Vec3::X),
            Vec3::new(0.0, 0.0, -3.0)
        ));
    }
}
