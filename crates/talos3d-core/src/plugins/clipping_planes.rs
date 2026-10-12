//! Clipping plane entities for architectural section views.
//!
//! A [`ClipPlaneNode`] is an authored entity that defines a half-space in world
//! space. The [`ClippingPlanesPlugin`] hides any renderable mesh whose
//! world-space bounding box lies fully on the hidden side of any active plane.
//! It recomputes all renderables only when the plane set changes and otherwise
//! visits only changed transforms/bounds/meshes. Crossing geometry remains
//! available to the canonical DrawingScene section projector.

use std::{any::Any, collections::HashMap};

use bevy::{camera::primitives::Aabb, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, AuthoredEntity, BoxedEntity, EntityBounds,
        EntityScope, HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, FaceId, HitCandidate},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        document_properties::DocumentProperties,
        identity::ElementId,
        modeling::csg::CsgOperand,
    },
};

pub const SECTION_VIEW_METADATA_KEY: &str = "section_views";

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
pub struct ClippedByPlane {
    previous: Visibility,
}

#[derive(Resource, Default)]
struct SectionViewSyncState {
    last_serialized: Option<Value>,
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin that drives invalidation-bounded clip-plane visibility culling.
pub struct ClippingPlanesPlugin;

impl Plugin for ClippingPlanesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SectionViewSyncState>().add_systems(
            Update,
            (
                sync_section_views,
                apply_clip_plane_visibility,
                draw_clip_plane_gizmos,
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Hide or show renderable mesh entities based on active clip planes.
///
/// Entities that are [`CsgOperand`] (already hidden by the CSG pipeline) are
/// skipped so we never fight with CSG visibility.
type ClipRenderable<'a> = (Entity, &'a GlobalTransform, Option<&'a Aabb>);

fn apply_clip_plane_visibility(
    mut commands: Commands,
    clip_planes: Query<Ref<ClipPlaneNode>>,
    mut renderables: ParamSet<(
        Query<ClipRenderable, (With<Mesh3d>, Without<ClipPlaneNode>, Without<CsgOperand>)>,
        Query<
            ClipRenderable,
            (
                With<Mesh3d>,
                Without<ClipPlaneNode>,
                Without<CsgOperand>,
                Or<(Changed<GlobalTransform>, Changed<Aabb>, Changed<Mesh3d>)>,
            ),
        >,
    )>,
    mut removed_clip_planes: RemovedComponents<ClipPlaneNode>,
    mut visibility: Query<(&mut Visibility, Option<&ClippedByPlane>)>,
) {
    let plane_set_changed = clip_planes.iter().any(|plane| plane.is_changed())
        || removed_clip_planes.read().count() > 0;
    let active_planes: Vec<ClipPlaneNode> = clip_planes
        .iter()
        .filter(|p| p.active)
        .map(|p| (*p).clone())
        .collect();
    if plane_set_changed {
        for (entity, transform, bounds) in renderables.p0().iter() {
            apply_visibility_for_entity(
                entity,
                transform,
                bounds,
                &active_planes,
                &mut visibility,
                &mut commands,
            );
        }
    } else if !active_planes.is_empty() {
        for (entity, transform, bounds) in renderables.p1().iter() {
            apply_visibility_for_entity(
                entity,
                transform,
                bounds,
                &active_planes,
                &mut visibility,
                &mut commands,
            );
        }
    }
}

fn apply_visibility_for_entity(
    entity: Entity,
    transform: &GlobalTransform,
    bounds: Option<&Aabb>,
    active_planes: &[ClipPlaneNode],
    visibility: &mut Query<(&mut Visibility, Option<&ClippedByPlane>)>,
    commands: &mut Commands,
) {
    // No bounds yet: defer instead of guessing from the object's local origin.
    let Some(bounds) = bounds else {
        return;
    };
    let affine = transform.affine();
    let centre: Vec3 = affine.transform_point3a(bounds.center).into();
    let half = bounds.half_extents;
    let clipped = active_planes.iter().any(|plane| {
        let normal = bevy::math::Vec3A::from(plane.normal);
        let radius = normal.dot(affine.matrix3.x_axis).abs() * half.x
            + normal.dot(affine.matrix3.y_axis).abs() * half.y
            + normal.dot(affine.matrix3.z_axis).abs() * half.z;
        (centre - plane.origin).dot(plane.normal) + radius < -1e-5
    });
    if let Ok((mut current, previous)) = visibility.get_mut(entity) {
        if clipped {
            if previous.is_none() {
                commands
                    .entity(entity)
                    .insert(ClippedByPlane { previous: *current });
            }
            if *current != Visibility::Hidden {
                *current = Visibility::Hidden;
            }
        } else if let Some(previous) = previous {
            if *current != previous.previous {
                *current = previous.previous;
            }
            commands.entity(entity).remove::<ClippedByPlane>();
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    fn scope(&self) -> EntityScope {
        EntityScope::DrawingMetadata
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
        serde_json::json!({
            "element_id": self.element_id,
            "name": self.node.name.clone(),
            "origin": self.node.origin.to_array(),
            "normal": self.node.normal.to_array(),
            "active": self.node.active,
        })
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

fn sync_section_views(world: &mut World) {
    let saved = {
        let doc_props = world.resource::<DocumentProperties>();
        doc_props
            .domain_defaults
            .get(SECTION_VIEW_METADATA_KEY)
            .cloned()
    };
    let saved_changed = {
        let sync_state = world.resource::<SectionViewSyncState>();
        saved != sync_state.last_serialized
    };

    if saved_changed {
        match saved.as_ref() {
            Some(value) => {
                let Some(snapshots) = deserialize_section_views(value) else {
                    world.resource_mut::<SectionViewSyncState>().last_serialized = saved.clone();
                    return;
                };
                apply_section_views_to_world(world, &snapshots);
            }
            None => apply_section_views_to_world(world, &[]),
        }
        world.resource_mut::<SectionViewSyncState>().last_serialized = saved.clone();
    }

    let serialized = serialize_section_views_from_world(world);
    {
        let mut doc_props = world.resource_mut::<DocumentProperties>();
        match &serialized {
            Some(value) => {
                if doc_props.domain_defaults.get(SECTION_VIEW_METADATA_KEY) != Some(value) {
                    doc_props
                        .domain_defaults
                        .insert(SECTION_VIEW_METADATA_KEY.to_string(), value.clone());
                }
            }
            None => {
                doc_props.domain_defaults.remove(SECTION_VIEW_METADATA_KEY);
            }
        }
    }
    world.resource_mut::<SectionViewSyncState>().last_serialized = serialized;
}

fn serialize_section_views_from_world(world: &mut World) -> Option<Value> {
    let mut query = world.query::<(&ElementId, &ClipPlaneNode)>();
    let mut section_views = query
        .iter(world)
        .map(|(element_id, node)| ClipPlaneSnapshot {
            element_id: *element_id,
            node: node.clone(),
        })
        .collect::<Vec<_>>();
    if section_views.is_empty() {
        return None;
    }
    section_views.sort_by_key(|snapshot| snapshot.element_id.0);
    serde_json::to_value(section_views).ok()
}

fn deserialize_section_views(value: &Value) -> Option<Vec<ClipPlaneSnapshot>> {
    let mut section_views: Vec<ClipPlaneSnapshot> = serde_json::from_value(value.clone()).ok()?;
    section_views.sort_by_key(|snapshot| snapshot.element_id.0);
    Some(section_views)
}

fn apply_section_views_to_world(world: &mut World, snapshots: &[ClipPlaneSnapshot]) {
    let mut existing_query = world.query::<(Entity, &ElementId, &ClipPlaneNode)>();
    let mut existing = existing_query
        .iter(world)
        .map(|(entity, element_id, node)| (element_id.0, (entity, node.clone())))
        .collect::<HashMap<_, _>>();

    for snapshot in snapshots {
        if let Some((entity, existing_node)) = existing.remove(&snapshot.element_id.0) {
            if existing_node != snapshot.node {
                world.entity_mut(entity).insert(snapshot.node.clone());
            }
        } else {
            world.spawn((
                snapshot.element_id,
                snapshot.node.clone(),
                Visibility::Visible,
            ));
        }
    }

    for (_, (entity, _)) in existing {
        let _ = world.despawn(entity);
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
        let name = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Section")
            .to_string();
        let origin = parse_vec3_field(data, "origin").unwrap_or(Vec3::ZERO);
        let normal = parse_normal_field(data).unwrap_or(Vec3::Y);
        let active = data.get("active").and_then(Value::as_bool).unwrap_or(true);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct VisibilityChangeCount(usize);

    fn count_visibility_changes(
        changed: Query<(), Changed<Visibility>>,
        mut count: ResMut<VisibilityChangeCount>,
    ) {
        count.0 += changed.iter().count();
    }

    #[test]
    fn clip_visibility_writes_only_when_the_result_changes() {
        let mut app = App::new();
        app.init_resource::<VisibilityChangeCount>().add_systems(
            Update,
            (apply_clip_plane_visibility, count_visibility_changes).chain(),
        );
        app.world_mut().spawn(ClipPlaneNode::at_y(0.0));
        let renderable = app
            .world_mut()
            .spawn((
                Mesh3d(Handle::default()),
                GlobalTransform::from_translation(Vec3::NEG_Y),
                Aabb::from_min_max(Vec3::splat(-0.1), Vec3::splat(0.1)),
                Visibility::Visible,
            ))
            .id();

        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(renderable).unwrap(),
            Visibility::Hidden
        );

        app.world_mut().resource_mut::<VisibilityChangeCount>().0 = 0;
        app.update();
        assert_eq!(app.world().resource::<VisibilityChangeCount>().0, 0);

        *app.world_mut()
            .entity_mut(renderable)
            .get_mut::<GlobalTransform>()
            .unwrap() = GlobalTransform::from_translation(Vec3::Y);
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(renderable).unwrap(),
            Visibility::Visible
        );
        assert_eq!(app.world().resource::<VisibilityChangeCount>().0, 1);
    }

    #[test]
    fn section_culls_actual_offset_geometry_and_retains_crossing_and_layer_hidden_meshes() {
        let mut app = App::new();
        app.add_systems(Update, apply_clip_plane_visibility);
        let plane = app
            .world_mut()
            .spawn(ClipPlaneNode {
                name: "cut".into(),
                origin: Vec3::Y * 1.2,
                normal: Vec3::NEG_Y,
                active: true,
            })
            .id();
        let roof = app
            .world_mut()
            .spawn((
                Mesh3d(Handle::default()),
                GlobalTransform::IDENTITY,
                Aabb::from_min_max(Vec3::new(-2.0, 2.3, -3.0), Vec3::new(2.0, 3.4, 3.0)),
                Visibility::Visible,
            ))
            .id();
        let wall = app
            .world_mut()
            .spawn((
                Mesh3d(Handle::default()),
                GlobalTransform::from_translation(Vec3::Y * 1.3),
                Aabb::from_min_max(Vec3::new(-2.0, -1.3, -0.1), Vec3::new(2.0, 1.3, 0.1)),
                Visibility::Visible,
            ))
            .id();
        let hidden = app
            .world_mut()
            .spawn((
                Mesh3d(Handle::default()),
                GlobalTransform::IDENTITY,
                Aabb::from_min_max(Vec3::ZERO, Vec3::ONE),
                Visibility::Hidden,
            ))
            .id();
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(roof).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            *app.world().get::<Visibility>(wall).unwrap(),
            Visibility::Visible
        );
        assert_eq!(
            *app.world().get::<Visibility>(hidden).unwrap(),
            Visibility::Hidden
        );
        app.world_mut()
            .get_mut::<ClipPlaneNode>(plane)
            .unwrap()
            .active = false;
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(roof).unwrap(),
            Visibility::Visible
        );
        assert_eq!(
            *app.world().get::<Visibility>(hidden).unwrap(),
            Visibility::Hidden
        );
    }

    #[test]
    fn section_views_restore_from_document_metadata() {
        let snapshot = ClipPlaneSnapshot {
            element_id: ElementId(9),
            node: ClipPlaneNode {
                name: "Section A-A".to_string(),
                origin: Vec3::new(0.0, 2.0, 0.0),
                normal: Vec3::Y,
                active: true,
            },
        };
        let serialized =
            serde_json::to_value(vec![snapshot.clone()]).expect("section view should serialize");

        let mut app = App::new();
        let mut doc_props = DocumentProperties::default();
        doc_props
            .domain_defaults
            .insert(SECTION_VIEW_METADATA_KEY.to_string(), serialized);
        app.insert_resource(doc_props)
            .init_resource::<SectionViewSyncState>()
            .add_systems(Update, sync_section_views);

        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&ElementId, &ClipPlaneNode)>();
        let restored = query
            .iter(world)
            .next()
            .expect("section view should be restored from metadata");
        assert_eq!(*restored.0, ElementId(9));
        assert_eq!(restored.1.name, "Section A-A");
        assert_eq!(restored.1.origin, Vec3::new(0.0, 2.0, 0.0));
        assert!(restored.1.active);
    }
}
