use std::any::Any;

use bevy::prelude::*;
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
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
        inference::{InferenceEngine, ReferenceEdge},
        snap::SnapKind,
    },
};

const GUIDE_LINE_COLOR: Color = Color::srgba(0.0, 0.8, 0.8, 0.7);
const GUIDE_LINE_SELECTED_COLOR: Color = Color::srgba(0.0, 1.0, 1.0, 0.9);
const GUIDE_LINE_HALF_LENGTH: f32 = 500.0;
const GUIDE_LINE_HIT_RADIUS: f32 = 0.15;
const GUIDE_ANCHOR_CROSS_SIZE: f32 = 0.12;

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
                    .map(|text| if text.is_empty() { None } else { Some(text.to_string()) })
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
        draw_guide_line_gizmo(gizmos, &self.anchor, &self.direction, self.finite_length, color);
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
        let direction = obj
            .get("direction")
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::X)
            .try_normalize()
            .unwrap_or(Vec3::X);
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
                kind: SnapKind::Endpoint,
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
                out.push(SnapPoint {
                    position: node.anchor,
                    kind: SnapKind::Midpoint,
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
                    id: "view.toggle_guide_lines".to_string(),
                    label: "Guide Lines".to_string(),
                    description: "Show or hide all guide lines".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Shift+G".to_string()),
                    icon: None,
                    hint: Some("Toggle visibility of construction guide lines".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("guide_lines".to_string()),
                },
                execute_toggle_guide_line_visibility,
            )
            .add_systems(Update, draw_guide_line_visuals);
    }
}
