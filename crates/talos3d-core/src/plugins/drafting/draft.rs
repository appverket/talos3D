//! Durable Draft metadata: one plane, paper/layout policy, defaults, and
//! references to authored content.
//!
//! A Draft never owns geometry. `members` are stable [`ElementId`] references
//! resolved against the authored model/drawing-metadata registries when a
//! [`DrawingScene`](crate::plugins::drafting_sheet::DrawingScene) is derived.

use std::{any::Any, collections::HashMap};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, read_only_property_field, scalar_from_json,
        vec3_from_json, AuthoredEntity, BoxedEntity, EntityBounds, EntityScope, HandleInfo,
        PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, CapabilityRegistry, FaceId, HitCandidate},
    plugins::{
        commands::{
            enqueue_apply_entity_changes, enqueue_create_boxed_entity, find_entity_by_element_id,
            find_entity_by_element_id_readonly, ApplyEntityChangesCommand,
        },
        cursor::DrawingPlane,
        document_properties::DocumentProperties,
        identity::{ElementId, ElementIdAllocator},
    },
};

use super::workspace::DraftingWorkspaceState;

pub const DRAFT_TYPE: &str = "draft";
pub const DRAFTS_METADATA_KEY: &str = "drafts";

/// ADR-026 name for the same plane frame already consumed by every drawing
/// tool. The alias prevents a second coordinate-conversion implementation.
pub type DraftPlane = DrawingPlane;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftLayout {
    pub scale_denominator: f32,
    pub paper_width_mm: f32,
    pub paper_height_mm: f32,
    pub margin_mm: f32,
}

impl Default for DraftLayout {
    fn default() -> Self {
        Self {
            scale_denominator: 50.0,
            paper_width_mm: 297.0,
            paper_height_mm: 210.0,
            margin_mm: 10.0,
        }
    }
}

impl DraftLayout {
    pub fn validate(&self) -> Result<(), String> {
        if !self.scale_denominator.is_finite() || self.scale_denominator <= 0.0 {
            return Err("draft scale_denominator must be finite and positive".to_string());
        }
        if !self.paper_width_mm.is_finite()
            || !self.paper_height_mm.is_finite()
            || self.paper_width_mm <= 0.0
            || self.paper_height_mm <= 0.0
        {
            return Err("draft paper dimensions must be finite and positive".to_string());
        }
        if !self.margin_mm.is_finite()
            || self.margin_mm < 0.0
            || self.margin_mm * 2.0 >= self.paper_width_mm.min(self.paper_height_mm)
        {
            return Err("draft margin must fit inside the paper dimensions".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftDefaults {
    /// Reference into the existing document [`LayerRegistry`](crate::plugins::layers::LayerRegistry).
    pub layer: String,
    /// Reference into the existing [`DimensionStyleRegistry`](super::DimensionStyleRegistry).
    pub dimension_style: String,
}

impl Default for DraftDefaults {
    fn default() -> Self {
        Self {
            layer: "Default".to_string(),
            dimension_style: "architectural_metric".to_string(),
        }
    }
}

/// Runtime ECS representation of one durable Draft.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftNode {
    pub name: String,
    pub plane: DraftPlane,
    pub layout: DraftLayout,
    pub defaults: DraftDefaults,
    /// Sorted, duplicate-free references. The referenced entities retain their
    /// own lifecycle and persistence; the Draft owns no snapshots or geometry.
    pub members: Vec<ElementId>,
}

impl DraftNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            // Camera-aligned top view: the Draft normal is the camera's
            // forward/view direction, while +X/+Y are tangent/bitangent.
            // This remains the same DrawingPlane frame used by tools; it is
            // intentionally not the upward extrusion normal of `ground()`.
            plane: DraftPlane {
                origin: Vec3::ZERO,
                normal: Vec3::NEG_Y,
                tangent: Vec3::X,
                bitangent: Vec3::NEG_Z,
            },
            layout: DraftLayout::default(),
            defaults: DraftDefaults::default(),
            members: Vec::new(),
        }
    }

    pub fn normalize_and_validate(&mut self) -> Result<(), String> {
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            return Err("draft name must not be empty".to_string());
        }
        self.plane.validate()?;
        self.layout.validate()?;
        self.defaults.layer = self.defaults.layer.trim().to_string();
        self.defaults.dimension_style = self.defaults.dimension_style.trim().to_string();
        if self.defaults.layer.is_empty() || self.defaults.dimension_style.is_empty() {
            return Err("draft layer and dimension style references must not be empty".to_string());
        }
        self.members.sort_by_key(|member| member.0);
        self.members.dedup();
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, element_id: ElementId) -> bool {
        self.members
            .binary_search_by_key(&element_id.0, |id| id.0)
            .is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftSnapshot {
    pub element_id: ElementId,
    pub node: DraftNode,
}

impl AuthoredEntity for DraftSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        DRAFT_TYPE
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.node.name.clone()
    }

    fn center(&self) -> Vec3 {
        self.node.plane.origin
    }

    fn scope(&self) -> EntityScope {
        EntityScope::DrawingMetadata
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        next.node.plane.origin += delta;
        next.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut next = self.clone();
        next.node.plane.origin = rotation * next.node.plane.origin;
        next.node.plane.normal = rotation * next.node.plane.normal;
        next.node.plane.tangent = rotation * next.node.plane.tangent;
        next.node.plane.bitangent = rotation * next.node.plane.bitangent;
        next.into()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn push_pull(&self, _face_id: FaceId, _distance: f32) -> Option<BoxedEntity> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "name",
                "Name",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.name.clone())),
                true,
            ),
            property_field_with(
                "scale_denominator",
                "Scale denominator",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.layout.scale_denominator)),
                true,
            ),
            property_field_with(
                "paper_width_mm",
                "Paper width (mm)",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.layout.paper_width_mm)),
                true,
            ),
            property_field_with(
                "paper_height_mm",
                "Paper height (mm)",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.layout.paper_height_mm)),
                true,
            ),
            property_field_with(
                "margin_mm",
                "Margin (mm)",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.layout.margin_mm)),
                true,
            ),
            property_field_with(
                "default_layer",
                "Default layer",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.defaults.layer.clone())),
                true,
            ),
            property_field_with(
                "default_dimension_style",
                "Default dimension style",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(
                    self.node.defaults.dimension_style.clone(),
                )),
                true,
            ),
            read_only_property_field(
                "member_count",
                "Members",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.members.len() as f32)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut next = self.clone();
        match property_name {
            "name" => {
                next.node.name = value
                    .as_str()
                    .ok_or_else(|| "name must be a string".to_string())?
                    .to_string();
            }
            "scale_denominator" => next.node.layout.scale_denominator = scalar_from_json(value)?,
            "paper_width_mm" => next.node.layout.paper_width_mm = scalar_from_json(value)?,
            "paper_height_mm" => next.node.layout.paper_height_mm = scalar_from_json(value)?,
            "margin_mm" => next.node.layout.margin_mm = scalar_from_json(value)?,
            "default_layer" => {
                next.node.defaults.layer = value
                    .as_str()
                    .ok_or_else(|| "default_layer must be a string".to_string())?
                    .to_string();
            }
            "default_dimension_style" => {
                next.node.defaults.dimension_style = value
                    .as_str()
                    .ok_or_else(|| "default_dimension_style must be a string".to_string())?
                    .to_string();
            }
            _ => {
                return Err(invalid_property_error(
                    DRAFT_TYPE,
                    &[
                        "name",
                        "scale_denominator",
                        "paper_width_mm",
                        "paper_height_mm",
                        "margin_mm",
                        "default_layer",
                        "default_dimension_style",
                    ],
                ));
            }
        }
        next.node.normalize_and_validate()?;
        Ok(next.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn to_json(&self) -> Value {
        json!({
            "element_id": self.element_id.0,
            "name": self.node.name,
            "plane": self.node.plane,
            "layout": self.node.layout,
            "defaults": self.node.defaults,
            "members": self.node.members.iter().map(|id| id.0).collect::<Vec<_>>(),
        })
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert(self.node.clone());
        } else {
            world.spawn((self.element_id, self.node.clone()));
        }
    }

    fn remove_from(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let _ = world.despawn(entity);
        }
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == DRAFT_TYPE && other.to_json() == self.to_json()
    }
}

impl From<DraftSnapshot> for BoxedEntity {
    fn from(snapshot: DraftSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

pub struct DraftFactory;

impl AuthoredEntityFactory for DraftFactory {
    fn type_name(&self) -> &'static str {
        DRAFT_TYPE
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        Some(
            DraftSnapshot {
                element_id: *entity_ref.get::<ElementId>()?,
                node: entity_ref.get::<DraftNode>()?.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        draft_snapshot_from_json(data, None).map(Into::into)
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();
        draft_snapshot_from_json(request, Some(element_id)).map(Into::into)
    }

    fn hit_test(&self, _world: &World, _ray: Ray3d) -> Option<HitCandidate> {
        None
    }
}

fn draft_snapshot_from_json(
    data: &Value,
    allocated_element_id: Option<ElementId>,
) -> Result<DraftSnapshot, String> {
    let element_id = allocated_element_id
        .or_else(|| {
            data.get("element_id")
                .and_then(Value::as_u64)
                .map(ElementId)
        })
        .ok_or_else(|| "draft element_id is required".to_string())?;
    let name = data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Draft")
        .to_string();

    let origin = data
        .get("origin")
        .or_else(|| data.pointer("/plane/origin"))
        .map(vec3_from_json)
        .transpose()?
        .unwrap_or(Vec3::ZERO);
    let normal = data
        .get("normal")
        .or_else(|| data.pointer("/plane/normal"))
        .map(vec3_from_json)
        .transpose()?
        .unwrap_or(Vec3::NEG_Y);
    let tangent = data
        .get("tangent")
        .or_else(|| data.pointer("/plane/tangent"))
        .map(vec3_from_json)
        .transpose()?
        .unwrap_or(Vec3::X);
    let plane = DraftPlane::try_from_origin_normal_tangent(origin, normal, tangent)?;

    let mut layout = DraftLayout::default();
    if let Some(value) = number_at(data, "scale_denominator", "/layout/scale_denominator") {
        layout.scale_denominator = value;
    }
    if let Some(value) = number_at(data, "paper_width_mm", "/layout/paper_width_mm") {
        layout.paper_width_mm = value;
    }
    if let Some(value) = number_at(data, "paper_height_mm", "/layout/paper_height_mm") {
        layout.paper_height_mm = value;
    }
    if let Some(value) = number_at(data, "margin_mm", "/layout/margin_mm") {
        layout.margin_mm = value;
    }

    let mut defaults = DraftDefaults::default();
    if let Some(value) = string_at(data, "default_layer", "/defaults/layer") {
        defaults.layer = value.to_string();
    }
    if let Some(value) = string_at(data, "default_dimension_style", "/defaults/dimension_style") {
        defaults.dimension_style = value.to_string();
    }

    let members = data
        .get("members")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .map(ElementId)
                        .ok_or_else(|| "draft members must be element-id integers".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    let mut node = DraftNode {
        name,
        plane,
        layout,
        defaults,
        members,
    };
    node.normalize_and_validate()?;
    Ok(DraftSnapshot { element_id, node })
}

fn number_at(data: &Value, direct: &str, pointer: &str) -> Option<f32> {
    data.get(direct)
        .or_else(|| data.pointer(pointer))
        .and_then(Value::as_f64)
        .map(|value| value as f32)
}

fn string_at<'a>(data: &'a Value, direct: &str, pointer: &str) -> Option<&'a str> {
    data.get(direct)
        .or_else(|| data.pointer(pointer))
        .and_then(Value::as_str)
}

#[derive(Resource, Default)]
pub(crate) struct DraftSyncState {
    last_serialized: Option<Value>,
}

pub(crate) fn sync_drafts(world: &mut World) {
    if !world.contains_resource::<DocumentProperties>() {
        return;
    }
    let saved = world
        .resource::<DocumentProperties>()
        .domain_defaults
        .get(DRAFTS_METADATA_KEY)
        .cloned();
    let saved_changed = saved != world.resource::<DraftSyncState>().last_serialized;

    if saved_changed {
        match saved.as_ref() {
            Some(value) => {
                let Some(snapshots) = deserialize_drafts(value) else {
                    world.resource_mut::<DraftSyncState>().last_serialized = saved;
                    return;
                };
                apply_drafts_to_world(world, &snapshots);
            }
            None => apply_drafts_to_world(world, &[]),
        }
        world.resource_mut::<DraftSyncState>().last_serialized = saved;
        reconcile_active_draft(world);
    }

    let serialized = serialize_drafts_from_world(world);
    {
        let mut properties = world.resource_mut::<DocumentProperties>();
        match &serialized {
            Some(value) => {
                if properties.domain_defaults.get(DRAFTS_METADATA_KEY) != Some(value) {
                    properties
                        .domain_defaults
                        .insert(DRAFTS_METADATA_KEY.to_string(), value.clone());
                }
            }
            None => {
                properties.domain_defaults.remove(DRAFTS_METADATA_KEY);
            }
        }
    }
    world.resource_mut::<DraftSyncState>().last_serialized = serialized;
    reconcile_active_draft(world);
}

fn serialize_drafts_from_world(world: &mut World) -> Option<Value> {
    let mut query = world.query::<(&ElementId, &DraftNode)>();
    let mut snapshots = query
        .iter(world)
        .map(|(element_id, node)| DraftSnapshot {
            element_id: *element_id,
            node: node.clone(),
        })
        .collect::<Vec<_>>();
    if snapshots.is_empty() {
        return None;
    }
    snapshots.sort_by_key(|snapshot| snapshot.element_id.0);
    serde_json::to_value(snapshots).ok()
}

fn deserialize_drafts(value: &Value) -> Option<Vec<DraftSnapshot>> {
    let mut snapshots: Vec<DraftSnapshot> = serde_json::from_value(value.clone()).ok()?;
    for snapshot in &mut snapshots {
        snapshot.node.normalize_and_validate().ok()?;
    }
    snapshots.sort_by_key(|snapshot| snapshot.element_id.0);
    Some(snapshots)
}

fn apply_drafts_to_world(world: &mut World, snapshots: &[DraftSnapshot]) {
    let mut query = world.query::<(Entity, &ElementId, &DraftNode)>();
    let mut existing = query
        .iter(world)
        .map(|(entity, element_id, node)| (element_id.0, (entity, node.clone())))
        .collect::<HashMap<_, _>>();
    for snapshot in snapshots {
        if let Some((entity, current)) = existing.remove(&snapshot.element_id.0) {
            if current != snapshot.node {
                world.entity_mut(entity).insert(snapshot.node.clone());
            }
        } else {
            world.spawn((snapshot.element_id, snapshot.node.clone()));
        }
    }
    for (_, (entity, _)) in existing {
        let _ = world.despawn(entity);
    }
}

pub(crate) fn first_draft_id(world: &mut World) -> Option<ElementId> {
    let mut query = world.query_filtered::<&ElementId, With<DraftNode>>();
    query.iter(world).copied().min_by_key(|id| id.0)
}

pub fn active_draft_snapshot(world: &World) -> Option<DraftSnapshot> {
    let id = world
        .get_resource::<DraftingWorkspaceState>()?
        .active_draft_id()?;
    let entity = find_entity_by_element_id_readonly(world, id)?;
    Some(DraftSnapshot {
        element_id: id,
        node: world.get::<DraftNode>(entity)?.clone(),
    })
}

/// Resolve the one Draft that owns an annotation's coordinate context.
///
/// Draft membership is the authority: annotations deliberately do not persist
/// a second copy of the plane. Ambiguous membership is rejected rather than
/// selecting an arbitrary coordinate frame.
pub fn draft_snapshot_for_member(world: &World, member: ElementId) -> Option<DraftSnapshot> {
    let mut query = world.try_query::<(&ElementId, &DraftNode)>()?;
    let mut matches = query
        .iter(world)
        .filter(|(_, node)| node.contains(member))
        .map(|(element_id, node)| DraftSnapshot {
            element_id: *element_id,
            node: node.clone(),
        });
    let resolved = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(resolved)
}

pub fn active_draft_layout(world: &World) -> Option<DraftLayout> {
    active_draft_snapshot(world).map(|snapshot| snapshot.node.layout)
}

fn capture_draft(world: &World, id: ElementId) -> Result<DraftSnapshot, String> {
    let entity = find_entity_by_element_id_readonly(world, id)
        .ok_or_else(|| format!("draft {} does not exist", id.0))?;
    let node = world
        .get::<DraftNode>(entity)
        .ok_or_else(|| format!("element {} is not a Draft", id.0))?;
    Ok(DraftSnapshot {
        element_id: id,
        node: node.clone(),
    })
}

fn reconcile_active_draft(world: &mut World) {
    let selected = world
        .get_resource::<DraftingWorkspaceState>()
        .and_then(DraftingWorkspaceState::active_draft_id);
    if selected.is_some_and(|id| {
        find_entity_by_element_id_readonly(world, id)
            .is_some_and(|entity| world.get::<DraftNode>(entity).is_some())
    }) {
        return;
    }
    let replacement = first_draft_id(world);
    if let Some(mut workspace) = world.get_resource_mut::<DraftingWorkspaceState>() {
        workspace.select_draft(replacement);
    }
}

fn validate_member(world: &World, draft_id: ElementId, member: ElementId) -> Result<(), String> {
    if member == draft_id {
        return Err("a Draft cannot contain itself".to_string());
    }
    let entity = find_entity_by_element_id_readonly(world, member)
        .ok_or_else(|| format!("member {} does not exist", member.0))?;
    if world.get::<DraftNode>(entity).is_some() {
        return Err("Drafts cannot contain other Draft containers".to_string());
    }
    if world
        .get::<super::primitive::DraftPrimitiveNode>(entity)
        .is_some()
    {
        let mut drafts = world
            .try_query::<(&ElementId, &DraftNode)>()
            .ok_or_else(|| "Draft membership query is unavailable".to_string())?;
        if let Some((owner, _)) = drafts
            .iter(world)
            .find(|(owner, node)| **owner != draft_id && node.contains(member))
        {
            return Err(format!(
                "plane-coordinate Draft primitive {} already belongs to Draft {}; move it instead of creating a second plane authority",
                member.0, owner.0
            ));
        }
    }
    let registry = world
        .get_resource::<CapabilityRegistry>()
        .ok_or_else(|| "CapabilityRegistry is unavailable".to_string())?;
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| format!("member {} disappeared", member.0))?;
    registry
        .capture_snapshot(&entity_ref, world)
        .ok_or_else(|| format!("member {} is not authored content", member.0))?;
    Ok(())
}

pub(crate) fn execute_create_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let element_id = world
        .get_resource::<ElementIdAllocator>()
        .ok_or_else(|| "ElementIdAllocator not available".to_string())?
        .next_id();
    let snapshot = draft_snapshot_from_json(parameters, Some(element_id))?;
    for member in &snapshot.node.members {
        validate_member(world, element_id, *member)?;
    }
    enqueue_create_boxed_entity(world, snapshot.clone().into());
    if let Some(mut workspace) = world.get_resource_mut::<DraftingWorkspaceState>() {
        workspace.select_draft(Some(element_id));
    }
    super::workspace::apply_active_draft_plane(world);
    super::workspace::apply_active_draft_camera(world);
    Ok(crate::plugins::command_registry::CommandResult {
        created: vec![element_id.0],
        output: Some(inspect_snapshot(world, &snapshot)),
        ..Default::default()
    })
}

pub(crate) fn execute_select_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let element_id = parameters
        .get("draft_id")
        .and_then(Value::as_u64)
        .map(ElementId)
        .ok_or_else(|| "draft_id is required".to_string())?;
    let snapshot = capture_draft(world, element_id)?;
    if let Some(mut workspace) = world.get_resource_mut::<DraftingWorkspaceState>() {
        workspace.select_draft(Some(element_id));
    } else {
        return Err("Drafting workspace state is unavailable".to_string());
    }
    super::workspace::apply_active_draft_plane(world);
    super::workspace::apply_active_draft_camera(world);
    Ok(crate::plugins::command_registry::CommandResult {
        output: Some(inspect_snapshot(world, &snapshot)),
        ..Default::default()
    })
}

pub(crate) fn execute_update_membership(
    world: &mut World,
    parameters: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let draft_id = parameters
        .get("draft_id")
        .and_then(Value::as_u64)
        .map(ElementId)
        .or_else(|| {
            world
                .get_resource::<DraftingWorkspaceState>()
                .and_then(DraftingWorkspaceState::active_draft_id)
        })
        .ok_or_else(|| "draft_id is required when no Draft is active".to_string())?;
    let mode = parameters
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("add");
    if !matches!(mode, "add" | "remove" | "replace") {
        return Err("mode must be add, remove, or replace".to_string());
    }
    let members = parameters
        .get("member_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| "member_ids must be an array".to_string())?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .map(ElementId)
                .ok_or_else(|| "member_ids must contain integers".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if mode != "remove" {
        for member in &members {
            validate_member(world, draft_id, *member)?;
        }
    }

    let before = capture_draft(world, draft_id)?;
    let mut after = before.clone();
    match mode {
        "add" => after.node.members.extend(members),
        "remove" => after
            .node
            .members
            .retain(|member| !members.contains(member)),
        "replace" => after.node.members = members,
        _ => unreachable!(),
    }
    after.node.normalize_and_validate()?;
    if after == before {
        return Ok(crate::plugins::command_registry::CommandResult {
            output: Some(inspect_snapshot(world, &after)),
            ..Default::default()
        });
    }
    enqueue_apply_entity_changes(
        world,
        ApplyEntityChangesCommand {
            label: "Update Draft membership",
            before: vec![before.into()],
            after: vec![after.clone().into()],
        },
    );
    Ok(crate::plugins::command_registry::CommandResult {
        modified: vec![draft_id.0],
        output: Some(inspect_snapshot(world, &after)),
        ..Default::default()
    })
}

pub(crate) fn execute_inspect_drafts(
    world: &mut World,
    _parameters: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let active = world
        .get_resource::<DraftingWorkspaceState>()
        .and_then(DraftingWorkspaceState::active_draft_id);
    let mut query = world.query::<(&ElementId, &DraftNode)>();
    let mut drafts = query
        .iter(world)
        .map(|(element_id, node)| {
            inspect_snapshot(
                world,
                &DraftSnapshot {
                    element_id: *element_id,
                    node: node.clone(),
                },
            )
        })
        .collect::<Vec<_>>();
    drafts.sort_by_key(|draft| draft["draft_id"].as_u64().unwrap_or(u64::MAX));
    Ok(crate::plugins::command_registry::CommandResult {
        output: Some(json!({
            "active_draft_id": active.map(|id| id.0),
            "drafts": drafts,
            "authority": {
                "durable": "DocumentProperties.domain_defaults.drafts",
                "projection": "DrawingScene (derived)",
                "geometry_ownership": "referenced authored entities"
            }
        })),
        ..Default::default()
    })
}

pub fn inspect_active_draft(world: &World) -> Value {
    active_draft_snapshot(world)
        .map(|snapshot| inspect_snapshot(world, &snapshot))
        .unwrap_or(Value::Null)
}

fn inspect_snapshot(world: &World, snapshot: &DraftSnapshot) -> Value {
    let registry = world.get_resource::<CapabilityRegistry>();
    let members = snapshot
        .node
        .members
        .iter()
        .map(|member| {
            let Some(entity) = find_entity_by_element_id_readonly(world, *member) else {
                return json!({"element_id": member.0, "status": "missing"});
            };
            let Some(registry) = registry else {
                return json!({"element_id": member.0, "status": "registry_unavailable"});
            };
            let Ok(entity_ref) = world.get_entity(entity) else {
                return json!({"element_id": member.0, "status": "missing"});
            };
            match registry.capture_snapshot(&entity_ref, world) {
                Some(member_snapshot) => json!({
                    "element_id": member.0,
                    "status": "resolved",
                    "type_name": member_snapshot.type_name(),
                    "scope": match member_snapshot.scope() {
                        EntityScope::AuthoredModel => "authored_model_3d",
                        EntityScope::DrawingMetadata => "drawing_annotation_2d",
                    }
                }),
                None => json!({"element_id": member.0, "status": "not_authored"}),
            }
        })
        .collect::<Vec<_>>();
    let stale_count = members
        .iter()
        .filter(|member| member["status"] != "resolved")
        .count();
    json!({
        "draft_id": snapshot.element_id.0,
        "name": snapshot.node.name,
        "plane": snapshot.node.plane,
        "layout": snapshot.node.layout,
        "defaults": snapshot.node.defaults,
        "members": members,
        "member_count": snapshot.node.members.len(),
        "stale_member_count": stale_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        history::{HistoryPlugin, PendingCommandQueue},
        modeling::{
            generic_factory::PrimitiveFactory,
            group::{GroupEditContext, GroupFrame, GroupMembers},
            primitives::{BoxPrimitive, ShapeRotation},
        },
    };

    fn command_app() -> App {
        let mut app = App::new();
        app.add_plugins(HistoryPlugin)
            .insert_resource(ElementIdAllocator::default())
            .insert_resource(CapabilityRegistry::default())
            .insert_resource(DocumentProperties::default())
            .init_resource::<DraftSyncState>()
            .init_resource::<DraftingWorkspaceState>()
            .init_resource::<DrawingPlane>();
        app
    }

    #[test]
    fn plane_constructor_rejects_parallel_axes_and_canonicalizes_basis() {
        assert!(DraftPlane::try_from_origin_normal_tangent(Vec3::ZERO, Vec3::Y, Vec3::Y).is_err());
        let plane = DraftPlane::try_from_origin_normal_tangent(
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::Y * 5.0,
            Vec3::new(2.0, 0.25, 0.0),
        )
        .expect("valid plane");
        plane.validate().expect("orthonormal plane");
        assert_eq!(plane.tangent, Vec3::X);
        assert_eq!(plane.bitangent, Vec3::Z);
    }

    #[test]
    fn node_normalization_sorts_and_deduplicates_references() {
        let mut node = DraftNode::new(" Plan ");
        node.members = vec![ElementId(9), ElementId(2), ElementId(9)];
        node.normalize_and_validate().expect("valid node");
        assert_eq!(node.name, "Plan");
        assert_eq!(node.members, vec![ElementId(2), ElementId(9)]);
    }

    #[test]
    fn snapshot_json_roundtrip_preserves_references_without_geometry() {
        let mut node = DraftNode::new("Storey");
        node.members = vec![ElementId(7), ElementId(11)];
        let snapshot = DraftSnapshot {
            element_id: ElementId(42),
            node,
        };
        let roundtrip = draft_snapshot_from_json(&snapshot.to_json(), None).expect("roundtrip");
        assert_eq!(roundtrip, snapshot);
        let json = snapshot.to_json();
        assert!(json.get("geometry").is_none());
        assert_eq!(json["members"], json!([7, 11]));
    }

    #[test]
    fn create_and_undo_flow_through_history_and_metadata_persistence() {
        let mut app = command_app();
        let result = execute_create_draft(
            app.world_mut(),
            &json!({
                "name": "Ground floor",
                "scale_denominator": 100.0,
                "paper_width_mm": 420.0,
                "paper_height_mm": 297.0,
                "margin_mm": 12.0
            }),
        )
        .expect("create command");
        assert_eq!(result.created, vec![0]);
        assert_eq!(
            app.world().resource::<PendingCommandQueue>().commands.len(),
            1
        );

        app.update();
        sync_drafts(app.world_mut());
        let entity = find_entity_by_element_id_readonly(app.world(), ElementId(0))
            .expect("history applied Draft");
        assert_eq!(
            app.world()
                .get::<DraftNode>(entity)
                .unwrap()
                .layout
                .scale_denominator,
            100.0
        );
        assert!(app
            .world()
            .resource::<DocumentProperties>()
            .domain_defaults
            .contains_key(DRAFTS_METADATA_KEY));

        app.world_mut()
            .resource_mut::<PendingCommandQueue>()
            .queue_undo();
        app.update();
        sync_drafts(app.world_mut());
        assert!(find_entity_by_element_id_readonly(app.world(), ElementId(0)).is_none());
        assert!(!app
            .world()
            .resource::<DocumentProperties>()
            .domain_defaults
            .contains_key(DRAFTS_METADATA_KEY));
    }

    #[test]
    fn metadata_reload_restores_draft_and_active_selection() {
        let mut source = command_app();
        execute_create_draft(
            source.world_mut(),
            &json!({
                "name": "Reloaded plan",
                "origin": [1.0, 2.0, 3.0],
                "normal": [0.0, 0.0, 1.0],
                "tangent": [1.0, 0.0, 0.0]
            }),
        )
        .expect("create command");
        source.update();
        sync_drafts(source.world_mut());
        let persisted = source
            .world()
            .resource::<DocumentProperties>()
            .domain_defaults
            .get(DRAFTS_METADATA_KEY)
            .cloned()
            .expect("Draft metadata persisted");

        let mut target = command_app();
        target
            .world_mut()
            .resource_mut::<DocumentProperties>()
            .domain_defaults
            .insert(DRAFTS_METADATA_KEY.to_string(), persisted);
        sync_drafts(target.world_mut());

        let restored = find_entity_by_element_id_readonly(target.world(), ElementId(0))
            .expect("Draft restored from document metadata");
        let node = target.world().get::<DraftNode>(restored).unwrap();
        assert_eq!(node.name, "Reloaded plan");
        assert_eq!(node.plane.origin, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(
            target
                .world()
                .resource::<DraftingWorkspaceState>()
                .active_draft_id(),
            Some(ElementId(0))
        );
    }

    #[test]
    fn draft_creation_is_not_transformed_or_owned_by_active_model_group() {
        let mut app = command_app();
        let group_id = ElementId(90);
        app.world_mut().spawn((
            group_id,
            GroupMembers {
                name: "Rotated model group".to_string(),
                member_ids: Vec::new(),
                frame: GroupFrame {
                    translation: Vec3::new(10.0, 20.0, 30.0),
                    rotation: Quat::from_rotation_y(0.75),
                },
                linked_model: None,
            },
        ));
        let mut edit_context = GroupEditContext::default();
        edit_context.enter(group_id);
        app.world_mut().insert_resource(edit_context);

        execute_create_draft(
            app.world_mut(),
            &json!({
                "name": "World drawing plane",
                "origin": [1.0, 2.0, 3.0],
                "normal": [0.0, 0.0, 1.0],
                "tangent": [1.0, 0.0, 0.0]
            }),
        )
        .expect("create command");
        app.update();

        let draft_entity =
            find_entity_by_element_id_readonly(app.world(), ElementId(0)).expect("Draft created");
        assert_eq!(
            app.world()
                .get::<DraftNode>(draft_entity)
                .unwrap()
                .plane
                .origin,
            Vec3::new(1.0, 2.0, 3.0),
            "drawing metadata must remain in its own plane frame"
        );
        let group_entity = find_entity_by_element_id_readonly(app.world(), group_id).unwrap();
        assert!(
            app.world()
                .get::<GroupMembers>(group_entity)
                .unwrap()
                .member_ids
                .is_empty(),
            "drawing metadata must never become model-group geometry"
        );
    }

    #[test]
    fn membership_references_model_entities_and_rejects_draft_cycles() {
        let mut app = command_app();
        {
            let mut registry = app.world_mut().resource_mut::<CapabilityRegistry>();
            registry.register_factory(DraftFactory);
            registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
            registry.register_factory(super::super::primitive::DraftLineFactory);
        }
        let mut draft = DraftNode::new("Mixed");
        draft.normalize_and_validate().unwrap();
        app.world_mut().spawn((ElementId(10), draft));
        app.world_mut().spawn((
            ElementId(20),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::splat(0.5),
            },
            ShapeRotation::default(),
        ));
        app.world_mut()
            .spawn((ElementId(30), DraftNode::new("Other")));
        app.world_mut().spawn((
            ElementId(40),
            super::super::primitive::DraftPrimitiveNode {
                geometry: super::super::primitive::DraftPrimitiveGeometry::Line(
                    super::super::primitive::DraftLine {
                        a: Vec2::ZERO,
                        b: Vec2::X,
                    },
                ),
                layer: "Default".into(),
                style_name: "architectural_metric".into(),
                visible: true,
            },
        ));
        app.world_mut()
            .resource_mut::<DraftingWorkspaceState>()
            .select_draft(Some(ElementId(10)));

        let result =
            execute_update_membership(app.world_mut(), &json!({"mode": "add", "member_ids": [20]}))
                .expect("reference authored model member");
        assert_eq!(result.modified, vec![10]);
        app.update();
        let draft_entity = find_entity_by_element_id_readonly(app.world(), ElementId(10)).unwrap();
        assert_eq!(
            app.world().get::<DraftNode>(draft_entity).unwrap().members,
            vec![ElementId(20)]
        );

        let inspected = inspect_active_draft(app.world());
        assert_eq!(inspected["members"][0]["scope"], "authored_model_3d");
        assert!(execute_update_membership(
            app.world_mut(),
            &json!({"mode": "add", "member_ids": [30]})
        )
        .is_err());
        assert!(execute_update_membership(
            app.world_mut(),
            &json!({"mode": "add", "member_ids": [10]})
        )
        .is_err());

        execute_update_membership(app.world_mut(), &json!({"mode": "add", "member_ids": [40]}))
            .expect("first Draft owns the plane-coordinate primitive");
        app.update();
        app.world_mut()
            .resource_mut::<DraftingWorkspaceState>()
            .select_draft(Some(ElementId(30)));
        assert!(execute_update_membership(
            app.world_mut(),
            &json!({"mode": "add", "member_ids": [40]})
        )
        .is_err());
    }
}
