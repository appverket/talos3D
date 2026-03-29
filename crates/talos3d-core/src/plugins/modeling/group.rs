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

// --- ECS component ---

/// Marker for entities that should be rendered muted during group editing.
#[derive(Component)]
pub struct GroupEditMuted;

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMembers {
    pub name: String,
    pub member_ids: Vec<ElementId>,
}

// --- Snapshot ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupSnapshot {
    pub element_id: ElementId,
    pub name: String,
    pub member_ids: Vec<ElementId>,
    /// When present, this group represents a single composite solid shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite: Option<CompositeSolid>,
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
                composite,
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
        let cached_bounds = compute_group_bounds_from_world(world, &member_ids);
        Ok(GroupSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            name,
            member_ids,
            composite,
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

        // Compute aggregate bounding box across all members (recursively)
        if let Some(bounds) = compute_group_bounds_from_world(world, &members.member_ids) {
            draw_bounds_wireframe(gizmos, &bounds, color);
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

fn find_entity_by_element_id_readonly(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .find(|e| e.get::<ElementId>().copied() == Some(element_id))
        .map(|e| e.id())
}

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
            }
        }
    }

    any.then_some(EntityBounds { min, max })
}

/// Draw a wireframe box from an EntityBounds.
fn draw_bounds_wireframe(gizmos: &mut Gizmos, bounds: &EntityBounds, color: Color) {
    let corners = bounds.corners();
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
