//! Clipping plane entities for architectural section views.
//!
//! A [`ClipPlaneNode`] is an authored entity that defines a half-space in world
//! space.  Each frame the [`ClippingPlanesPlugin`] hides any renderable mesh
//! whose Transform origin lies on the *negative-normal* side of every active
//! plane.  Geometry that straddles a plane is left visible (approximate but
//! correct for whole-room architectural cuts).

use std::any::Any;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, AuthoredEntity, BoxedEntity, EntityBounds,
        HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, FaceId, HitCandidate},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        modeling::csg::CsgOperand,
    },
};

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/// An authored clipping plane that cuts the viewport.
///
/// Geometry on the *negative-normal* side of the plane (i.e. where
/// `dot(pos - origin, normal) < 0`) is hidden while the plane is active.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipPlaneNode {
    /// Display name, e.g. `"Section A-A"`.
    pub name: String,
    /// A point on the plane in world space.
    pub origin: Vec3,
    /// Normal pointing toward the **visible** side.
    pub normal: Vec3,
    /// Whether this plane is currently cutting the view.
    pub active: bool,
}

impl ClipPlaneNode {
    /// Create a horizontal section cut at `height` on the Y axis.
    ///
    /// The normal points upward so geometry *above* the cut is visible.
    pub fn horizontal(name: impl Into<String>, height: f32) -> Self {
        Self {
            name: name.into(),
            origin: Vec3::new(0.0, height, 0.0),
            normal: Vec3::Y,
            active: true,
        }
    }

    /// Shorthand — horizontal section cut named `"Section"`.
    pub fn at_y(height: f32) -> Self {
        Self::horizontal("Section", height)
    }
}

/// Marker: this mesh entity is currently hidden because it lies on the clipped
/// side of at least one active [`ClipPlaneNode`].
#[derive(Component, Debug, Clone)]
pub struct ClippedByPlane;

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin that drives clip-plane visibility culling each frame.
pub struct ClippingPlanesPlugin;

impl Plugin for ClippingPlanesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (apply_clip_plane_visibility, draw_clip_plane_gizmos),
        );
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Each frame: hide or show renderable mesh entities based on active clip planes.
///
/// Entities that are [`CsgOperand`] (already hidden by the CSG pipeline) are
/// skipped so we never fight with CSG visibility.
fn apply_clip_plane_visibility(
    clip_planes: Query<&ClipPlaneNode>,
    renderables: Query<
        (Entity, &Transform),
        (With<Mesh3d>, Without<ClipPlaneNode>, Without<CsgOperand>),
    >,
    mut visibility: Query<&mut Visibility>,
) {
    let active_planes: Vec<&ClipPlaneNode> = clip_planes.iter().filter(|p| p.active).collect();

    for (entity, transform) in &renderables {
        let pos = transform.translation;
        let clipped = active_planes
            .iter()
            .any(|plane| (pos - plane.origin).dot(plane.normal) < 0.0);

        if let Ok(mut vis) = visibility.get_mut(entity) {
            // Only overwrite Visible/Hidden; never un-hide a CsgOperand-hidden
            // entity (those use Visibility::Hidden for other reasons but are
            // excluded by the query above anyway).
            *vis = if clipped {
                Visibility::Hidden
            } else {
                Visibility::Visible
            };
        }
    }
}

/// Draw a square outline and normal arrow for every active clip plane.
fn draw_clip_plane_gizmos(clip_planes: Query<&ClipPlaneNode>, mut gizmos: Gizmos) {
    let color = Color::srgba(0.2, 0.6, 1.0, 0.8);
    for plane in clip_planes.iter().filter(|p| p.active) {
        draw_plane_gizmo(plane, &mut gizmos, color);
    }
}

// ---------------------------------------------------------------------------
// ClipPlaneSnapshot — AuthoredEntity
// ---------------------------------------------------------------------------

/// Serialisable snapshot of a [`ClipPlaneNode`] entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPlaneSnapshot {
    pub element_id: ElementId,
    pub node: ClipPlaneNode,
}

impl AuthoredEntity for ClipPlaneSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "clip_plane"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.node.name.clone()
    }

    fn center(&self) -> Vec3 {
        self.node.origin
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snap = self.clone();
        snap.node.origin += delta;
        snap.into()
    }

    fn rotate_by(&self, rotation: bevy::math::Quat) -> BoxedEntity {
        let mut snap = self.clone();
        snap.node.origin = rotation * snap.node.origin;
        snap.node.normal = (rotation * snap.node.normal).normalize();
        snap.into()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        // Planes are infinite — scaling the normal makes no physical sense.
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
                false,
            ),
            property_field_with(
                "origin_x",
                "Origin X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.origin.x)),
                false,
            ),
            property_field_with(
                "origin_y",
                "Origin Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.origin.y)),
                false,
            ),
            property_field_with(
                "origin_z",
                "Origin Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.origin.z)),
                false,
            ),
            property_field_with(
                "normal_x",
                "Normal X",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.normal.x)),
                false,
            ),
            property_field_with(
                "normal_y",
                "Normal Y",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.normal.y)),
                false,
            ),
            property_field_with(
                "normal_z",
                "Normal Z",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.normal.z)),
                false,
            ),
            property_field_with(
                "active",
                "Active",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(
                    if self.node.active { "true" } else { "false" }.to_string(),
                )),
                false,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snap = self.clone();
        match property_name {
            "name" => {
                snap.node.name = value
                    .as_str()
                    .ok_or_else(|| "name must be a string".to_string())?
                    .to_string();
            }
            "origin_x" => {
                snap.node.origin.x = value
                    .as_f64()
                    .ok_or_else(|| "origin_x must be a number".to_string())?
                    as f32;
            }
            "origin_y" => {
                snap.node.origin.y = value
                    .as_f64()
                    .ok_or_else(|| "origin_y must be a number".to_string())?
                    as f32;
            }
            "origin_z" => {
                snap.node.origin.z = value
                    .as_f64()
                    .ok_or_else(|| "origin_z must be a number".to_string())?
                    as f32;
            }
            "normal_x" => {
                snap.node.normal.x = value
                    .as_f64()
                    .ok_or_else(|| "normal_x must be a number".to_string())?
                    as f32;
            }
            "normal_y" => {
                snap.node.normal.y = value
                    .as_f64()
                    .ok_or_else(|| "normal_y must be a number".to_string())?
                    as f32;
            }
            "normal_z" => {
                snap.node.normal.z = value
                    .as_f64()
                    .ok_or_else(|| "normal_z must be a number".to_string())?
                    as f32;
            }
            "active" => {
                let s = value
                    .as_str()
                    .ok_or_else(|| "active must be \"true\" or \"false\"".to_string())?;
                snap.node.active = match s {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    _ => return Err(format!("active must be \"true\" or \"false\", got \"{s}\"")),
                };
            }
            _ => {
                return Err(invalid_property_error(
                    "clip_plane",
                    &[
                        "name", "origin_x", "origin_y", "origin_z", "normal_x", "normal_y",
                        "normal_z", "active",
                    ],
                ));
            }
        }
        Ok(snap.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        // Planes are infinite — no meaningful bounding box.
        None
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        None
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(&self.node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world
                .entity_mut(entity)
                .insert((self.node.clone(), Visibility::Visible));
        } else {
            world.spawn((self.element_id, self.node.clone(), Visibility::Visible));
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        None
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn preview_line_count(&self) -> usize {
        0
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == "clip_plane" && other.to_json() == self.to_json()
    }
}

impl From<ClipPlaneSnapshot> for BoxedEntity {
    fn from(snap: ClipPlaneSnapshot) -> Self {
        Self(Box::new(snap))
    }
}

// ---------------------------------------------------------------------------
// ClipPlaneFactory — AuthoredEntityFactory
// ---------------------------------------------------------------------------

pub struct ClipPlaneFactory;

impl AuthoredEntityFactory for ClipPlaneFactory {
    fn type_name(&self) -> &'static str {
        "clip_plane"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<ClipPlaneNode>()?;
        Some(
            ClipPlaneSnapshot {
                element_id,
                node: node.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(|v| v.as_u64())
                .ok_or("Missing or invalid element_id")?,
        );
        let node: ClipPlaneNode =
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        Ok(ClipPlaneSnapshot { element_id, node }.into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();

        let name = request
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Section")
            .to_string();

        let active = request
            .get("active")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let origin = parse_vec3_field(request, "origin").unwrap_or(Vec3::ZERO);
        let normal = parse_normal_field(request).unwrap_or(Vec3::Y);

        Ok(ClipPlaneSnapshot {
            element_id,
            node: ClipPlaneNode {
                name,
                origin,
                normal,
                active,
            },
        }
        .into())
    }

    fn draw_selection(
        &self,
        world: &World,
        entity: bevy::ecs::entity::Entity,
        gizmos: &mut Gizmos,
        color: Color,
    ) {
        let Some(node) = world.get::<ClipPlaneNode>(entity) else {
            return;
        };
        draw_plane_gizmo(node, gizmos, color);
    }

    fn hit_test(&self, _world: &World, _ray: bevy::math::Ray3d) -> Option<HitCandidate> {
        // Infinite planes are not directly clickable via ray-cast in MVP.
        None
    }
}

// ---------------------------------------------------------------------------
// Gizmo drawing helper
// ---------------------------------------------------------------------------

/// Draw the plane outline + normal arrow at a given node state.
fn draw_plane_gizmo(node: &ClipPlaneNode, gizmos: &mut Gizmos, color: Color) {
    let tangent = if node.normal.abs().dot(Vec3::X) < 0.9 {
        node.normal.cross(Vec3::X).normalize()
    } else {
        node.normal.cross(Vec3::Z).normalize()
    };
    let bitangent = node.normal.cross(tangent);
    let o = node.origin;
    let half = 5.0_f32;

    let corners = [
        o + tangent * half + bitangent * half,
        o - tangent * half + bitangent * half,
        o - tangent * half - bitangent * half,
        o + tangent * half - bitangent * half,
    ];
    gizmos.line(corners[0], corners[1], color);
    gizmos.line(corners[1], corners[2], color);
    gizmos.line(corners[2], corners[3], color);
    gizmos.line(corners[3], corners[0], color);
    gizmos.arrow(o, o + node.normal * 2.0, color);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse `[f32; 3]` from a JSON field named `key`.
fn parse_vec3_field(request: &Value, key: &str) -> Option<Vec3> {
    let arr = request.get(key)?.as_array()?;
    if arr.len() >= 3 {
        let x = arr[0].as_f64()? as f32;
        let y = arr[1].as_f64()? as f32;
        let z = arr[2].as_f64()? as f32;
        Some(Vec3::new(x, y, z))
    } else {
        None
    }
}

/// Parse the `normal` field from a create request.
///
/// Accepts either an `[x, y, z]` array **or** one of the shorthand strings
/// `"x"`, `"-x"`, `"y"`, `"-y"`, `"z"`, `"-z"`.
fn parse_normal_field(request: &Value) -> Option<Vec3> {
    let field = request.get("normal")?;

    if let Some(s) = field.as_str() {
        return match s.to_lowercase().as_str() {
            "x" | "+x" => Some(Vec3::X),
            "-x" => Some(Vec3::NEG_X),
            "y" | "+y" => Some(Vec3::Y),
            "-y" => Some(Vec3::NEG_Y),
            "z" | "+z" => Some(Vec3::Z),
            "-z" => Some(Vec3::NEG_Z),
            _ => None,
        };
    }

    parse_vec3_field(request, "normal")
}
