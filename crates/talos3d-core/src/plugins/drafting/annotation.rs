//! [`DimensionAnnotation`] — the authored-entity form of a drafting dimension.
//!
//! The runtime ECS representation is [`DimensionAnnotationNode`] (a Bevy
//! component). The serializable snapshot that implements [`AuthoredEntity`] is
//! [`DimensionAnnotationSnapshot`]. A [`DimensionAnnotationFactory`] registers
//! with the capability registry so the generic `create_entity` MCP tool can
//! instantiate dims by type_name `"drafting_dimension"`.
//!
//! Dimensions are **drawing metadata** per ADR-025; they persist via
//! `DocumentProperties.domain_defaults[DRAFTING_ANNOTATIONS_KEY]` rather than
//! the main entity list.

use std::any::Any;

use bevy::{ecs::component::Component, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        scalar_from_json, vec3_from_json, AuthoredEntity, BoxedEntity, EntityBounds, EntityScope,
        HandleInfo, HandleKind, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, HitCandidate, ModelSummaryAccumulator, SnapPoint,
    },
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
        snap::SnapKind,
    },
};

use super::{
    kind::{DimensionKind, DimensionKindTag},
    render::{render_dimension, DimensionInput},
    style::{DimensionStyle, DimensionStyleRegistry},
};

/// The entity type_name string. Stable — persistence and MCP depend on it.
pub const DRAFTING_DIMENSION_TYPE: &str = "drafting_dimension";

/// Runtime ECS component for a drafting dimension.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DimensionAnnotationNode {
    pub kind: DimensionKind,
    pub a: Vec3,
    pub b: Vec3,
    pub offset: Vec3,
    pub style_name: String,
    pub text_override: Option<String>,
    pub visible: bool,
}

impl DimensionAnnotationNode {
    pub fn to_input(&self) -> DimensionInput {
        DimensionInput {
            kind: self.kind.clone(),
            a: self.a,
            b: self.b,
            offset: self.offset,
            text_override: self.text_override.clone(),
        }
    }
}

/// Serializable snapshot implementing [`AuthoredEntity`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionAnnotationSnapshot {
    pub element_id: ElementId,
    pub kind: DimensionKind,
    pub a: Vec3,
    pub b: Vec3,
    pub offset: Vec3,
    pub style_name: String,
    pub text_override: Option<String>,
    pub visible: bool,
}

impl DimensionAnnotationSnapshot {
    fn midpoint(&self) -> Vec3 {
        (self.a + self.b) * 0.5
    }

    fn bounds_points(&self) -> EntityBounds {
        let mut pts = vec![self.a, self.b, self.a + self.offset, self.b + self.offset];
        // Radial / diameter use a centre — include it.
        match &self.kind {
            DimensionKind::Radial { center } | DimensionKind::Diameter { center } => {
                pts.push(*center);
            }
            DimensionKind::Angular { vertex } => pts.push(*vertex),
            _ => {}
        }
        let mut min = pts[0];
        let mut max = pts[0];
        for p in &pts[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        EntityBounds { min, max }
    }
}

impl AuthoredEntity for DimensionAnnotationSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        DRAFTING_DIMENSION_TYPE
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("{} dim", self.kind.tag().as_str())
    }

    fn center(&self) -> Vec3 {
        self.midpoint() + self.offset * 0.5
    }

    fn scope(&self) -> EntityScope {
        EntityScope::DrawingMetadata
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        next.a += delta;
        next.b += delta;
        // offset is relative to the feature, don't translate it
        match &mut next.kind {
            DimensionKind::Radial { center } | DimensionKind::Diameter { center } => {
                *center += delta;
            }
            DimensionKind::Angular { vertex } => *vertex += delta,
            _ => {}
        }
        next.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut next = self.clone();
        next.a = rotation * next.a;
        next.b = rotation * next.b;
        next.offset = rotation * next.offset;
        match &mut next.kind {
            DimensionKind::Linear { direction } => *direction = rotation * *direction,
            DimensionKind::Radial { center } | DimensionKind::Diameter { center } => {
                *center = rotation * *center;
            }
            DimensionKind::Angular { vertex } => *vertex = rotation * *vertex,
            _ => {}
        }
        next.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        next.a = center + (next.a - center) * factor;
        next.b = center + (next.b - center) * factor;
        next.offset *= factor;
        match &mut next.kind {
            DimensionKind::Radial { center: c } | DimensionKind::Diameter { center: c } => {
                *c = center + (*c - center) * factor;
            }
            DimensionKind::Angular { vertex } => {
                *vertex = center + (*vertex - center) * factor;
            }
            _ => {}
        }
        next.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            read_only_property_field(
                "kind",
                "Kind",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.kind.tag().as_str().to_string())),
            ),
            property_field(
                "a",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.a)),
            ),
            property_field(
                "b",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.b)),
            ),
            property_field_with(
                "offset",
                "Offset",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.offset)),
                true,
            ),
            property_field_with(
                "style",
                "Style",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.style_name.clone())),
                true,
            ),
            property_field_with(
                "text_override",
                "Text override",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(
                    self.text_override.clone().unwrap_or_default(),
                )),
                true,
            ),
            property_field_with(
                "visible",
                "Visible",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.visible.to_string())),
                true,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut next = self.clone();
        match property_name {
            "a" => next.a = vec3_from_json(value)?,
            "b" => next.b = vec3_from_json(value)?,
            "offset" => next.offset = vec3_from_json(value)?,
            "style" => {
                next.style_name = value
                    .as_str()
                    .ok_or_else(|| "style must be a string".to_string())?
                    .to_string();
            }
            "text_override" => {
                next.text_override = value
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(ToOwned::to_owned);
            }
            "visible" => {
                next.visible = parse_bool_json(value)?;
            }
            _ => {
                return Err(invalid_property_error(
                    DRAFTING_DIMENSION_TYPE,
                    &["a", "b", "offset", "style", "text_override", "visible"],
                ));
            }
        }
        Ok(next.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![
            HandleInfo {
                id: "a".into(),
                position: self.a,
                kind: HandleKind::Vertex,
                label: "A".into(),
            },
            HandleInfo {
                id: "b".into(),
                position: self.b,
                kind: HandleKind::Vertex,
                label: "B".into(),
            },
            HandleInfo {
                id: "offset".into(),
                position: self.midpoint() + self.offset,
                kind: HandleKind::Control,
                label: "Offset".into(),
            },
        ]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        Some(self.bounds_points())
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let mut next = self.clone();
        match handle_id {
            "a" => next.a = cursor,
            "b" => next.b = cursor,
            "offset" => next.offset = cursor - self.midpoint(),
            _ => return None,
        }
        Some(next.into())
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        let node = DimensionAnnotationNode {
            kind: self.kind.clone(),
            a: self.a,
            b: self.b,
            offset: self.offset,
            style_name: self.style_name.clone(),
            text_override: self.text_override.clone(),
            visible: self.visible,
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
        // Simple gizmo preview: draw segment a→b and extension-line hints.
        gizmos.line(self.a, self.b, color);
        gizmos.line(self.a, self.a + self.offset, color);
        gizmos.line(self.b, self.b + self.offset, color);
        gizmos.line(self.a + self.offset, self.b + self.offset, color);
    }

    fn preview_line_count(&self) -> usize {
        4
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == DRAFTING_DIMENSION_TYPE && other.to_json() == self.to_json()
    }
}

impl From<DimensionAnnotationSnapshot> for BoxedEntity {
    fn from(snapshot: DimensionAnnotationSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

// ─── Factory ─────────────────────────────────────────────────────────────────

pub struct DimensionAnnotationFactory;

impl AuthoredEntityFactory for DimensionAnnotationFactory {
    fn type_name(&self) -> &'static str {
        DRAFTING_DIMENSION_TYPE
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<DimensionAnnotationNode>()?;
        Some(
            DimensionAnnotationSnapshot {
                element_id,
                kind: node.kind.clone(),
                a: node.a,
                b: node.b,
                offset: node.offset,
                style_name: node.style_name.clone(),
                text_override: node.text_override.clone(),
                visible: node.visible,
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let snap: DimensionAnnotationSnapshot =
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
        Ok(snap.into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let element_id = world
            .get_resource::<ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();

        // Accept a few convenient shapes:
        //   {kind: "linear", direction: [1,0,0], a: [...], b: [...], offset: [...], style: "..."}
        //   {kind: "aligned", a, b, offset_distance: 0.5, style: "..."}
        //   {kind: "radial" | "diameter", center, radius_point, offset, style}
        let kind_str = object
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("linear");

        let a = object
            .get("a")
            .or_else(|| object.get("start"))
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::ZERO);
        let b = object
            .get("b")
            .or_else(|| object.get("end"))
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::X);

        let offset = if let Some(v) = object.get("offset") {
            vec3_from_json(v)?
        } else if let Some(d) = object.get("offset_distance") {
            // offset_distance is a scalar — compute a perpendicular automatically.
            let dist = scalar_from_json(d)?;
            let axis = (b - a).try_normalize().unwrap_or(Vec3::X);
            let perp = axis.cross(Vec3::Z).try_normalize().unwrap_or(Vec3::Y);
            perp * dist
        } else {
            Vec3::new(0.0, 0.5, 0.0)
        };

        let kind = match kind_str {
            "linear" => {
                let direction = object
                    .get("direction")
                    .map(vec3_from_json)
                    .transpose()?
                    .unwrap_or(Vec3::X);
                DimensionKind::Linear { direction }
            }
            "aligned" => DimensionKind::Aligned,
            "angular" => {
                let vertex = object
                    .get("vertex")
                    .map(vec3_from_json)
                    .transpose()?
                    .ok_or_else(|| "angular dim requires `vertex`".to_string())?;
                DimensionKind::Angular { vertex }
            }
            "radial" => {
                let center = object
                    .get("center")
                    .map(vec3_from_json)
                    .transpose()?
                    .ok_or_else(|| "radial dim requires `center`".to_string())?;
                DimensionKind::Radial { center }
            }
            "diameter" => {
                let center = object
                    .get("center")
                    .map(vec3_from_json)
                    .transpose()?
                    .ok_or_else(|| "diameter dim requires `center`".to_string())?;
                DimensionKind::Diameter { center }
            }
            "leader" => {
                let text = object
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "leader dim requires `text`".to_string())?
                    .to_string();
                DimensionKind::Leader { text }
            }
            other => return Err(format!("unknown dimension kind '{other}'")),
        };

        let style_name = object
            .get("style")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| {
                world
                    .get_resource::<DimensionStyleRegistry>()
                    .map(|r| r.default_name().to_string())
                    .unwrap_or_else(|| "architectural_metric".to_string())
            });

        let text_override = object
            .get("text_override")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(ToOwned::to_owned);

        let visible = object
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        Ok(DimensionAnnotationSnapshot {
            element_id,
            kind,
            a,
            b,
            offset,
            style_name,
            text_override,
            visible,
        }
        .into())
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        // Distance from ray to the line a→b, picking the closest annotation
        // whose projected line the ray passes within 0.15 m of.
        let mut query = world.try_query::<(Entity, &DimensionAnnotationNode)>()?;
        let mut best: Option<(Entity, f32)> = None;
        for (entity, node) in query.iter(world) {
            if !node.visible {
                continue;
            }
            let mid = (node.a + node.b) * 0.5;
            let d = ray_to_point_distance(ray.origin, *ray.direction, mid);
            if d > 0.3 {
                continue;
            }
            let along = ray.origin.distance(mid);
            match best {
                Some((_, prev)) if prev < along => {}
                _ => best = Some((entity, along)),
            }
        }
        best.map(|(entity, distance)| HitCandidate { entity, distance })
    }

    fn collect_snap_points(&self, world: &World, out: &mut Vec<SnapPoint>) {
        let Some(mut query) = world.try_query::<&DimensionAnnotationNode>() else {
            return;
        };
        for node in query.iter(world) {
            if !node.visible {
                continue;
            }
            out.push(SnapPoint {
                position: node.a,
                kind: SnapKind::Endpoint,
            });
            out.push(SnapPoint {
                position: node.b,
                kind: SnapKind::Endpoint,
            });
            out.push(SnapPoint {
                position: (node.a + node.b) * 0.5 + node.offset,
                kind: SnapKind::Control,
            });
        }
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let Some(mut query) = world.try_query::<&DimensionAnnotationNode>() else {
            return;
        };
        let mut linear = 0usize;
        let mut aligned = 0usize;
        let mut radial = 0usize;
        let mut diameter = 0usize;
        let mut angular = 0usize;
        let mut leader = 0usize;
        for node in query.iter(world) {
            match node.kind.tag() {
                DimensionKindTag::Linear => linear += 1,
                DimensionKindTag::Aligned => aligned += 1,
                DimensionKindTag::Radial => radial += 1,
                DimensionKindTag::Diameter => diameter += 1,
                DimensionKindTag::Angular => angular += 1,
                DimensionKindTag::Leader => leader += 1,
            }
        }
        if linear + aligned + radial + diameter + angular + leader == 0 {
            return;
        }
        summary.metrics.insert(
            "drafting_dimensions".to_string(),
            json!({
                "linear": linear,
                "aligned": aligned,
                "radial": radial,
                "diameter": diameter,
                "angular": angular,
                "leader": leader,
            }),
        );
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn ray_to_point_distance(origin: Vec3, direction: Vec3, point: Vec3) -> f32 {
    let to_point = point - origin;
    let t = to_point.dot(direction).max(0.0);
    let closest = origin + direction * t;
    closest.distance(point)
}

fn parse_bool_json(value: &Value) -> Result<bool, String> {
    if let Some(b) = value.as_bool() {
        return Ok(b);
    }
    if let Some(s) = value.as_str() {
        return match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            _ => Err(format!("cannot parse '{s}' as bool")),
        };
    }
    Err("expected bool".to_string())
}

/// Render a live annotation to primitives using a resolved style and scale.
/// Helper for other modules (vector_drawing integration).
#[must_use]
pub fn render_annotation(
    node: &DimensionAnnotationNode,
    registry: &DimensionStyleRegistry,
    world_to_paper: f32,
) -> Vec<super::render::DimPrimitive> {
    let style: DimensionStyle = registry.resolve(Some(&node.style_name));
    render_dimension(&node.to_input(), &style, world_to_paper)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_parses_linear_create_request() {
        let factory = DimensionAnnotationFactory;
        // Create a minimal world with the ElementIdAllocator resource.
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(DimensionStyleRegistry::default());

        let request = json!({
            "kind": "linear",
            "direction": [1.0, 0.0, 0.0],
            "a": [0.0, 0.0, 0.0],
            "b": [4.572, 0.0, 0.0],
            "offset": [0.0, 0.5, 0.0],
            "style": "architectural_imperial",
        });
        let snap = factory
            .from_create_request(&world, &request)
            .expect("create ok");
        assert_eq!(snap.type_name(), DRAFTING_DIMENSION_TYPE);
    }

    #[test]
    fn factory_rejects_unknown_kind() {
        let factory = DimensionAnnotationFactory;
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());
        let request = json!({ "kind": "not_a_kind" });
        assert!(factory.from_create_request(&world, &request).is_err());
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let snap = DimensionAnnotationSnapshot {
            element_id: ElementId(42),
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(4.572, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.5, 0.0),
            style_name: "architectural_imperial".into(),
            text_override: None,
            visible: true,
        };
        let json = snap.to_json();
        let factory = DimensionAnnotationFactory;
        let round = factory.from_persisted_json(&json).expect("roundtrip ok");
        assert!(round.0.eq_snapshot(&snap));
    }
}
