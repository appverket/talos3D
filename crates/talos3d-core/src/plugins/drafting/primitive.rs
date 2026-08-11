//! Plane-coordinate authored annotations for the unified Drafting workspace.
//!
//! Five typed intents share one durable aggregate, persistence adapter, edit
//! path, and world/draft conversion. They are drawing metadata, never meshes.

use std::{any::Any, collections::HashMap};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, read_only_property_field, AuthoredEntity,
        BoxedEntity, EntityBounds, EntityScope, HandleInfo, HandleKind, PropertyFieldDef,
        PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityRegistryAppExt, HitCandidate, SnapPoint,
    },
    plugins::{
        command_registry::{
            activate_tool_command, CommandCategory, CommandDescriptor, CommandHandler,
            CommandRegistryAppExt,
        },
        commands::{
            despawn_by_element_id, enqueue_apply_entity_changes, find_entity_by_element_id,
            ApplyEntityChangesCommand,
        },
        document_properties::DocumentProperties,
        identity::{ElementId, ElementIdAllocator},
        snap::SnapKind,
        tools::ActiveTool,
    },
};

use super::{
    active_draft_snapshot, draft::draft_snapshot_for_member, DimPrimitive, DimensionStyle,
    DraftPlane, TextAnchor, DRAFTING_CAPABILITY_ID,
};

pub const DRAFT_PRIMITIVES_METADATA_KEY: &str = "draft_primitives";
pub const DRAFT_LINE_TYPE: &str = "draft_line";
pub const DRAFT_POLYLINE_TYPE: &str = "draft_polyline";
pub const DRAFT_RECTANGLE_TYPE: &str = "draft_rectangle";
pub const DRAFT_CIRCLE_TYPE: &str = "draft_circle";
pub const DRAFT_TEXT_TYPE: &str = "draft_text";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftPrimitiveKind {
    Line,
    Polyline,
    Rectangle,
    Circle,
    Text,
}

impl DraftPrimitiveKind {
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Line => DRAFT_LINE_TYPE,
            Self::Polyline => DRAFT_POLYLINE_TYPE,
            Self::Rectangle => DRAFT_RECTANGLE_TYPE,
            Self::Circle => DRAFT_CIRCLE_TYPE,
            Self::Text => DRAFT_TEXT_TYPE,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Line => "Draft line",
            Self::Polyline => "Draft polyline",
            Self::Rectangle => "Draft rectangle",
            Self::Circle => "Draft circle",
            Self::Text => "Draft text",
        }
    }

    fn tool(self) -> ActiveTool {
        match self {
            Self::Line => ActiveTool::PlaceDraftLine,
            Self::Polyline => ActiveTool::PlaceDraftPolyline,
            Self::Rectangle => ActiveTool::PlaceDraftRectangle,
            Self::Circle => ActiveTool::PlaceDraftCircle,
            Self::Text => ActiveTool::PlaceDraftText,
        }
    }

    fn tool_name(self) -> &'static str {
        match self {
            Self::Line => "PlaceDraftLine",
            Self::Polyline => "PlaceDraftPolyline",
            Self::Rectangle => "PlaceDraftRectangle",
            Self::Circle => "PlaceDraftCircle",
            Self::Text => "PlaceDraftText",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftLine {
    pub a: Vec2,
    pub b: Vec2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftPolyline {
    pub points: Vec<Vec2>,
    #[serde(default)]
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftRectangle {
    pub center: Vec2,
    pub half_extents: Vec2,
    #[serde(default)]
    pub rotation_rad: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftCircle {
    pub center: Vec2,
    pub radius: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftText {
    pub anchor: Vec2,
    pub content: String,
    #[serde(default)]
    pub rotation_rad: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftPrimitiveGeometry {
    Line(DraftLine),
    Polyline(DraftPolyline),
    Rectangle(DraftRectangle),
    Circle(DraftCircle),
    Text(DraftText),
}

impl DraftPrimitiveGeometry {
    pub fn kind(&self) -> DraftPrimitiveKind {
        match self {
            Self::Line(_) => DraftPrimitiveKind::Line,
            Self::Polyline(_) => DraftPrimitiveKind::Polyline,
            Self::Rectangle(_) => DraftPrimitiveKind::Rectangle,
            Self::Circle(_) => DraftPrimitiveKind::Circle,
            Self::Text(_) => DraftPrimitiveKind::Text,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let finite = |point: Vec2| point.is_finite();
        match self {
            Self::Line(line) => {
                if !finite(line.a) || !finite(line.b) || line.a.distance_squared(line.b) <= 1e-10 {
                    return Err("draft line requires two distinct finite points".to_string());
                }
            }
            Self::Polyline(polyline) => {
                if polyline.points.len() < 2 || polyline.points.iter().any(|p| !finite(*p)) {
                    return Err("draft polyline requires at least two finite points".to_string());
                }
                if polyline
                    .points
                    .windows(2)
                    .any(|pair| pair[0].distance_squared(pair[1]) <= 1e-10)
                {
                    return Err("draft polyline cannot contain zero-length segments".to_string());
                }
            }
            Self::Rectangle(rectangle) => {
                if !finite(rectangle.center)
                    || !finite(rectangle.half_extents)
                    || !rectangle.rotation_rad.is_finite()
                    || rectangle.half_extents.x <= 0.0
                    || rectangle.half_extents.y <= 0.0
                {
                    return Err("draft rectangle requires a finite center/rotation and positive half extents".to_string());
                }
            }
            Self::Circle(circle) => {
                if !finite(circle.center) || !circle.radius.is_finite() || circle.radius <= 0.0 {
                    return Err(
                        "draft circle requires a finite center and positive radius".to_string()
                    );
                }
            }
            Self::Text(text) => {
                if !finite(text.anchor)
                    || !text.rotation_rad.is_finite()
                    || text.content.trim().is_empty()
                {
                    return Err(
                        "draft text requires a finite anchor/rotation and non-empty content"
                            .to_string(),
                    );
                }
            }
        }
        Ok(())
    }

    pub fn center(&self) -> Vec2 {
        match self {
            Self::Line(line) => (line.a + line.b) * 0.5,
            Self::Polyline(polyline) => {
                polyline.points.iter().copied().sum::<Vec2>() / polyline.points.len() as f32
            }
            Self::Rectangle(rectangle) => rectangle.center,
            Self::Circle(circle) => circle.center,
            Self::Text(text) => text.anchor,
        }
    }

    pub fn segments(&self) -> Vec<(Vec2, Vec2)> {
        match self {
            Self::Line(line) => vec![(line.a, line.b)],
            Self::Polyline(polyline) => {
                let mut segments = polyline
                    .points
                    .windows(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect::<Vec<_>>();
                if polyline.closed && polyline.points.len() > 2 {
                    segments.push((*polyline.points.last().unwrap(), polyline.points[0]));
                }
                segments
            }
            Self::Rectangle(rectangle) => {
                let (sin, cos) = rectangle.rotation_rad.sin_cos();
                let axis_u = Vec2::new(cos, sin) * rectangle.half_extents.x;
                let axis_v = Vec2::new(-sin, cos) * rectangle.half_extents.y;
                let a = rectangle.center - axis_u - axis_v;
                let b = rectangle.center + axis_u - axis_v;
                let c = rectangle.center + axis_u + axis_v;
                let d = rectangle.center - axis_u + axis_v;
                vec![(a, b), (b, c), (c, d), (d, a)]
            }
            Self::Circle(circle) => circle_segments(circle, 64),
            Self::Text(_) => Vec::new(),
        }
    }

    fn transform_points(&mut self, mut transform: impl FnMut(Vec2) -> Vec2) {
        match self {
            Self::Line(line) => {
                line.a = transform(line.a);
                line.b = transform(line.b);
            }
            Self::Polyline(polyline) => {
                for point in &mut polyline.points {
                    *point = transform(*point);
                }
            }
            Self::Rectangle(rectangle) => {
                rectangle.center = transform(rectangle.center);
            }
            Self::Circle(circle) => circle.center = transform(circle.center),
            Self::Text(text) => text.anchor = transform(text.anchor),
        }
    }

    fn translate(&mut self, delta: Vec2) {
        self.transform_points(|point| point + delta);
    }

    fn rotate(&mut self, angle: f32) {
        let (sin, cos) = angle.sin_cos();
        self.transform_points(|point| {
            Vec2::new(cos * point.x - sin * point.y, sin * point.x + cos * point.y)
        });
        if let Self::Text(text) = self {
            text.rotation_rad += angle;
        } else if let Self::Rectangle(rectangle) = self {
            rectangle.rotation_rad += angle;
        }
    }

    fn scale(&mut self, factor: Vec2, center: Vec2) {
        self.transform_points(|point| center + (point - center) * factor);
        if let Self::Circle(circle) = self {
            circle.radius *= factor.x.abs().min(factor.y.abs());
        } else if let Self::Rectangle(rectangle) = self {
            rectangle.half_extents *= factor.abs();
        }
    }
}

fn circle_segments(circle: &DraftCircle, count: usize) -> Vec<(Vec2, Vec2)> {
    (0..count)
        .map(|index| {
            let angle_a = std::f32::consts::TAU * index as f32 / count as f32;
            let angle_b = std::f32::consts::TAU * (index + 1) as f32 / count as f32;
            (
                circle.center + Vec2::new(angle_a.cos(), angle_a.sin()) * circle.radius,
                circle.center + Vec2::new(angle_b.cos(), angle_b.sin()) * circle.radius,
            )
        })
        .collect()
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftPrimitiveNode {
    pub geometry: DraftPrimitiveGeometry,
    pub layer: String,
    pub style_name: String,
    #[serde(default = "default_true")]
    pub visible: bool,
}

fn default_true() -> bool {
    true
}

impl DraftPrimitiveNode {
    pub fn validate(&self) -> Result<(), String> {
        self.geometry.validate()?;
        if self.layer.trim().is_empty() || self.style_name.trim().is_empty() {
            return Err("draft annotation layer/style references must be non-empty".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftPrimitiveSnapshot {
    pub element_id: ElementId,
    pub node: DraftPrimitiveNode,
    /// Derived membership context used by history apply/remove. Durable
    /// membership remains stored only on the Draft.
    #[serde(skip)]
    pub draft_id: Option<ElementId>,
    /// Derived edit context. It is captured from Draft membership and is never
    /// persisted as a second coordinate authority.
    #[serde(skip, default = "DraftPlane::ground")]
    pub plane: DraftPlane,
}

impl DraftPrimitiveSnapshot {
    fn local_center(&self) -> Vec2 {
        self.node.geometry.center()
    }

    fn world_center(&self) -> Vec3 {
        self.plane.to_world(self.local_center())
    }

    fn world_segments(&self) -> Vec<(Vec3, Vec3)> {
        self.node
            .geometry
            .segments()
            .into_iter()
            .map(|(a, b)| (self.plane.to_world(a), self.plane.to_world(b)))
            .collect()
    }

    /// Adapt semantic Draft geometry into the same backend-neutral primitives
    /// already consumed by live Drafting and every vector exporter.
    pub(crate) fn to_scene_primitives(
        &self,
        style: &DimensionStyle,
        mut project: impl FnMut(Vec2) -> Option<Vec2>,
    ) -> Vec<DimPrimitive> {
        if !self.node.visible {
            return Vec::new();
        }
        match &self.node.geometry {
            DraftPrimitiveGeometry::Text(text) => {
                let Some(anchor) = project(text.anchor) else {
                    return Vec::new();
                };
                let direction = Vec2::new(text.rotation_rad.cos(), text.rotation_rad.sin());
                let rotation_rad = project(text.anchor + direction)
                    .map(|end| end - anchor)
                    .filter(|direction| direction.length_squared() > 1e-10)
                    .map(|direction| direction.y.atan2(direction.x))
                    .unwrap_or(0.0);
                vec![DimPrimitive::Text {
                    anchor,
                    content: text.content.clone(),
                    height_mm: style.text_height_mm,
                    rotation_rad,
                    anchor_mode: TextAnchor::CenterBaseline,
                    font_family: style.text_font.clone(),
                    color_hex: style.text_color_hex.clone(),
                }]
            }
            geometry => geometry
                .segments()
                .into_iter()
                .filter_map(|(a, b)| {
                    Some(DimPrimitive::LineSegment {
                        a: project(a)?,
                        b: project(b)?,
                        stroke_mm: style.dim_line_stroke_mm,
                    })
                })
                .collect(),
        }
    }
}

impl AuthoredEntity for DraftPrimitiveSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        self.node.geometry.kind().type_name()
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.node.geometry.kind().label().to_string()
    }

    fn center(&self) -> Vec3 {
        self.world_center()
    }

    fn scope(&self) -> EntityScope {
        EntityScope::DrawingMetadata
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        next.node.geometry.translate(Vec2::new(
            delta.dot(self.plane.tangent),
            delta.dot(self.plane.bitangent),
        ));
        next.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut next = self.clone();
        let rotated_tangent = rotation * self.plane.tangent;
        let angle = rotated_tangent
            .dot(self.plane.bitangent)
            .atan2(rotated_tangent.dot(self.plane.tangent));
        next.node.geometry.rotate(angle);
        next.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        let local_center = self.plane.project_to_2d(center);
        let local_factor = Vec2::new(
            (self.plane.tangent * factor).length(),
            (self.plane.bitangent * factor).length(),
        );
        next.node.geometry.scale(local_factor, local_center);
        next.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            read_only_property_field(
                "kind",
                "Kind",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.type_name().to_string())),
            ),
            property_field_with(
                "layer",
                "Layer",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.layer.clone())),
                true,
            ),
            property_field_with(
                "style",
                "Style",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.style_name.clone())),
                true,
            ),
            property_field_with(
                "visible",
                "Visible",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.visible.to_string())),
                true,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut next = self.clone();
        match property_name {
            "layer" => next.node.layer = required_string(value, "layer")?,
            "style" => next.node.style_name = required_string(value, "style")?,
            "visible" => {
                next.node.visible = value
                    .as_bool()
                    .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                    .ok_or_else(|| "visible must be a boolean".to_string())?;
            }
            _ => {
                return Err(invalid_property_error(
                    self.type_name(),
                    &["layer", "style", "visible"],
                ));
            }
        }
        next.node.validate()?;
        Ok(next.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        self.node
            .geometry
            .segments()
            .into_iter()
            .enumerate()
            .flat_map(|(index, (a, b))| [(index * 2, a), (index * 2 + 1, b)])
            .map(|(index, point)| HandleInfo {
                id: format!("point:{index}"),
                position: self.plane.to_world(point),
                kind: HandleKind::Vertex,
                label: "Draft point".to_string(),
            })
            .collect()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        let mut points = self
            .world_segments()
            .into_iter()
            .flat_map(|(a, b)| [a, b])
            .collect::<Vec<_>>();
        if points.is_empty() {
            points.push(self.world_center());
        }
        let min = points
            .iter()
            .copied()
            .fold(Vec3::splat(f32::INFINITY), Vec3::min);
        let max = points
            .iter()
            .copied()
            .fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max);
        Some(EntityBounds { min, max })
    }

    fn snap_segments(&self) -> Vec<(Vec3, Vec3)> {
        self.world_segments()
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert(self.node.clone());
        } else {
            world.spawn((self.element_id, self.node.clone()));
        }
        ensure_membership(world, self.draft_id, self.element_id);
    }

    fn remove_from(&self, world: &mut World) {
        remove_membership(world, self.element_id);
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        for (a, b) in self.world_segments() {
            gizmos.line(a, b, color);
        }
        if matches!(self.node.geometry, DraftPrimitiveGeometry::Text(_)) {
            let center = self.world_center();
            gizmos.line(
                center - self.plane.tangent * 0.1,
                center + self.plane.tangent * 0.1,
                color,
            );
            gizmos.line(
                center - self.plane.bitangent * 0.1,
                center + self.plane.bitangent * 0.1,
                color,
            );
        }
    }

    fn preview_line_count(&self) -> usize {
        self.world_segments().len()
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl From<DraftPrimitiveSnapshot> for BoxedEntity {
    fn from(snapshot: DraftPrimitiveSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

fn ensure_membership(world: &mut World, draft_id: Option<ElementId>, member_id: ElementId) {
    let Some(draft_id) = draft_id else {
        return;
    };
    let Some(entity) = find_entity_by_element_id(world, draft_id) else {
        return;
    };
    let Some(mut draft) = world.get_mut::<super::DraftNode>(entity) else {
        return;
    };
    draft.members.push(member_id);
    draft.members.sort_by_key(|member| member.0);
    draft.members.dedup();
}

fn remove_membership(world: &mut World, member_id: ElementId) {
    let Some(mut query) = world.try_query::<&mut super::DraftNode>() else {
        return;
    };
    for mut draft in query.iter_mut(world) {
        draft.members.retain(|member| *member != member_id);
    }
}

fn capture_snapshot_for_kind(
    entity_ref: &bevy::ecs::world::EntityRef,
    world: &World,
    kind: DraftPrimitiveKind,
) -> Option<BoxedEntity> {
    let element_id = *entity_ref.get::<ElementId>()?;
    let node = entity_ref.get::<DraftPrimitiveNode>()?;
    if node.geometry.kind() != kind {
        return None;
    }
    let draft = draft_snapshot_for_member(world, element_id);
    let plane = draft
        .as_ref()
        .map(|draft| draft.node.plane.clone())
        .unwrap_or_else(DraftPlane::ground);
    Some(
        DraftPrimitiveSnapshot {
            element_id,
            node: node.clone(),
            plane,
            draft_id: draft.map(|draft| draft.element_id),
        }
        .into(),
    )
}

fn persisted_snapshot(data: &Value, kind: DraftPrimitiveKind) -> Result<BoxedEntity, String> {
    let snapshot: DraftPrimitiveSnapshot =
        serde_json::from_value(data.clone()).map_err(|error| error.to_string())?;
    if snapshot.node.geometry.kind() != kind {
        return Err(format!("expected {} persisted data", kind.type_name()));
    }
    snapshot.node.validate()?;
    Ok(snapshot.into())
}

fn hit_test_kind(world: &World, ray: Ray3d, kind: DraftPrimitiveKind) -> Option<HitCandidate> {
    let mut query = world.try_query::<(Entity, &ElementId, &DraftPrimitiveNode)>()?;
    let mut best = None;
    for (entity, element_id, node) in query.iter(world) {
        if !node.visible || node.geometry.kind() != kind {
            continue;
        }
        let Some(draft) = draft_snapshot_for_member(world, *element_id) else {
            continue;
        };
        let hit = draft.node.plane.intersect_ray(ray)?;
        let uv = draft.node.plane.project_to_2d(hit);
        let distance_2d = primitive_distance(&node.geometry, uv);
        if distance_2d > 0.15 {
            continue;
        }
        let ray_distance = ray.origin.distance(hit);
        if best.is_none_or(|candidate: HitCandidate| ray_distance < candidate.distance) {
            best = Some(HitCandidate {
                entity,
                distance: ray_distance,
            });
        }
    }
    best
}

fn primitive_distance(geometry: &DraftPrimitiveGeometry, point: Vec2) -> f32 {
    if let DraftPrimitiveGeometry::Text(text) = geometry {
        return point.distance(text.anchor);
    }
    geometry
        .segments()
        .into_iter()
        .map(|(a, b)| distance_to_segment(point, a, b))
        .fold(f32::INFINITY, f32::min)
}

fn distance_to_segment(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let segment = b - a;
    let t = ((point - a).dot(segment) / segment.length_squared().max(1e-10)).clamp(0.0, 1.0);
    point.distance(a + segment * t)
}

fn collect_snap_points_for_kind(world: &World, out: &mut Vec<SnapPoint>, kind: DraftPrimitiveKind) {
    let Some(mut query) = world.try_query::<(&ElementId, &DraftPrimitiveNode)>() else {
        return;
    };
    for (element_id, node) in query.iter(world) {
        if !node.visible || node.geometry.kind() != kind {
            continue;
        }
        let Some(draft) = draft_snapshot_for_member(world, *element_id) else {
            continue;
        };
        let mut local_points = node
            .geometry
            .segments()
            .into_iter()
            .flat_map(|(a, b)| [a, b])
            .collect::<Vec<_>>();
        local_points.push(node.geometry.center());
        local_points.sort_by_key(|point| (point.x.to_bits(), point.y.to_bits()));
        local_points.dedup();
        for point in local_points {
            out.push(SnapPoint {
                position: draft.node.plane.to_world(point),
                kind: SnapKind::Endpoint,
                element_id: Some(*element_id),
                label: Some(kind.label().to_string()),
            });
        }
    }
}

macro_rules! draft_factory {
    ($name:ident, $kind:expr) => {
        pub struct $name;

        impl AuthoredEntityFactory for $name {
            fn type_name(&self) -> &'static str {
                $kind.type_name()
            }

            fn capture_snapshot(
                &self,
                entity_ref: &bevy::ecs::world::EntityRef,
                world: &World,
            ) -> Option<BoxedEntity> {
                capture_snapshot_for_kind(entity_ref, world, $kind)
            }

            fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
                persisted_snapshot(data, $kind)
            }

            fn from_create_request(
                &self,
                _world: &World,
                _request: &Value,
            ) -> Result<BoxedEntity, String> {
                Err(format!(
                    "use drafting.create_{} so annotation creation and Draft membership are atomic",
                    $kind.type_name().trim_start_matches("draft_")
                ))
            }

            fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
                hit_test_kind(world, ray, $kind)
            }

            fn collect_snap_points(&self, world: &World, out: &mut Vec<SnapPoint>) {
                collect_snap_points_for_kind(world, out, $kind);
            }
        }
    };
}

draft_factory!(DraftLineFactory, DraftPrimitiveKind::Line);
draft_factory!(DraftPolylineFactory, DraftPrimitiveKind::Polyline);
draft_factory!(DraftRectangleFactory, DraftPrimitiveKind::Rectangle);
draft_factory!(DraftCircleFactory, DraftPrimitiveKind::Circle);
draft_factory!(DraftTextFactory, DraftPrimitiveKind::Text);

pub(crate) fn register_draft_primitive_support(app: &mut App) {
    app.init_resource::<DraftPrimitiveSyncState>()
        .register_authored_entity_factory(DraftLineFactory)
        .register_authored_entity_factory(DraftPolylineFactory)
        .register_authored_entity_factory(DraftRectangleFactory)
        .register_authored_entity_factory(DraftCircleFactory)
        .register_authored_entity_factory(DraftTextFactory);

    let commands: [(DraftPrimitiveKind, CommandHandler); 5] = [
        (DraftPrimitiveKind::Line, execute_create_line),
        (DraftPrimitiveKind::Polyline, execute_create_polyline),
        (DraftPrimitiveKind::Rectangle, execute_create_rectangle),
        (DraftPrimitiveKind::Circle, execute_create_circle),
        (DraftPrimitiveKind::Text, execute_create_text),
    ];
    for (kind, handler) in commands {
        app.register_command(create_command_descriptor(kind), handler);
    }
    app.register_command(
        CommandDescriptor {
            id: "drafting.inspect_primitives".to_string(),
            label: "Inspect Draft primitives".to_string(),
            description: "Inspect authored plane-coordinate Draft annotations and their resolved Draft membership.".to_string(),
            category: CommandCategory::View,
            parameters: None,
            version: 1,
            default_shortcut: None,
            icon: None,
            hint: None,
            requires_selection: false,
            show_in_menu: false,
            activates_tool: None,
            capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
        },
        execute_inspect_primitives,
    )
    .add_systems(
        Update,
        sync_draft_primitives.after(super::draft::sync_drafts),
    );
}

fn create_command_descriptor(kind: DraftPrimitiveKind) -> CommandDescriptor {
    let suffix = kind.type_name().trim_start_matches("draft_");
    CommandDescriptor {
        id: format!("drafting.create_{suffix}"),
        label: format!("Create {}", kind.label()),
        description: format!(
            "Create a semantic {} in the active Draft plane and add it to Draft membership atomically.",
            kind.type_name()
        ),
        category: CommandCategory::Create,
        parameters: Some(create_parameters_schema(kind)),
        version: 1,
        default_shortcut: None,
        icon: None,
        hint: Some("Coordinates are [u, v] values in the active Draft plane.".to_string()),
        requires_selection: false,
        show_in_menu: true,
        activates_tool: Some(kind.tool_name().to_string()),
        capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
    }
}

fn create_parameters_schema(kind: DraftPrimitiveKind) -> Value {
    let point = json!({
        "type": "array",
        "items": {"type": "number"},
        "minItems": 2,
        "maxItems": 2
    });
    let mut properties = serde_json::Map::from_iter([
        ("layer".to_string(), json!({"type":"string"})),
        ("style".to_string(), json!({"type":"string"})),
        (
            "visible".to_string(),
            json!({"type":"boolean","default":true}),
        ),
    ]);
    let required = match kind {
        DraftPrimitiveKind::Line => {
            properties.insert("a".to_string(), point.clone());
            properties.insert("b".to_string(), point.clone());
            vec!["a", "b"]
        }
        DraftPrimitiveKind::Polyline => {
            properties.insert(
                "points".to_string(),
                json!({"type":"array","items":point,"minItems":2}),
            );
            properties.insert(
                "closed".to_string(),
                json!({"type":"boolean","default":false}),
            );
            vec!["points"]
        }
        DraftPrimitiveKind::Rectangle => {
            properties.insert("a".to_string(), point.clone());
            properties.insert("b".to_string(), point.clone());
            properties.insert(
                "rotation_rad".to_string(),
                json!({"type":"number","default":0}),
            );
            vec!["a", "b"]
        }
        DraftPrimitiveKind::Circle => {
            properties.insert("center".to_string(), point);
            properties.insert(
                "radius".to_string(),
                json!({"type":"number","exclusiveMinimum":0}),
            );
            vec!["center", "radius"]
        }
        DraftPrimitiveKind::Text => {
            properties.insert("anchor".to_string(), point);
            properties.insert(
                "content".to_string(),
                json!({"type":"string","minLength":1}),
            );
            properties.insert(
                "rotation_rad".to_string(),
                json!({"type":"number","default":0}),
            );
            vec!["anchor", "content"]
        }
    };
    json!({
        "type":"object",
        "required":required,
        "properties":properties,
        "additionalProperties":false
    })
}

#[derive(Resource, Default)]
pub(crate) struct DraftPrimitiveSyncState {
    last_serialized: Option<Value>,
}

pub(crate) fn sync_draft_primitives(world: &mut World) {
    if !world.contains_resource::<DocumentProperties>() {
        return;
    }
    let saved = world
        .resource::<DocumentProperties>()
        .domain_defaults
        .get(DRAFT_PRIMITIVES_METADATA_KEY)
        .cloned();
    let saved_changed = saved != world.resource::<DraftPrimitiveSyncState>().last_serialized;
    if saved_changed {
        let snapshots = saved
            .as_ref()
            .and_then(|value| {
                serde_json::from_value::<Vec<DraftPrimitiveSnapshot>>(value.clone()).ok()
            })
            .unwrap_or_default();
        apply_snapshots(world, &snapshots);
        world
            .resource_mut::<DraftPrimitiveSyncState>()
            .last_serialized = saved;
    }

    let serialized = serialize_from_world(world);
    {
        let mut properties = world.resource_mut::<DocumentProperties>();
        match &serialized {
            Some(value)
                if properties
                    .domain_defaults
                    .get(DRAFT_PRIMITIVES_METADATA_KEY)
                    != Some(value) =>
            {
                properties
                    .domain_defaults
                    .insert(DRAFT_PRIMITIVES_METADATA_KEY.to_string(), value.clone());
            }
            Some(_) => {}
            None => {
                properties
                    .domain_defaults
                    .remove(DRAFT_PRIMITIVES_METADATA_KEY);
            }
        }
    }
    world
        .resource_mut::<DraftPrimitiveSyncState>()
        .last_serialized = serialized;
}

fn serialize_from_world(world: &mut World) -> Option<Value> {
    let mut query = world.query::<(&ElementId, &DraftPrimitiveNode)>();
    let mut snapshots = query
        .iter(world)
        .map(|(element_id, node)| DraftPrimitiveSnapshot {
            element_id: *element_id,
            node: node.clone(),
            plane: DraftPlane::ground(),
            draft_id: None,
        })
        .collect::<Vec<_>>();
    if snapshots.is_empty() {
        return None;
    }
    snapshots.sort_by_key(|snapshot| snapshot.element_id.0);
    serde_json::to_value(snapshots).ok()
}

fn apply_snapshots(world: &mut World, snapshots: &[DraftPrimitiveSnapshot]) {
    let mut query = world.query::<(Entity, &ElementId, &DraftPrimitiveNode)>();
    let mut existing = query
        .iter(world)
        .map(|(entity, id, node)| (id.0, (entity, node.clone())))
        .collect::<HashMap<_, _>>();
    for snapshot in snapshots {
        if snapshot.node.validate().is_err() {
            continue;
        }
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

pub(crate) fn execute_create_line(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    create_or_activate(world, params, DraftPrimitiveKind::Line)
}

pub(crate) fn execute_create_polyline(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    create_or_activate(world, params, DraftPrimitiveKind::Polyline)
}

pub(crate) fn execute_create_rectangle(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    create_or_activate(world, params, DraftPrimitiveKind::Rectangle)
}

pub(crate) fn execute_create_circle(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    create_or_activate(world, params, DraftPrimitiveKind::Circle)
}

pub(crate) fn execute_create_text(
    world: &mut World,
    params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    create_or_activate(world, params, DraftPrimitiveKind::Text)
}

fn create_or_activate(
    world: &mut World,
    params: &Value,
    kind: DraftPrimitiveKind,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    if params.as_object().is_some_and(serde_json::Map::is_empty) {
        let workspace = world
            .get_resource::<super::DraftingWorkspaceState>()
            .ok_or_else(|| "Drafting workspace state is unavailable".to_string())?;
        if !workspace.is_active() || active_draft_snapshot(world).is_none() {
            return Err(
                "enter Drafting and select a Draft before activating a Draft primitive tool"
                    .to_string(),
            );
        }
        return activate_tool_command(world, kind.tool());
    }
    create_primitive(world, params, kind)
}

fn create_primitive(
    world: &mut World,
    params: &Value,
    kind: DraftPrimitiveKind,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let draft = active_draft_snapshot(world).ok_or_else(|| {
        "an active Draft is required to create plane-coordinate annotations".to_string()
    })?;
    let element_id = world
        .get_resource::<ElementIdAllocator>()
        .ok_or_else(|| "ElementIdAllocator not available".to_string())?
        .next_id();
    let geometry = parse_geometry(params, kind)?;
    let node = DraftPrimitiveNode {
        geometry,
        layer: params
            .get("layer")
            .and_then(Value::as_str)
            .unwrap_or(&draft.node.defaults.layer)
            .to_string(),
        style_name: params
            .get("style")
            .and_then(Value::as_str)
            .unwrap_or(&draft.node.defaults.dimension_style)
            .to_string(),
        visible: params
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    };
    node.validate()?;
    let annotation = DraftPrimitiveSnapshot {
        element_id,
        node,
        plane: draft.node.plane.clone(),
        draft_id: Some(draft.element_id),
    };
    let mut updated_draft = draft.clone();
    updated_draft.node.members.push(element_id);
    updated_draft.node.normalize_and_validate()?;
    let draft_id = updated_draft.element_id;
    enqueue_apply_entity_changes(
        world,
        ApplyEntityChangesCommand {
            label: "Create Draft primitive",
            before: vec![draft.into()],
            after: vec![updated_draft.into(), annotation.clone().into()],
        },
    );
    Ok(crate::plugins::command_registry::CommandResult {
        created: vec![element_id.0],
        modified: vec![draft_id.0],
        output: Some(inspect_snapshot(world, &annotation, Some(draft_id))),
        ..Default::default()
    })
}

fn parse_geometry(
    params: &Value,
    kind: DraftPrimitiveKind,
) -> Result<DraftPrimitiveGeometry, String> {
    Ok(match kind {
        DraftPrimitiveKind::Line => DraftPrimitiveGeometry::Line(DraftLine {
            a: vec2_field(params, "a")?,
            b: vec2_field(params, "b")?,
        }),
        DraftPrimitiveKind::Polyline => DraftPrimitiveGeometry::Polyline(DraftPolyline {
            points: params
                .get("points")
                .and_then(Value::as_array)
                .ok_or_else(|| "points must be an array".to_string())?
                .iter()
                .map(vec2_from_json)
                .collect::<Result<Vec<_>, _>>()?,
            closed: params
                .get("closed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        DraftPrimitiveKind::Rectangle => {
            let a = vec2_field(params, "a")?;
            let b = vec2_field(params, "b")?;
            let min = a.min(b);
            let max = a.max(b);
            DraftPrimitiveGeometry::Rectangle(DraftRectangle {
                center: (min + max) * 0.5,
                half_extents: (max - min) * 0.5,
                rotation_rad: params
                    .get("rotation_rad")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0) as f32,
            })
        }
        DraftPrimitiveKind::Circle => DraftPrimitiveGeometry::Circle(DraftCircle {
            center: vec2_field(params, "center")?,
            radius: params
                .get("radius")
                .and_then(Value::as_f64)
                .ok_or_else(|| "radius must be a number".to_string())? as f32,
        }),
        DraftPrimitiveKind::Text => DraftPrimitiveGeometry::Text(DraftText {
            anchor: vec2_field(params, "anchor")?,
            content: required_string(params.get("content").unwrap_or(&Value::Null), "content")?,
            rotation_rad: params
                .get("rotation_rad")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32,
        }),
    })
}

fn vec2_field(params: &Value, field: &str) -> Result<Vec2, String> {
    params
        .get(field)
        .ok_or_else(|| format!("{field} is required"))
        .and_then(vec2_from_json)
}

fn vec2_from_json(value: &Value) -> Result<Vec2, String> {
    let values = value
        .as_array()
        .ok_or_else(|| "expected [u, v]".to_string())?;
    if values.len() != 2 {
        return Err("expected exactly two plane coordinates".to_string());
    }
    let u = values[0]
        .as_f64()
        .ok_or_else(|| "plane coordinate must be numeric".to_string())? as f32;
    let v = values[1]
        .as_f64()
        .ok_or_else(|| "plane coordinate must be numeric".to_string())? as f32;
    let point = Vec2::new(u, v);
    if !point.is_finite() {
        return Err("plane coordinates must be finite".to_string());
    }
    Ok(point)
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

pub(crate) fn execute_inspect_primitives(
    world: &mut World,
    _params: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let mut query = world.query::<(&ElementId, &DraftPrimitiveNode)>();
    let mut annotations = query
        .iter(world)
        .map(|(element_id, node)| {
            let draft = draft_snapshot_for_member(world, *element_id);
            let snapshot = DraftPrimitiveSnapshot {
                element_id: *element_id,
                node: node.clone(),
                plane: draft
                    .as_ref()
                    .map(|draft| draft.node.plane.clone())
                    .unwrap_or_else(DraftPlane::ground),
                draft_id: draft.as_ref().map(|draft| draft.element_id),
            };
            inspect_snapshot(world, &snapshot, draft.map(|draft| draft.element_id))
        })
        .collect::<Vec<_>>();
    annotations.sort_by_key(|value| value["element_id"].as_u64().unwrap_or(u64::MAX));
    Ok(crate::plugins::command_registry::CommandResult {
        output: Some(json!({
            "annotations": annotations,
            "authority": {
                "durable": "DocumentProperties.domain_defaults.draft_primitives",
                "coordinates": "Draft.plane via DrawingPlane",
                "membership": "Draft.members",
                "projection": "DrawingScene (derived)"
            }
        })),
        ..Default::default()
    })
}

fn inspect_snapshot(
    _world: &World,
    snapshot: &DraftPrimitiveSnapshot,
    draft_id: Option<ElementId>,
) -> Value {
    json!({
        "element_id": snapshot.element_id.0,
        "type_name": snapshot.type_name(),
        "draft_id": draft_id.map(|id| id.0),
        "membership_status": if draft_id.is_some() { "resolved" } else { "orphaned" },
        "geometry": snapshot.node.geometry,
        "layer": snapshot.node.layer,
        "style": snapshot.node.style_name,
        "visible": snapshot.node.visible,
        "local_center": snapshot.local_center(),
        "world_center": snapshot.world_center(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        command_registry::CommandResult,
        drafting::{draft::DraftSyncState, workspace::DraftingWorkspaceState, DraftNode},
        history::{HistoryPlugin, PendingCommandQueue},
    };

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(HistoryPlugin)
            .insert_resource(ElementIdAllocator::default())
            .insert_resource(DocumentProperties::default())
            .init_resource::<DraftPrimitiveSyncState>()
            .init_resource::<DraftSyncState>()
            .init_resource::<DraftingWorkspaceState>();
        let draft_id = ElementId(10);
        app.world_mut().spawn((draft_id, DraftNode::new("Plan")));
        app.world_mut()
            .resource_mut::<DraftingWorkspaceState>()
            .select_draft(Some(draft_id));
        app
    }

    fn flush(app: &mut App) {
        app.update();
        app.update();
    }

    #[test]
    fn typed_create_is_one_atomic_annotation_and_membership_edit() {
        let mut app = app();
        let CommandResult {
            created, modified, ..
        } = execute_create_rectangle(app.world_mut(), &json!({"a":[0,0],"b":[4,3]})).unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(modified, vec![10]);
        flush(&mut app);
        let created_id = ElementId(created[0]);
        let draft = active_draft_snapshot(app.world()).unwrap();
        assert_eq!(draft.node.members, vec![created_id]);
        assert!(find_entity_by_element_id(app.world_mut(), created_id).is_some());
        assert_eq!(
            app.world().resource::<PendingCommandQueue>().commands.len(),
            0
        );
    }

    #[test]
    fn plane_is_derived_and_not_persisted_per_annotation() {
        let snapshot = DraftPrimitiveSnapshot {
            element_id: ElementId(42),
            node: DraftPrimitiveNode {
                geometry: DraftPrimitiveGeometry::Line(DraftLine {
                    a: Vec2::ZERO,
                    b: Vec2::X,
                }),
                layer: "Default".into(),
                style_name: "architectural_metric".into(),
                visible: true,
            },
            plane: DraftPlane::try_from_origin_normal_tangent(
                Vec3::new(4.0, 5.0, 6.0),
                Vec3::Z,
                Vec3::X,
            )
            .unwrap(),
            draft_id: Some(ElementId(7)),
        };
        let value = snapshot.to_json();
        assert!(value.get("plane").is_none());
        assert_eq!(value["node"]["geometry"]["kind"], "line");
    }

    #[test]
    fn world_translation_maps_through_the_one_draft_plane() {
        let plane =
            DraftPlane::try_from_origin_normal_tangent(Vec3::ZERO, Vec3::NEG_Y, Vec3::X).unwrap();
        let snapshot = DraftPrimitiveSnapshot {
            element_id: ElementId(1),
            node: DraftPrimitiveNode {
                geometry: DraftPrimitiveGeometry::Line(DraftLine {
                    a: Vec2::ZERO,
                    b: Vec2::X,
                }),
                layer: "Default".into(),
                style_name: "architectural_metric".into(),
                visible: true,
            },
            plane: plane.clone(),
            draft_id: None,
        };
        let moved = snapshot.translate_by(Vec3::new(2.0, 0.0, -3.0));
        let moved = moved
            .0
            .as_any()
            .downcast_ref::<DraftPrimitiveSnapshot>()
            .unwrap();
        let DraftPrimitiveGeometry::Line(line) = &moved.node.geometry else {
            panic!()
        };
        assert_eq!(line.a, Vec2::new(2.0, 3.0));
        assert_eq!(moved.center(), plane.to_world(Vec2::new(2.5, 3.0)));
    }

    #[test]
    fn history_apply_and_remove_keep_draft_membership_atomic() {
        let mut world = World::new();
        let draft_id = ElementId(10);
        let primitive_id = ElementId(20);
        world.spawn((draft_id, DraftNode::new("Plan")));
        let snapshot = DraftPrimitiveSnapshot {
            element_id: primitive_id,
            node: DraftPrimitiveNode {
                geometry: DraftPrimitiveGeometry::Circle(DraftCircle {
                    center: Vec2::ZERO,
                    radius: 1.0,
                }),
                layer: "Default".into(),
                style_name: "architectural_metric".into(),
                visible: true,
            },
            plane: DraftPlane::ground(),
            draft_id: Some(draft_id),
        };

        snapshot.apply_to(&mut world);
        assert!(active_members(&world, draft_id).contains(&primitive_id));
        snapshot.remove_from(&mut world);
        assert!(!active_members(&world, draft_id).contains(&primitive_id));
        snapshot.apply_to(&mut world);
        assert!(active_members(&world, draft_id).contains(&primitive_id));
    }

    #[test]
    fn persistence_round_trip_keeps_semantics_without_copying_plane_or_membership() {
        let node = DraftPrimitiveNode {
            geometry: DraftPrimitiveGeometry::Text(DraftText {
                anchor: Vec2::new(2.0, 3.0),
                content: "ROOM".into(),
                rotation_rad: 0.25,
            }),
            layer: "Notes".into(),
            style_name: "architectural_metric".into(),
            visible: true,
        };
        let mut source = World::new();
        source.spawn((ElementId(30), node.clone()));
        let value = serialize_from_world(&mut source).unwrap();
        assert!(value[0].get("plane").is_none());
        assert!(value[0].get("draft_id").is_none());

        let snapshots: Vec<DraftPrimitiveSnapshot> = serde_json::from_value(value).unwrap();
        let mut target = World::new();
        apply_snapshots(&mut target, &snapshots);
        let entity =
            crate::plugins::commands::find_entity_by_element_id_readonly(&target, ElementId(30))
                .unwrap();
        assert_eq!(target.get::<DraftPrimitiveNode>(entity), Some(&node));
    }

    #[test]
    fn rectangle_rotation_preserves_rectangle_instead_of_bounding_boxing_it() {
        let mut geometry = DraftPrimitiveGeometry::Rectangle(DraftRectangle {
            center: Vec2::new(2.0, 0.0),
            half_extents: Vec2::new(2.0, 1.0),
            rotation_rad: 0.0,
        });
        geometry.rotate(std::f32::consts::FRAC_PI_2);
        let DraftPrimitiveGeometry::Rectangle(rectangle) = &geometry else {
            panic!()
        };
        assert!(rectangle.center.abs_diff_eq(Vec2::new(0.0, 2.0), 1e-5));
        assert!((rectangle.rotation_rad - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        let lengths = geometry
            .segments()
            .into_iter()
            .map(|(a, b)| a.distance(b))
            .collect::<Vec<_>>();
        assert_eq!(lengths, vec![4.0, 2.0, 4.0, 2.0]);
    }

    fn active_members(world: &World, draft_id: ElementId) -> Vec<ElementId> {
        let entity =
            crate::plugins::commands::find_entity_by_element_id_readonly(world, draft_id).unwrap();
        world.get::<DraftNode>(entity).unwrap().members.clone()
    }
}
