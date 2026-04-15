use std::{any::Any, f32::consts::PI};

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, scalar_from_json,
        vec3_from_json, AuthoredEntity, BoxedEntity, HandleInfo, HandleKind, PropertyFieldDef,
        PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityDescriptor, CapabilityDistribution, CapabilityMaturity,
        CapabilityRegistryAppExt, HitCandidate, ModelSummaryAccumulator, SnapPoint,
    },
    plugins::{
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        commands::{despawn_by_element_id, find_entity_by_element_id, CreateEntityCommand},
        cursor::{CursorWorldPos, DrawingPlane},
        egui_chrome::EguiWantsInput,
        face_edit::{face_vertices_for_entity, FaceEditContext},
        identity::{ElementId, ElementIdAllocator},
        inference::{InferenceEngine, ReferenceEdge},
        scene_ray,
        snap::SnapKind,
        snap::SnapResult,
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
        tools::ActiveTool,
        ui::StatusBarData,
    },
};

const GUIDE_LINE_COLOR: Color = Color::srgba(0.0, 0.8, 0.8, 0.7);
const GUIDE_LINE_SELECTED_COLOR: Color = Color::srgba(0.0, 1.0, 1.0, 0.9);
const GUIDE_LINE_HALF_LENGTH: f32 = 500.0;
const GUIDE_LINE_HIT_RADIUS: f32 = 0.15;
const GUIDE_ANCHOR_CROSS_SIZE: f32 = 0.12;
const GUIDE_LINE_MIN_DIRECTION_LENGTH: f32 = 0.01;
const GUIDE_EDGE_SNAP_RADIUS: f32 = 0.2;
const GUIDE_PROTRACTOR_SEGMENTS: usize = 32;
const GUIDE_PROTRACTOR_SCREEN_FACTOR: f32 = 0.08;
const GUIDE_PROTRACTOR_MIN_RADIUS: f32 = 0.45;
const GUIDE_PROTRACTOR_MAX_RADIUS: f32 = 2.0;
const GUIDE_PROTRACTOR_SNAP_INCREMENT_RADIANS: f32 = PI / 12.0;

pub struct GuideLinePlugin;

// ---------------------------------------------------------------------------
// ECS marker component
// ---------------------------------------------------------------------------

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct GuideLineNode {
    pub anchor: Vec3,
    pub direction: Vec3,
    pub finite_length: Option<f32>,
    pub visible: bool,
    pub label: Option<String>,
}

impl Default for GuideLineNode {
    fn default() -> Self {
        Self {
            anchor: Vec3::ZERO,
            direction: Vec3::X,
            finite_length: None,
            visible: true,
            label: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Global visibility toggle resource
// ---------------------------------------------------------------------------

#[derive(Resource, Debug, Clone)]
pub struct GuideLineVisibility {
    pub show_all: bool,
}

impl Default for GuideLineVisibility {
    fn default() -> Self {
        Self { show_all: true }
    }
}

#[derive(Resource, Default, Clone)]
struct GuideLineToolState {
    anchor: Option<Vec3>,
    host_plane: Option<DrawingPlane>,
    reference_direction: Option<Vec3>,
    reference_label: Option<String>,
    axis_lock: GuideAxisLock,
    numeric_buffer: Option<String>,
    hover_position: Option<Vec3>,
    hover_reference_direction: Option<Vec3>,
    hover_reference_label: Option<String>,
}

impl GuideLineToolState {
    fn clear_session(&mut self) {
        self.anchor = None;
        self.host_plane = None;
        self.reference_direction = None;
        self.reference_label = None;
        self.numeric_buffer = None;
        self.hover_position = None;
        self.hover_reference_direction = None;
        self.hover_reference_label = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GuideAxisLock {
    #[default]
    None,
    X,
    Y,
    Z,
}

#[derive(Debug, Clone)]
struct HoverGuideState {
    position: Option<Vec3>,
    host_plane: DrawingPlane,
    reference_direction: Option<Vec3>,
    reference_label: Option<String>,
}

#[derive(Debug, Clone)]
struct EdgeReference {
    snapped_point: Vec3,
    direction: Vec3,
    distance: f32,
    label: String,
}

#[derive(Debug, Clone)]
struct GuidePreviewState {
    anchor: Vec3,
    direction: Vec3,
    host_plane: DrawingPlane,
    base_direction: Option<Vec3>,
    angle_degrees: Option<f32>,
}

// ---------------------------------------------------------------------------
// Snapshot (AuthoredEntity impl)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GuideLineSnapshot {
    pub element_id: ElementId,
    pub anchor: Vec3,
    pub direction: Vec3,
    pub finite_length: Option<f32>,
    pub visible: bool,
    pub label: Option<String>,
}

impl GuideLineSnapshot {
    fn display_label(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| "Guide Line".to_string())
    }

    fn effective_endpoints(&self) -> (Vec3, Vec3) {
        let half = self.finite_length.unwrap_or(GUIDE_LINE_HALF_LENGTH);
        let dir = self.direction.normalize_or_zero();
        (self.anchor - dir * half, self.anchor + dir * half)
    }
}

impl AuthoredEntity for GuideLineSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "guide_line"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.display_label()
    }

    fn center(&self) -> Vec3 {
        self.anchor
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut s = self.clone();
        s.anchor += delta;
        s.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut s = self.clone();
        s.anchor = rotation * s.anchor;
        s.direction = rotation * s.direction;
        s.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut s = self.clone();
        s.anchor = center + (s.anchor - center) * factor;
        if let Some(length) = s.finite_length {
            let scale_along = factor.dot(s.direction.normalize_or_zero().abs());
            s.finite_length = Some(length * scale_along.max(0.001));
        }
        s.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let mut fields = vec![
            property_field(
                "anchor",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.anchor)),
            ),
            property_field(
                "direction",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.direction)),
            ),
            property_field(
                "visible",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.visible.to_string())),
            ),
        ];
        if let Some(length) = self.finite_length {
            fields.push(property_field(
                "length",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(length)),
            ));
        }
        if let Some(label) = &self.label {
            fields.push(property_field_with(
                "label",
                "Label",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(label.clone())),
                true,
            ));
        }
        fields
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut s = self.clone();
        match property_name {
            "anchor" => s.anchor = vec3_from_json(value)?,
            "direction" => {
                let dir = vec3_from_json(value)?;
                if dir.length_squared() < 1e-8 {
                    return Err("Direction must be non-zero".to_string());
                }
                s.direction = dir.normalize();
            }
            "length" => {
                let v = scalar_from_json(value)?;
                s.finite_length = if v <= 0.0 { None } else { Some(v) };
            }
            "visible" => {
                s.visible = match value.as_bool() {
                    Some(b) => b,
                    None => match value.as_str() {
                        Some("true") | Some("1") => true,
                        Some("false") | Some("0") => false,
                        _ => return Err("visible must be true or false".to_string()),
                    },
                };
            }
            "label" => {
                s.label = value
                    .as_str()
                    .map(|text| {
                        if text.is_empty() {
                            None
                        } else {
                            Some(text.to_string())
                        }
                    })
                    .unwrap_or(None);
            }
            _ => {
                return Err(invalid_property_error(
                    "guide_line",
                    &["anchor", "direction", "length", "visible", "label"],
                ));
            }
        }
        Ok(s.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        let (start, end) = self.effective_endpoints();
        vec![
            HandleInfo {
                id: "anchor".to_string(),
                position: self.anchor,
                kind: HandleKind::Center,
                label: "Anchor".to_string(),
            },
            HandleInfo {
                id: "start".to_string(),
                position: start,
                kind: HandleKind::Vertex,
                label: "Start".to_string(),
            },
            HandleInfo {
                id: "end".to_string(),
                position: end,
                kind: HandleKind::Vertex,
                label: "End".to_string(),
            },
        ]
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        match handle_id {
            "anchor" => {
                let mut s = self.clone();
                s.anchor = cursor;
                Some(s.into())
            }
            _ => None,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "element_id": self.element_id,
            "anchor": self.anchor.to_array(),
            "direction": self.direction.to_array(),
            "finite_length": self.finite_length,
            "visible": self.visible,
            "label": self.label,
        })
    }

    fn apply_to(&self, world: &mut World) {
        let node = GuideLineNode {
            anchor: self.anchor,
            direction: self.direction,
            finite_length: self.finite_length,
            visible: self.visible,
            label: self.label.clone(),
        };
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert(node);
        } else {
            world.spawn((self.element_id, node));
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_guide_line_gizmo(
            gizmos,
            &self.anchor,
            &self.direction,
            self.finite_length,
            color,
        );
    }

    fn preview_line_count(&self) -> usize {
        3 // main line + anchor cross (2 arms)
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == "guide_line" && other.to_json() == self.to_json()
    }
}

impl From<GuideLineSnapshot> for BoxedEntity {
    fn from(snapshot: GuideLineSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct GuideLineFactory;

impl AuthoredEntityFactory for GuideLineFactory {
    fn type_name(&self) -> &'static str {
        "guide_line"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<GuideLineNode>()?;
        Some(
            GuideLineSnapshot {
                element_id,
                anchor: node.anchor,
                direction: node.direction,
                finite_length: node.finite_length,
                visible: node.visible,
                label: node.label.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id: ElementId = serde_json::from_value(
            data.get("element_id")
                .cloned()
                .ok_or_else(|| "Missing element_id".to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let anchor = data
            .get("anchor")
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::ZERO);
        let direction = data
            .get("direction")
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::X)
            .try_normalize()
            .unwrap_or(Vec3::X);
        let finite_length = data
            .get("finite_length")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        let visible = data
            .get("visible")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let label = data.get("label").and_then(|v| v.as_str()).map(String::from);
        Ok(GuideLineSnapshot {
            element_id,
            anchor,
            direction,
            finite_length,
            visible,
            label,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let obj = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let element_id = world
            .get_resource::<ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();
        let anchor = obj
            .get("anchor")
            .or_else(|| obj.get("position"))
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::ZERO);
        let direction = obj.get("direction").map(vec3_from_json).transpose()?;
        let through = obj.get("through").map(vec3_from_json).transpose()?;
        let reference_direction = obj
            .get("reference_direction")
            .map(vec3_from_json)
            .transpose()?;
        let angle_degrees = obj
            .get("angle_degrees")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        let plane_normal = obj
            .get("plane_normal")
            .or_else(|| obj.get("host_normal"))
            .map(vec3_from_json)
            .transpose()?;
        let direction = resolve_guide_request_direction(
            anchor,
            direction,
            through,
            reference_direction,
            angle_degrees,
            plane_normal,
        )?;
        let finite_length = obj
            .get("finite_length")
            .or_else(|| obj.get("length"))
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        let visible = obj.get("visible").and_then(|v| v.as_bool()).unwrap_or(true);
        let label = obj
            .get("label")
            .or_else(|| obj.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(GuideLineSnapshot {
            element_id,
            anchor,
            direction,
            finite_length,
            visible,
            label,
        }
        .into())
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let visibility = world.get_resource::<GuideLineVisibility>()?;
        if !visibility.show_all {
            return None;
        }
        let mut query = world.try_query::<(Entity, &ElementId, &GuideLineNode)>()?;
        query
            .iter(world)
            .filter(|(_, _, node)| node.visible)
            .filter_map(|(entity, _element_id, node)| {
                let half = node.finite_length.unwrap_or(GUIDE_LINE_HALF_LENGTH);
                let dir = node.direction.normalize_or_zero();
                let start = node.anchor - dir * half;
                let end = node.anchor + dir * half;
                let dist = ray_segment_distance(ray.origin, ray.direction.into(), start, end)?;
                if dist > GUIDE_LINE_HIT_RADIUS {
                    return None;
                }
                let t = (node.anchor - ray.origin).dot(ray.direction.into());
                if t < 0.0 {
                    return None;
                }
                Some(HitCandidate {
                    entity,
                    distance: t,
                })
            })
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
    }

    fn collect_snap_points(&self, world: &World, out: &mut Vec<SnapPoint>) {
        let Some(visibility) = world.get_resource::<GuideLineVisibility>() else {
            return;
        };
        if !visibility.show_all {
            return;
        }
        let Some(mut query) = world.try_query::<&GuideLineNode>() else {
            return;
        };
        for node in query.iter(world) {
            if !node.visible {
                continue;
            }
            out.push(SnapPoint {
                position: node.anchor,
                kind: SnapKind::GuideAnchor,
            });
            if let Some(length) = node.finite_length {
                let dir = node.direction.normalize_or_zero();
                out.push(SnapPoint {
                    position: node.anchor - dir * length,
                    kind: SnapKind::Endpoint,
                });
                out.push(SnapPoint {
                    position: node.anchor + dir * length,
                    kind: SnapKind::Endpoint,
                });
            }
        }
    }

    fn collect_inference_geometry(&self, world: &World, engine: &mut InferenceEngine) {
        let Some(visibility) = world.get_resource::<GuideLineVisibility>() else {
            return;
        };
        if !visibility.show_all {
            return;
        }
        let Some(mut query) = world.try_query::<(&ElementId, &GuideLineNode)>() else {
            return;
        };
        for (element_id, node) in query.iter(world) {
            if !node.visible {
                continue;
            }
            let half = node.finite_length.unwrap_or(GUIDE_LINE_HALF_LENGTH);
            let dir = node.direction.normalize_or_zero();
            engine.add_reference_edge(ReferenceEdge {
                start: node.anchor - dir * half,
                end: node.anchor + dir * half,
                entity_label: node
                    .label
                    .clone()
                    .unwrap_or_else(|| "Guide Line".to_string()),
                element_id: *element_id,
            });
        }
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let Some(mut query) = world.try_query::<&GuideLineNode>() else {
            return;
        };
        for _ in query.iter(world) {
            *summary
                .entity_counts
                .entry("guide_line".to_string())
                .or_default() += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Gizmo drawing
// ---------------------------------------------------------------------------

fn draw_guide_line_gizmo(
    gizmos: &mut Gizmos,
    anchor: &Vec3,
    direction: &Vec3,
    finite_length: Option<f32>,
    color: Color,
) {
    let dir = direction.normalize_or_zero();
    let half = finite_length.unwrap_or(GUIDE_LINE_HALF_LENGTH);
    let start = *anchor - dir * half;
    let end = *anchor + dir * half;
    gizmos.line(start, end, color);

    // Anchor cross glyph — two short perpendicular lines
    let (perp1, perp2) = perpendicular_pair(dir);
    gizmos.line(
        *anchor - perp1 * GUIDE_ANCHOR_CROSS_SIZE,
        *anchor + perp1 * GUIDE_ANCHOR_CROSS_SIZE,
        color,
    );
    gizmos.line(
        *anchor - perp2 * GUIDE_ANCHOR_CROSS_SIZE,
        *anchor + perp2 * GUIDE_ANCHOR_CROSS_SIZE,
        color,
    );
}

fn draw_guide_line_visuals(
    visibility: Res<GuideLineVisibility>,
    guides: Query<(Entity, &GuideLineNode), Without<crate::plugins::selection::Selected>>,
    selected_guides: Query<(Entity, &GuideLineNode), With<crate::plugins::selection::Selected>>,
    mut gizmos: Gizmos,
) {
    if !visibility.show_all {
        return;
    }
    for (_entity, node) in &guides {
        if !node.visible {
            continue;
        }
        draw_guide_line_gizmo(
            &mut gizmos,
            &node.anchor,
            &node.direction,
            node.finite_length,
            GUIDE_LINE_COLOR,
        );
    }
    for (_entity, node) in &selected_guides {
        if !node.visible {
            continue;
        }
        draw_guide_line_gizmo(
            &mut gizmos,
            &node.anchor,
            &node.direction,
            node.finite_length,
            GUIDE_LINE_SELECTED_COLOR,
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn perpendicular_pair(dir: Vec3) -> (Vec3, Vec3) {
    let candidate = if dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let perp1 = dir.cross(candidate).normalize_or_zero();
    let perp2 = dir.cross(perp1).normalize_or_zero();
    (perp1, perp2)
}

fn host_plane_from_context(
    drawing_plane: &DrawingPlane,
    face_context: &FaceEditContext,
) -> DrawingPlane {
    face_context
        .selected_face
        .as_ref()
        .map(|face| DrawingPlane::from_face(face.centroid, face.normal))
        .unwrap_or_else(|| drawing_plane.clone())
}

fn project_point_to_plane(point: Vec3, plane: &DrawingPlane) -> Vec3 {
    point - plane.normal * (point - plane.origin).dot(plane.normal)
}

fn project_direction_to_plane(direction: Vec3, plane_normal: Vec3) -> Option<Vec3> {
    let rejected = direction - plane_normal * direction.dot(plane_normal);
    rejected.try_normalize()
}

fn selected_guide_reference(world: &mut World) -> Option<(Vec3, String)> {
    let mut query =
        world.query_filtered::<&GuideLineNode, With<crate::plugins::selection::Selected>>();
    query.iter(world).next().map(|node| {
        (
            node.direction.normalize_or_zero(),
            node.label
                .clone()
                .unwrap_or_else(|| "Guide Line".to_string()),
        )
    })
}

fn nearest_face_edge_reference(
    world: &World,
    entity: Entity,
    face_id: crate::capability_registry::FaceId,
    probe: Vec3,
) -> Option<EdgeReference> {
    let verts = face_vertices_for_entity(world, entity, face_id)?;
    let mut best: Option<EdgeReference> = None;
    for index in 0..verts.len() {
        let start = verts[index];
        let end = verts[(index + 1) % verts.len()];
        let delta = end - start;
        let Some(direction) = delta.try_normalize() else {
            continue;
        };
        let t = (probe - start).dot(delta) / delta.length_squared();
        let snapped_point = start + delta * t.clamp(0.0, 1.0);
        let candidate = EdgeReference {
            snapped_point,
            direction,
            distance: probe.distance(snapped_point),
            label: "Face edge".to_string(),
        };
        let replace = best
            .as_ref()
            .map(|best| candidate.distance < best.distance)
            .unwrap_or(true);
        if replace {
            best = Some(candidate);
        }
    }
    best
}

fn compute_hover_guide_state(world: &mut World) -> HoverGuideState {
    let drawing_plane = world.resource::<DrawingPlane>().clone();
    let face_context = world.resource::<FaceEditContext>().clone();
    let cursor_world_pos = world.resource::<CursorWorldPos>();
    let snap_result = world.resource::<SnapResult>().clone();
    let fallback_plane = host_plane_from_context(&drawing_plane, &face_context);
    let base_position = snap_result
        .position
        .or(cursor_world_pos.snapped)
        .or(cursor_world_pos.raw);

    let mut edge_reference = None;
    let mut host_plane = fallback_plane.clone();

    if let (Some(entity), Some(selected_face), Some(base_position)) = (
        face_context.entity,
        face_context.selected_face.as_ref(),
        base_position,
    ) {
        host_plane = DrawingPlane::from_face(selected_face.centroid, selected_face.normal);
        edge_reference = nearest_face_edge_reference(
            world,
            entity,
            selected_face.face_id,
            project_point_to_plane(base_position, &host_plane),
        );
    } else if let Some(ray) = scene_ray::build_camera_ray(world) {
        if let Some(face_hit) = scene_ray::ray_cast_nearest_face(world, ray) {
            host_plane = DrawingPlane::from_face(face_hit.centroid, face_hit.normal);
            edge_reference = base_position.and_then(|base_position| {
                nearest_face_edge_reference(
                    world,
                    face_hit.entity,
                    face_hit.face_id,
                    project_point_to_plane(base_position, &host_plane),
                )
            });
        }
    }

    let position = if snap_result.target.is_some() {
        base_position.map(|position| project_point_to_plane(position, &host_plane))
    } else if let Some(edge_reference) = edge_reference.as_ref() {
        if edge_reference.distance <= GUIDE_EDGE_SNAP_RADIUS {
            Some(edge_reference.snapped_point)
        } else {
            base_position.map(|position| project_point_to_plane(position, &host_plane))
        }
    } else {
        base_position.map(|position| project_point_to_plane(position, &host_plane))
    };

    let selected_reference = selected_guide_reference(world);
    HoverGuideState {
        position,
        host_plane,
        reference_direction: edge_reference
            .as_ref()
            .map(|edge_reference| edge_reference.direction)
            .or_else(|| selected_reference.as_ref().map(|(direction, _)| *direction)),
        reference_label: edge_reference
            .as_ref()
            .map(|edge_reference| edge_reference.label.clone())
            .or_else(|| selected_reference.as_ref().map(|(_, label)| label.clone())),
    }
}

fn toggle_axis_lock(current: GuideAxisLock, axis: GuideAxisLock) -> GuideAxisLock {
    if current == axis {
        GuideAxisLock::None
    } else {
        axis
    }
}

fn axis_lock_label(axis_lock: GuideAxisLock) -> &'static str {
    match axis_lock {
        GuideAxisLock::None => "",
        GuideAxisLock::X => " X",
        GuideAxisLock::Y => " Y",
        GuideAxisLock::Z => " Z",
    }
}

fn axis_lock_direction(axis_lock: GuideAxisLock, host_plane: &DrawingPlane) -> Option<Vec3> {
    let axis = match axis_lock {
        GuideAxisLock::None => return None,
        GuideAxisLock::X => Vec3::X,
        GuideAxisLock::Y => Vec3::Y,
        GuideAxisLock::Z => Vec3::Z,
    };
    project_direction_to_plane(axis, host_plane.normal).or_else(|| {
        let fallback = if axis_lock == GuideAxisLock::Y {
            host_plane.bitangent
        } else {
            host_plane.tangent
        };
        fallback.try_normalize()
    })
}

fn push_numeric_char(buffer: &mut Option<String>, character: char) {
    buffer.get_or_insert_with(String::new).push(character);
}

fn push_minus(buffer: &mut Option<String>) {
    let value = buffer.get_or_insert_with(String::new);
    if value.starts_with('-') {
        value.remove(0);
    } else {
        value.insert(0, '-');
    }
}

fn pop_numeric_char(buffer: &mut Option<String>) {
    let Some(value) = buffer.as_mut() else {
        return;
    };
    value.pop();
    if value.is_empty() || value == "-" {
        *buffer = None;
    }
}

fn parse_numeric_angle_degrees(buffer: &Option<String>) -> Option<f32> {
    buffer.as_deref()?.parse::<f32>().ok()
}

fn normalize_angle(angle_radians: f32) -> f32 {
    let mut angle = angle_radians;
    while angle > PI {
        angle -= PI * 2.0;
    }
    while angle < -PI {
        angle += PI * 2.0;
    }
    angle
}

fn snap_angle(angle_radians: f32, increment_radians: f32) -> f32 {
    (angle_radians / increment_radians).round() * increment_radians
}

fn signed_angle_on_plane(base_direction: Vec3, current_direction: Vec3, plane_normal: Vec3) -> f32 {
    let base_direction = base_direction.normalize_or_zero();
    let current_direction = current_direction.normalize_or_zero();
    let sin = base_direction.cross(current_direction).dot(plane_normal);
    let cos = base_direction.dot(current_direction).clamp(-1.0, 1.0);
    sin.atan2(cos)
}

fn resolve_preview_state(
    tool_state: &GuideLineToolState,
    drawing_plane: &DrawingPlane,
    control_pressed: bool,
) -> Option<GuidePreviewState> {
    let anchor = tool_state.anchor?;
    let host_plane = tool_state
        .host_plane
        .clone()
        .unwrap_or_else(|| drawing_plane.clone());
    let hover_position = project_point_to_plane(tool_state.hover_position?, &host_plane);

    if let Some(axis_direction) = axis_lock_direction(tool_state.axis_lock, &host_plane) {
        return Some(GuidePreviewState {
            anchor,
            direction: axis_direction,
            host_plane,
            base_direction: None,
            angle_degrees: None,
        });
    }

    let projected_direction =
        project_direction_to_plane(hover_position - anchor, host_plane.normal)?;
    if projected_direction.length_squared()
        < GUIDE_LINE_MIN_DIRECTION_LENGTH * GUIDE_LINE_MIN_DIRECTION_LENGTH
    {
        return None;
    }

    let base_direction = tool_state
        .reference_direction
        .or(tool_state.hover_reference_direction)
        .and_then(|direction| project_direction_to_plane(direction, host_plane.normal))
        .or_else(|| host_plane.tangent.try_normalize());

    let numeric_angle_radians =
        parse_numeric_angle_degrees(&tool_state.numeric_buffer).map(|degrees| degrees.to_radians());
    let use_protractor = base_direction.is_some()
        && (numeric_angle_radians.is_some()
            || control_pressed
            || tool_state.reference_direction.is_some());

    if let (true, Some(base_direction)) = (use_protractor, base_direction) {
        let mut angle_radians = numeric_angle_radians.unwrap_or_else(|| {
            signed_angle_on_plane(base_direction, projected_direction, host_plane.normal)
        });
        if control_pressed && numeric_angle_radians.is_none() {
            angle_radians = snap_angle(angle_radians, GUIDE_PROTRACTOR_SNAP_INCREMENT_RADIANS);
        }
        let direction = Quat::from_axis_angle(host_plane.normal, angle_radians)
            * base_direction.normalize_or_zero();
        return Some(GuidePreviewState {
            anchor,
            direction: direction.normalize_or_zero(),
            host_plane,
            base_direction: Some(base_direction),
            angle_degrees: Some(angle_radians.to_degrees()),
        });
    }

    Some(GuidePreviewState {
        anchor,
        direction: projected_direction.normalize_or_zero(),
        host_plane,
        base_direction: None,
        angle_degrees: None,
    })
}

fn guide_tool_hint(
    tool_state: &GuideLineToolState,
    drawing_plane: &DrawingPlane,
    control_pressed: bool,
) -> String {
    if tool_state.anchor.is_none() {
        return "Click an anchor point or edge · X/Y/Z lock · Drag from face edit to host on the selected face"
            .to_string();
    }

    let preview = resolve_preview_state(tool_state, drawing_plane, control_pressed);
    let mut parts = vec![
        "Click to place guide".to_string(),
        "type angle + Enter".to_string(),
        "Ctrl snaps protractor".to_string(),
        format!("X/Y/Z lock{}", axis_lock_label(tool_state.axis_lock)),
    ];
    if let Some(label) = tool_state
        .reference_label
        .as_ref()
        .or(tool_state.hover_reference_label.as_ref())
    {
        parts.push(format!("ref {label}"));
    }
    if let Some(angle_degrees) = preview.and_then(|preview| preview.angle_degrees) {
        parts.push(format!("angle {angle_degrees:.1}°"));
    }
    if let Some(buffer) = tool_state.numeric_buffer.as_ref() {
        parts.push(format!("typed {buffer}°"));
    }
    parts.join(" · ")
}

fn resolve_guide_request_direction(
    anchor: Vec3,
    direction: Option<Vec3>,
    through: Option<Vec3>,
    reference_direction: Option<Vec3>,
    angle_degrees: Option<f32>,
    plane_normal: Option<Vec3>,
) -> Result<Vec3, String> {
    if let Some(direction) = direction {
        return direction
            .try_normalize()
            .ok_or_else(|| "direction must be non-zero".to_string());
    }

    if let Some(through) = through {
        let direction = through - anchor;
        return direction
            .try_normalize()
            .ok_or_else(|| "through point must differ from anchor".to_string());
    }

    let angle_degrees = angle_degrees
        .ok_or_else(|| "guide_line requires direction, through, or angle_degrees".to_string())?;
    let plane_normal = plane_normal
        .and_then(|normal| normal.try_normalize())
        .unwrap_or(Vec3::Y);
    let base_direction = reference_direction
        .and_then(|direction| project_direction_to_plane(direction, plane_normal))
        .unwrap_or_else(|| {
            let plane = DrawingPlane::from_face(anchor, plane_normal);
            plane.tangent
        });
    let rotated = Quat::from_axis_angle(plane_normal, angle_degrees.to_radians()) * base_direction;
    rotated
        .try_normalize()
        .ok_or_else(|| "could not resolve guide direction from angle request".to_string())
}

fn ray_segment_distance(
    ray_origin: Vec3,
    ray_dir: Vec3,
    seg_start: Vec3,
    seg_end: Vec3,
) -> Option<f32> {
    let d = seg_end - seg_start;
    let w = ray_origin - seg_start;
    let a = ray_dir.dot(ray_dir);
    let b = ray_dir.dot(d);
    let c = d.dot(d);
    let denom = a * c - b * b;
    if denom.abs() < 1e-10 {
        return None; // parallel
    }
    let e_val = ray_dir.dot(w);
    let f_val = d.dot(w);
    let mut s = (b * f_val - c * e_val) / denom;
    let mut t = (a * f_val - b * e_val) / denom;
    s = s.max(0.0);
    t = t.clamp(0.0, 1.0);
    let closest_on_ray = ray_origin + ray_dir * s;
    let closest_on_seg = seg_start + d * t;
    Some(closest_on_ray.distance(closest_on_seg))
}

// ---------------------------------------------------------------------------
// Command: toggle visibility
// ---------------------------------------------------------------------------

fn execute_toggle_guide_line_visibility(
    world: &mut World,
    _params: &Value,
) -> Result<CommandResult, String> {
    let mut vis = world.resource_mut::<GuideLineVisibility>();
    vis.show_all = !vis.show_all;
    Ok(CommandResult::empty())
}

fn execute_place_guide_line(world: &mut World, _params: &Value) -> Result<CommandResult, String> {
    world
        .resource_mut::<NextState<ActiveTool>>()
        .set(ActiveTool::PlaceGuideLine);
    Ok(CommandResult::empty())
}

fn initialize_guide_line_tool(mut commands: Commands) {
    commands.insert_resource(GuideLineToolState::default());
}

fn cleanup_guide_line_tool(mut commands: Commands) {
    commands.remove_resource::<GuideLineToolState>();
}

fn handle_guide_line_tool(world: &mut World) {
    let egui_wants_input = world.resource::<EguiWantsInput>().clone();
    let just_pressed_keys: Vec<KeyCode> = world
        .resource::<ButtonInput<KeyCode>>()
        .get_just_pressed()
        .copied()
        .collect();
    let mouse_left_pressed = world
        .resource::<ButtonInput<MouseButton>>()
        .just_pressed(MouseButton::Left);
    let control_pressed = {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    };

    if !egui_wants_input.keyboard {
        let escape_pressed = just_pressed_keys.contains(&KeyCode::Escape);
        if escape_pressed {
            world
                .resource_mut::<NextState<ActiveTool>>()
                .set(ActiveTool::Select);
            world.resource_mut::<StatusBarData>().hint.clear();
            return;
        }
    }

    let hover_state = if egui_wants_input.pointer {
        let drawing_plane = world.resource::<DrawingPlane>().clone();
        let face_context = world.resource::<FaceEditContext>().clone();
        HoverGuideState {
            position: None,
            host_plane: host_plane_from_context(&drawing_plane, &face_context),
            reference_direction: selected_guide_reference(world).map(|(direction, _)| direction),
            reference_label: selected_guide_reference(world).map(|(_, label)| label),
        }
    } else {
        compute_hover_guide_state(world)
    };

    {
        let mut tool_state = world.resource_mut::<GuideLineToolState>();
        tool_state.hover_position = hover_state.position;
        tool_state.hover_reference_direction = hover_state.reference_direction;
        tool_state.hover_reference_label = hover_state.reference_label.clone();
    }

    if !egui_wants_input.keyboard {
        let mut tool_state = world.resource_mut::<GuideLineToolState>();
        for key in just_pressed_keys {
            match key {
                KeyCode::KeyX => {
                    tool_state.axis_lock = toggle_axis_lock(tool_state.axis_lock, GuideAxisLock::X)
                }
                KeyCode::KeyY => {
                    tool_state.axis_lock = toggle_axis_lock(tool_state.axis_lock, GuideAxisLock::Y)
                }
                KeyCode::KeyZ => {
                    tool_state.axis_lock = toggle_axis_lock(tool_state.axis_lock, GuideAxisLock::Z)
                }
                KeyCode::Digit0 | KeyCode::Numpad0 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '0')
                }
                KeyCode::Digit1 | KeyCode::Numpad1 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '1')
                }
                KeyCode::Digit2 | KeyCode::Numpad2 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '2')
                }
                KeyCode::Digit3 | KeyCode::Numpad3 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '3')
                }
                KeyCode::Digit4 | KeyCode::Numpad4 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '4')
                }
                KeyCode::Digit5 | KeyCode::Numpad5 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '5')
                }
                KeyCode::Digit6 | KeyCode::Numpad6 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '6')
                }
                KeyCode::Digit7 | KeyCode::Numpad7 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '7')
                }
                KeyCode::Digit8 | KeyCode::Numpad8 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '8')
                }
                KeyCode::Digit9 | KeyCode::Numpad9 => {
                    push_numeric_char(&mut tool_state.numeric_buffer, '9')
                }
                KeyCode::Period | KeyCode::NumpadDecimal | KeyCode::NumpadComma => {
                    if !tool_state
                        .numeric_buffer
                        .as_deref()
                        .unwrap_or_default()
                        .contains('.')
                    {
                        push_numeric_char(&mut tool_state.numeric_buffer, '.');
                    }
                }
                KeyCode::Minus | KeyCode::NumpadSubtract => {
                    push_minus(&mut tool_state.numeric_buffer)
                }
                KeyCode::Backspace | KeyCode::NumpadBackspace => {
                    pop_numeric_char(&mut tool_state.numeric_buffer)
                }
                _ => {}
            }
        }
    }

    let commit_with_enter = !egui_wants_input.keyboard
        && (world
            .resource::<ButtonInput<KeyCode>>()
            .just_pressed(KeyCode::Enter)
            || world
                .resource::<ButtonInput<KeyCode>>()
                .just_pressed(KeyCode::NumpadEnter));
    let preview_state = {
        let tool_state = world.resource::<GuideLineToolState>().clone();
        let drawing_plane = world.resource::<DrawingPlane>().clone();
        resolve_preview_state(&tool_state, &drawing_plane, control_pressed)
    };

    if !egui_wants_input.pointer && (mouse_left_pressed || commit_with_enter) {
        let next_element_id = world.resource::<ElementIdAllocator>().next_id();
        let mut pending_snapshot = None;
        {
            let mut tool_state = world.resource_mut::<GuideLineToolState>();
            match tool_state.anchor {
                None => {
                    let Some(anchor) = hover_state.position else {
                        return;
                    };
                    tool_state.anchor = Some(anchor);
                    tool_state.host_plane = Some(hover_state.host_plane.clone());
                    tool_state.reference_direction =
                        hover_state.reference_direction.and_then(|direction| {
                            project_direction_to_plane(direction, hover_state.host_plane.normal)
                        });
                    tool_state.reference_label = hover_state.reference_label.clone();
                }
                Some(anchor) => {
                    let Some(preview_state) = preview_state else {
                        return;
                    };
                    if preview_state.direction.length_squared()
                        < GUIDE_LINE_MIN_DIRECTION_LENGTH * GUIDE_LINE_MIN_DIRECTION_LENGTH
                    {
                        return;
                    }

                    pending_snapshot = Some(GuideLineSnapshot {
                        element_id: next_element_id,
                        anchor,
                        direction: preview_state.direction.normalize_or_zero(),
                        finite_length: None,
                        visible: true,
                        label: None,
                    });
                    tool_state.clear_session();
                }
            }
        }
        if let Some(snapshot) = pending_snapshot {
            world.write_message(CreateEntityCommand {
                snapshot: snapshot.into(),
            });
        }
    }

    let hint = {
        let tool_state = world.resource::<GuideLineToolState>().clone();
        let drawing_plane = world.resource::<DrawingPlane>().clone();
        guide_tool_hint(&tool_state, &drawing_plane, control_pressed)
    };
    world.resource_mut::<StatusBarData>().hint = hint;
}

fn draw_guide_line_tool_preview(
    guide_line_tool_state: Option<Res<GuideLineToolState>>,
    drawing_plane: Res<DrawingPlane>,
    keys: Res<ButtonInput<KeyCode>>,
    camera_query: Query<&GlobalTransform, With<Camera>>,
    mut gizmos: Gizmos,
) {
    let Some(guide_line_tool_state) = guide_line_tool_state else {
        return;
    };
    let control_pressed = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let Some(preview) =
        resolve_preview_state(&guide_line_tool_state, &drawing_plane, control_pressed)
    else {
        return;
    };

    draw_guide_line_gizmo(
        &mut gizmos,
        &preview.anchor,
        &preview.direction,
        None,
        GUIDE_LINE_SELECTED_COLOR,
    );

    if let (Some(base_direction), Some(angle_degrees)) =
        (preview.base_direction, preview.angle_degrees)
    {
        let radius = camera_query
            .iter()
            .next()
            .map(|camera_transform| {
                let camera_distance = (camera_transform.translation() - preview.anchor).length();
                (camera_distance * GUIDE_PROTRACTOR_SCREEN_FACTOR)
                    .clamp(GUIDE_PROTRACTOR_MIN_RADIUS, GUIDE_PROTRACTOR_MAX_RADIUS)
            })
            .unwrap_or(0.9);
        let start_direction = base_direction.normalize_or_zero();
        let end_direction = preview.direction.normalize_or_zero();
        gizmos.line(
            preview.anchor,
            preview.anchor + start_direction * radius,
            GUIDE_LINE_COLOR,
        );
        gizmos.line(
            preview.anchor,
            preview.anchor + end_direction * radius,
            GUIDE_LINE_SELECTED_COLOR,
        );
        let angle_radians = normalize_angle(angle_degrees.to_radians());
        let mut previous = None;
        for index in 0..=GUIDE_PROTRACTOR_SEGMENTS {
            let t = index as f32 / GUIDE_PROTRACTOR_SEGMENTS as f32;
            let angle = angle_radians * t;
            let direction =
                Quat::from_axis_angle(preview.host_plane.normal, angle) * start_direction;
            let point = preview.anchor + direction * radius;
            if let Some(previous) = previous {
                gizmos.line(previous, point, GUIDE_LINE_SELECTED_COLOR);
            }
            previous = Some(point);
        }
    }
}

fn draw_guide_line_tool_overlay(
    mut contexts: EguiContexts,
    guide_line_tool_state: Option<Res<GuideLineToolState>>,
    drawing_plane: Res<DrawingPlane>,
    keys: Res<ButtonInput<KeyCode>>,
    camera_query: Query<(&Camera, &GlobalTransform)>,
    window_query: Query<&Window, With<PrimaryWindow>>,
) {
    let Some(guide_line_tool_state) = guide_line_tool_state else {
        return;
    };
    let control_pressed = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let Some(preview) =
        resolve_preview_state(&guide_line_tool_state, &drawing_plane, control_pressed)
    else {
        return;
    };
    let Some(angle_degrees) = preview.angle_degrees else {
        return;
    };
    let Some(base_direction) = preview.base_direction else {
        return;
    };
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    let Ok(window) = window_query.single() else {
        return;
    };
    let Ok(ctx_ref) = contexts.ctx_mut() else {
        return;
    };
    let radius = (camera_transform.translation().distance(preview.anchor)
        * GUIDE_PROTRACTOR_SCREEN_FACTOR)
        .clamp(GUIDE_PROTRACTOR_MIN_RADIUS, GUIDE_PROTRACTOR_MAX_RADIUS);
    let label_direction = Quat::from_axis_angle(
        preview.host_plane.normal,
        normalize_angle(angle_degrees.to_radians()) * 0.5,
    ) * base_direction.normalize_or_zero();
    let label_point = preview.anchor + label_direction * (radius * 1.15);
    let Ok(screen_pos) = camera.world_to_viewport(camera_transform, label_point) else {
        return;
    };
    if screen_pos.x < 0.0
        || screen_pos.y < 0.0
        || screen_pos.x > window.width()
        || screen_pos.y > window.height()
    {
        return;
    }

    let painter = ctx_ref.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("guide_line_angle_overlay"),
    ));
    let text = if let Some(buffer) = guide_line_tool_state.numeric_buffer.as_ref() {
        format!("{buffer}°")
    } else {
        format!("{angle_degrees:.1}°")
    };
    let pos = egui::pos2(screen_pos.x, screen_pos.y);
    let rect = egui::Rect::from_center_size(
        pos,
        egui::vec2(text.chars().count() as f32 * 8.0 + 16.0, 24.0),
    );
    painter.rect_filled(rect, 5.0, egui::Color32::from_black_alpha(180));
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 220, 220)),
        egui::StrokeKind::Outside,
    );
    painter.text(
        pos,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

impl Plugin for GuideLinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GuideLineVisibility>()
            .register_authored_entity_factory(GuideLineFactory)
            .register_capability(CapabilityDescriptor {
                id: "guide_lines".to_string(),
                name: "Guide Lines".to_string(),
                version: 1,
                api_version: crate::capability_registry::CAPABILITY_API_VERSION,
                description: Some(
                    "Construction reference lines for precision modeling. Place guide lines from anchor points with direction vectors; snap and inference integrate automatically."
                        .to_string(),
                ),
                dependencies: vec![],
                optional_dependencies: vec![],
                conflicts: vec![],
                maturity: CapabilityMaturity::Stable,
                distribution: CapabilityDistribution::Bundled,
                license: None,
                repository: None,
            })
            .register_command(
                CommandDescriptor {
                    id: "guide_lines.place".to_string(),
                    label: "Guide Line".to_string(),
                    description: "Activate guide line placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: None,
                    default_shortcut: Some("G".to_string()),
                    icon: Some("icon.guide_line".to_string()),
                    hint: Some(
                        "Click anchor or edge, then drag/click to place the guide · X/Y/Z locks axis · type angle + Enter"
                            .to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlaceGuideLine".to_string()),
                    capability_id: Some(crate::plugins::drawing_export::DRAFTING_CAPABILITY_ID.to_string()),
                },
                execute_place_guide_line,
            )
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_guide_lines".to_string(),
                    label: "Guide Lines".to_string(),
                    description: "Show or hide all guide lines".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Shift+G".to_string()),
                    icon: Some("icon.guide_lines".to_string()),
                    hint: Some("Toggle visibility of construction guide lines".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(crate::plugins::drawing_export::DRAFTING_CAPABILITY_ID.to_string()),
                },
                execute_toggle_guide_line_visibility,
            )
            .register_toolbar(ToolbarDescriptor {
                id: "reference_geometry".to_string(),
                label: "Reference".to_string(),
                default_dock: ToolbarDock::Left,
                default_visible: true,
                sections: vec![ToolbarSection {
                    label: "Construction".to_string(),
                    command_ids: vec![
                        "guide_lines.place".to_string(),
                        "view.toggle_guide_lines".to_string(),
                    ],
                }],
            })
            .add_systems(OnEnter(ActiveTool::PlaceGuideLine), initialize_guide_line_tool)
            .add_systems(OnExit(ActiveTool::PlaceGuideLine), cleanup_guide_line_tool)
            .add_systems(
                Update,
                (
                    handle_guide_line_tool,
                    draw_guide_line_tool_preview,
                    draw_guide_line_tool_overlay,
                    draw_guide_line_visuals,
                )
                    .run_if(in_state(ActiveTool::PlaceGuideLine)),
            )
            .add_systems(
                Update,
                draw_guide_line_visuals.run_if(not(in_state(ActiveTool::PlaceGuideLine))),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::AuthoredEntityFactory;

    #[test]
    fn create_request_normalizes_direction_and_supports_length_alias() {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());

        let snapshot = GuideLineFactory
            .from_create_request(
                &world,
                &json!({
                    "type": "guide_line",
                    "anchor": [1.0, 2.0, 3.0],
                    "direction": [3.0, 0.0, 4.0],
                    "length": 2.5,
                    "visible": false,
                    "label": "Axis A"
                }),
            )
            .expect("guide line request should parse");

        let snapshot = snapshot
            .0
            .as_any()
            .downcast_ref::<GuideLineSnapshot>()
            .expect("snapshot should downcast");
        assert_eq!(snapshot.anchor, Vec3::new(1.0, 2.0, 3.0));
        assert!((snapshot.direction.length() - 1.0).abs() < 1e-5);
        assert_eq!(snapshot.finite_length, Some(2.5));
        assert!(!snapshot.visible);
        assert_eq!(snapshot.label.as_deref(), Some("Axis A"));
    }

    #[test]
    fn create_request_supports_through_point_and_reference_angle() {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());

        let through_snapshot = GuideLineFactory
            .from_create_request(
                &world,
                &json!({
                    "type": "guide_line",
                    "anchor": [1.0, 0.0, 1.0],
                    "through": [1.0, 0.0, 4.0],
                }),
            )
            .expect("through-point guide line request should parse");
        let through_snapshot = through_snapshot
            .0
            .as_any()
            .downcast_ref::<GuideLineSnapshot>()
            .expect("snapshot should downcast");
        assert_eq!(through_snapshot.direction, Vec3::Z);

        let angled_snapshot = GuideLineFactory
            .from_create_request(
                &world,
                &json!({
                    "type": "guide_line",
                    "anchor": [0.0, 0.0, 0.0],
                    "reference_direction": [1.0, 0.0, 0.0],
                    "angle_degrees": 90.0,
                    "plane_normal": [0.0, 1.0, 0.0],
                }),
            )
            .expect("angle-based guide line request should parse");
        let angled_snapshot = angled_snapshot
            .0
            .as_any()
            .downcast_ref::<GuideLineSnapshot>()
            .expect("snapshot should downcast");
        assert!(
            angled_snapshot
                .direction
                .distance(Vec3::new(0.0, 0.0, -1.0))
                < 1e-5
        );
    }

    #[test]
    fn guide_line_snap_points_use_dedicated_anchor_kind() {
        let mut world = World::new();
        world.insert_resource(GuideLineVisibility::default());
        world.spawn((
            ElementId(7),
            GuideLineNode {
                anchor: Vec3::new(1.0, 0.0, 2.0),
                direction: Vec3::X,
                finite_length: Some(2.0),
                visible: true,
                label: None,
            },
        ));

        let mut snap_points = Vec::new();
        GuideLineFactory.collect_snap_points(&world, &mut snap_points);

        assert_eq!(snap_points.len(), 3);
        assert_eq!(snap_points[0].position, Vec3::new(1.0, 0.0, 2.0));
        assert_eq!(snap_points[0].kind, SnapKind::GuideAnchor);
        assert!(snap_points[1..]
            .iter()
            .all(|snap_point| snap_point.kind == SnapKind::Endpoint));
    }
}
