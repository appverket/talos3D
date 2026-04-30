use std::{any::Any, fmt};

use bevy::prelude::*;
use serde_json::Value;

use crate::plugins::{identity::ElementId, materials::MaterialAssignment};

// --- Shared helpers for AuthoredEntity implementations ---

pub fn property_field(
    name: &'static str,
    kind: PropertyValueKind,
    value: Option<PropertyValue>,
) -> PropertyFieldDef {
    property_field_with(name, name, kind, value, true)
}

pub fn property_field_with(
    name: &'static str,
    label: &'static str,
    kind: PropertyValueKind,
    value: Option<PropertyValue>,
    editable: bool,
) -> PropertyFieldDef {
    PropertyFieldDef {
        name,
        label,
        kind,
        value,
        editable,
    }
}

pub fn read_only_property_field(
    name: &'static str,
    label: &'static str,
    kind: PropertyValueKind,
    value: Option<PropertyValue>,
) -> PropertyFieldDef {
    property_field_with(name, label, kind, value, false)
}

pub fn invalid_property_error(entity_type: &str, valid_properties: &[&str]) -> String {
    format!(
        "Invalid property name for {entity_type}. Valid properties: {}",
        valid_properties.join(", ")
    )
}

pub fn scalar_from_json(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|v| v as f32)
        .ok_or_else(|| "Expected numeric value".to_string())
}

pub fn vec2_from_json(value: &Value) -> Result<Vec2, String> {
    if let Some(array) = value.as_array() {
        if array.len() == 2 {
            return Ok(Vec2::new(
                scalar_from_json(&array[0])?,
                scalar_from_json(&array[1])?,
            ));
        }
    }

    if let Some(object) = value.as_object() {
        return Ok(Vec2::new(
            object
                .get("x")
                .map(scalar_from_json)
                .transpose()?
                .ok_or_else(|| "Missing numeric field 'x'".to_string())?,
            object
                .get("y")
                .map(scalar_from_json)
                .transpose()?
                .ok_or_else(|| "Missing numeric field 'y'".to_string())?,
        ));
    }

    Err("Expected a Vec2 as [x, y] or {\"x\": ..., \"y\": ...}".to_string())
}

pub fn vec3_from_json(value: &Value) -> Result<Vec3, String> {
    if let Some(array) = value.as_array() {
        if array.len() == 3 {
            return Ok(Vec3::new(
                scalar_from_json(&array[0])?,
                scalar_from_json(&array[1])?,
                scalar_from_json(&array[2])?,
            ));
        }
    }

    if let Some(object) = value.as_object() {
        return Ok(Vec3::new(
            object
                .get("x")
                .map(scalar_from_json)
                .transpose()?
                .ok_or_else(|| "Missing numeric field 'x'".to_string())?,
            object
                .get("y")
                .map(scalar_from_json)
                .transpose()?
                .ok_or_else(|| "Missing numeric field 'y'".to_string())?,
            object
                .get("z")
                .map(scalar_from_json)
                .transpose()?
                .ok_or_else(|| "Missing numeric field 'z'".to_string())?,
        ));
    }

    Err("Expected a Vec3 as [x, y, z] or {\"x\": ..., \"y\": ..., \"z\": ...}".to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValueKind {
    Scalar,
    Vec2,
    Vec3,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Scalar(f32),
    Vec2(Vec2),
    Vec3(Vec3),
    Text(String),
}

impl PropertyValue {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Scalar(value) => Value::from(*value),
            Self::Vec2(value) => Value::Array(vec![Value::from(value.x), Value::from(value.y)]),
            Self::Vec3(value) => Value::Array(vec![
                Value::from(value.x),
                Value::from(value.y),
                Value::from(value.z),
            ]),
            Self::Text(value) => Value::String(value.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyFieldDef {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: PropertyValueKind,
    pub value: Option<PropertyValue>,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HandleKind {
    Vertex,
    Center,
    Control,
    Parameter,
}

impl HandleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vertex => "Vertex",
            Self::Center => "Center",
            Self::Control => "Control",
            Self::Parameter => "Parameter",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HandleInfo {
    pub id: String,
    pub position: Vec3,
    pub kind: HandleKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityBounds {
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPullAffordance {
    Allowed,
    Blocked(PushPullBlockReason),
}

impl PushPullAffordance {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    pub fn status_hint(self) -> &'static str {
        match self {
            Self::Allowed => "G: push/pull",
            Self::Blocked(PushPullBlockReason::CapOnly) => "cap-only push/pull",
            Self::Blocked(PushPullBlockReason::PreserveSolidIntent) => {
                "solid-constrained push/pull"
            }
            Self::Blocked(PushPullBlockReason::UnsupportedFace) => "push/pull unavailable",
        }
    }

    pub fn blocked_feedback(self) -> &'static str {
        match self {
            Self::Allowed => "Push/pull is available for this face",
            Self::Blocked(PushPullBlockReason::CapOnly) => {
                "This solid preserves its semantics by allowing push/pull on caps only"
            }
            Self::Blocked(PushPullBlockReason::PreserveSolidIntent) => {
                "This face is constrained by the authored solid semantics and attached features"
            }
            Self::Blocked(PushPullBlockReason::UnsupportedFace) => {
                "Push/pull is unavailable for this face"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPullBlockReason {
    UnsupportedFace,
    CapOnly,
    PreserveSolidIntent,
}

impl EntityBounds {
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn corners(&self) -> [Vec3; 8] {
        let min = self.min;
        let max = self.max;
        [
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(min.x, max.y, max.z),
            Vec3::new(max.x, max.y, max.z),
            Vec3::new(max.x, max.y, min.z),
        ]
    }

    pub fn face_centers(&self) -> [(Vec3, Vec3); 6] {
        let center = self.center();
        [
            (Vec3::new(self.max.x, center.y, center.z), Vec3::X),
            (Vec3::new(self.min.x, center.y, center.z), -Vec3::X),
            (Vec3::new(center.x, self.max.y, center.z), Vec3::Y),
            (Vec3::new(center.x, self.min.y, center.z), -Vec3::Y),
            (Vec3::new(center.x, center.y, self.max.z), Vec3::Z),
            (Vec3::new(center.x, center.y, self.min.z), -Vec3::Z),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityScope {
    AuthoredModel,
    DrawingMetadata,
}

pub trait AuthoredEntity: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
    fn type_name(&self) -> &'static str;
    fn element_id(&self) -> ElementId;
    fn label(&self) -> String;
    fn center(&self) -> Vec3;
    fn scope(&self) -> EntityScope {
        EntityScope::AuthoredModel
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity;
    fn rotate_by(&self, rotation: Quat) -> BoxedEntity;
    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity;

    /// Push/pull a face by the given signed distance along the face normal.
    /// Returns None if this entity type doesn't support push/pull for the given face.
    fn push_pull(
        &self,
        _face_id: crate::capability_registry::FaceId,
        _distance: f32,
    ) -> Option<BoxedEntity> {
        None
    }

    /// Whether push/pull is semantically valid for the given face.
    /// Interaction code uses this to reflect authored constraints directly.
    fn push_pull_affordance(
        &self,
        _face_id: crate::capability_registry::FaceId,
    ) -> PushPullAffordance {
        PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace)
    }

    fn transform_parent(&self) -> Option<ElementId> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef>;
    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String>;
    fn material_assignment(&self) -> Option<MaterialAssignment> {
        None
    }
    fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<BoxedEntity, String> {
        let _ = assignment;
        Err(format!(
            "{} snapshots do not support material assignment",
            self.type_name()
        ))
    }

    fn handles(&self) -> Vec<HandleInfo>;
    fn bounds(&self) -> Option<EntityBounds> {
        None
    }
    fn to_json(&self) -> Value;
    fn to_persisted_json(&self) -> Value {
        self.to_json()
    }

    fn apply_to(&self, world: &mut World);
    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }
    fn remove_from(&self, world: &mut World);
    fn preview_transform(&self) -> Option<Transform> {
        None
    }
    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        None
    }
    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color);
    fn preview_line_count(&self) -> usize {
        0
    }
    fn sync_preview_entity(&self, _world: &mut World, _existing: Option<Entity>) -> Option<Entity> {
        None
    }
    fn cleanup_preview_entity(&self, world: &mut World, preview_entity: Entity) {
        let _ = world.despawn(preview_entity);
    }

    fn box_clone(&self) -> BoxedEntity;
    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool;
}

pub struct BoxedEntity(pub Box<dyn AuthoredEntity>);

impl Clone for BoxedEntity {
    fn clone(&self) -> Self {
        self.0.box_clone()
    }
}

impl PartialEq for BoxedEntity {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_snapshot(other.0.as_ref())
    }
}

impl fmt::Debug for BoxedEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoxedEntity")
            .field("type_name", &self.type_name())
            .field("element_id", &self.element_id())
            .finish()
    }
}

impl BoxedEntity {
    pub fn type_name(&self) -> &'static str {
        self.0.type_name()
    }

    pub fn element_id(&self) -> ElementId {
        self.0.element_id()
    }

    pub fn label(&self) -> String {
        self.0.label()
    }

    pub fn center(&self) -> Vec3 {
        self.0.center()
    }

    pub fn scope(&self) -> EntityScope {
        self.0.scope()
    }

    pub fn translate_by(&self, delta: Vec3) -> Self {
        self.0.translate_by(delta)
    }

    pub fn rotate_by(&self, rotation: Quat) -> Self {
        self.0.rotate_by(rotation)
    }

    pub fn scale_by(&self, factor: Vec3, center: Vec3) -> Self {
        self.0.scale_by(factor, center)
    }

    pub fn push_pull(
        &self,
        face_id: crate::capability_registry::FaceId,
        distance: f32,
    ) -> Option<Self> {
        self.0.push_pull(face_id, distance)
    }

    pub fn push_pull_affordance(
        &self,
        face_id: crate::capability_registry::FaceId,
    ) -> PushPullAffordance {
        self.0.push_pull_affordance(face_id)
    }

    pub fn transform_parent(&self) -> Option<ElementId> {
        self.0.transform_parent()
    }

    pub fn property_fields(&self) -> Vec<PropertyFieldDef> {
        self.0.property_fields()
    }

    pub fn set_property_json(&self, property_name: &str, value: &Value) -> Result<Self, String> {
        self.0.set_property_json(property_name, value)
    }

    pub fn material_assignment(&self) -> Option<MaterialAssignment> {
        self.0.material_assignment()
    }

    pub fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<Self, String> {
        self.0.set_material_assignment(assignment)
    }

    pub fn handles(&self) -> Vec<HandleInfo> {
        self.0.handles()
    }

    pub fn bounds(&self) -> Option<EntityBounds> {
        self.0.bounds()
    }

    pub fn to_json(&self) -> Value {
        self.0.to_json()
    }

    pub fn to_persisted_json(&self) -> Value {
        self.0.to_persisted_json()
    }

    pub fn apply_to(&self, world: &mut World) {
        self.0.apply_to(world);
    }

    pub fn apply_with_previous(&self, world: &mut World, previous: Option<&Self>) {
        self.0
            .apply_with_previous(world, previous.map(|snapshot| snapshot.0.as_ref()));
    }

    pub fn remove_from(&self, world: &mut World) {
        self.0.remove_from(world);
    }

    pub fn preview_transform(&self) -> Option<Transform> {
        self.0.preview_transform()
    }

    pub fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<Self> {
        self.0.drag_handle(handle_id, cursor)
    }

    pub fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        self.0.draw_preview(gizmos, color);
    }

    pub fn preview_line_count(&self) -> usize {
        self.0.preview_line_count()
    }

    pub fn sync_preview_entity(
        &self,
        world: &mut World,
        existing: Option<Entity>,
    ) -> Option<Entity> {
        self.0.sync_preview_entity(world, existing)
    }

    pub fn cleanup_preview_entity(&self, world: &mut World, preview_entity: Entity) {
        self.0.cleanup_preview_entity(world, preview_entity);
    }
}
