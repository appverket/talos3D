//! Fillet and chamfer geometry nodes.
//!
//! Fillet and chamfer are authored feature nodes that wrap a live source
//! entity. The source remains editable while the feature owns the derived mesh.

use std::any::Any;
use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, AuthoredEntity, BoxedEntity, EntityBounds,
        HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, HitCandidate, ModelSummaryAccumulator},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        modeling::{
            bsp_csg,
            csg::EvaluatedCsg,
            mesh_generation::{NeedsEvaluation, NeedsMesh},
            primitive_trait::{ray_aabb_intersection, Primitive},
            primitives::{BoxPrimitive, ShapeRotation},
            profile::{ProfileExtrusion, ProfileRevolve, ProfileSweep},
            profile_feature::EvaluatedFeature,
        },
    },
};

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilletNode {
    pub source: ElementId,
    pub radius: f32,
    pub segments: u32,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChamferNode {
    pub source: ElementId,
    pub distance: f32,
}

#[derive(Component, Debug, Clone)]
pub struct FilletOperand {
    pub owner: ElementId,
}

#[derive(Component, Debug, Clone)]
pub struct EvaluatedFillet {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct FilletSnapshot {
    pub element_id: ElementId,
    pub fillet_node: FilletNode,
}

impl PartialEq for FilletSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id && self.fillet_node == other.fillet_node
    }
}

impl AuthoredEntity for FilletSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "fillet"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("Fillet({})", self.fillet_node.source.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.box_clone()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn push_pull(
        &self,
        _face_id: crate::capability_registry::FaceId,
        _distance: f32,
    ) -> Option<BoxedEntity> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "radius",
                "Radius (m)",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.fillet_node.radius)),
                true,
            ),
            property_field_with(
                "segments",
                "Segments",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.fillet_node.segments as f32)),
                true,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        let as_f32 = |v: &Value| -> Result<f32, String> {
            v.as_f64()
                .map(|number| number as f32)
                .ok_or_else(|| format!("Expected a number for '{property_name}'"))
        };
        match property_name {
            "radius" => snapshot.fillet_node.radius = as_f32(value)?.max(0.0),
            "segments" => snapshot.fillet_node.segments = as_f32(value)?.max(1.0) as u32,
            _ => return Err(invalid_property_error("fillet", &["radius", "segments"])),
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        None
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(&self.fillet_node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.fillet_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        } else {
            world.spawn((
                self.element_id,
                self.fillet_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        if let Some(source_entity) = find_entity_by_element_id(world, self.fillet_node.source) {
            world
                .entity_mut(source_entity)
                .insert(FilletOperand {
                    owner: self.element_id,
                })
                .insert(Visibility::Hidden);
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        restore_source_visibility(world, self.element_id, self.fillet_node.source);
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
        other.type_name() == "fillet" && other.to_json() == self.to_json()
    }
}

impl From<FilletSnapshot> for BoxedEntity {
    fn from(snapshot: FilletSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

pub struct FilletFactory;

impl AuthoredEntityFactory for FilletFactory {
    fn type_name(&self) -> &'static str {
        "fillet"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let fillet_node = entity_ref.get::<FilletNode>()?.clone();
        Some(
            FilletSnapshot {
                element_id,
                fillet_node,
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(Value::as_u64)
                .ok_or("Missing or invalid element_id")?,
        );
        let fillet_node = serde_json::from_value::<FilletNode>(data.clone())
            .map_err(|error| error.to_string())?;
        Ok(FilletSnapshot {
            element_id,
            fillet_node,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();
        let source = request
            .get("source")
            .and_then(Value::as_u64)
            .map(ElementId)
            .ok_or("Missing 'source' field (u64)")?;
        let radius = request
            .get("radius")
            .and_then(Value::as_f64)
            .unwrap_or(0.05) as f32;
        let segments = request
            .get("segments")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .max(1) as u32;
        Ok(FilletSnapshot {
            element_id,
            fillet_node: FilletNode {
                source,
                radius,
                segments,
            },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Some(evaluated) = world.get::<EvaluatedFillet>(entity) else {
            return;
        };
        draw_evaluated_wireframe(&evaluated.vertices, &evaluated.indices, gizmos, color);
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        world
            .get::<EvaluatedFillet>(entity)
            .map(|evaluated| evaluated.indices.len())
            .unwrap_or(0)
    }

    fn hit_test(&self, world: &World, ray: bevy::math::Ray3d) -> Option<HitCandidate> {
        evaluated_fillet_hit_test(world, ray, false)
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        contribute_fillet_summary(world, summary);
    }

    fn collect_delete_dependencies(
        &self,
        world: &World,
        requested_ids: &[ElementId],
        out: &mut Vec<ElementId>,
    ) {
        collect_source_dependencies::<FilletNode>(world, requested_ids, out, |node| node.source);
    }
}

#[derive(Debug, Clone)]
pub struct ChamferSnapshot {
    pub element_id: ElementId,
    pub chamfer_node: ChamferNode,
}

impl PartialEq for ChamferSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id && self.chamfer_node == other.chamfer_node
    }
}

impl AuthoredEntity for ChamferSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "chamfer"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("Chamfer({})", self.chamfer_node.source.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.box_clone()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.box_clone()
    }

    fn push_pull(
        &self,
        _face_id: crate::capability_registry::FaceId,
        _distance: f32,
    ) -> Option<BoxedEntity> {
        None
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![property_field_with(
            "distance",
            "Distance (m)",
            PropertyValueKind::Scalar,
            Some(PropertyValue::Scalar(self.chamfer_node.distance)),
            true,
        )]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "distance" => {
                snapshot.chamfer_node.distance = value
                    .as_f64()
                    .map(|number| (number as f32).max(0.0))
                    .ok_or_else(|| format!("Expected a number for '{property_name}'"))?;
            }
            _ => return Err(invalid_property_error("chamfer", &["distance"])),
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        None
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(&self.chamfer_node).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.chamfer_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        } else {
            world.spawn((
                self.element_id,
                self.chamfer_node.clone(),
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        if let Some(source_entity) = find_entity_by_element_id(world, self.chamfer_node.source) {
            world
                .entity_mut(source_entity)
                .insert(FilletOperand {
                    owner: self.element_id,
                })
                .insert(Visibility::Hidden);
        }
    }

    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        restore_source_visibility(world, self.element_id, self.chamfer_node.source);
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
        other.type_name() == "chamfer" && other.to_json() == self.to_json()
    }
}

impl From<ChamferSnapshot> for BoxedEntity {
    fn from(snapshot: ChamferSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

pub struct ChamferFactory;

impl AuthoredEntityFactory for ChamferFactory {
    fn type_name(&self) -> &'static str {
        "chamfer"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let chamfer_node = entity_ref.get::<ChamferNode>()?.clone();
        Some(
            ChamferSnapshot {
                element_id,
                chamfer_node,
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id = ElementId(
            data.get("element_id")
                .and_then(Value::as_u64)
                .ok_or("Missing or invalid element_id")?,
        );
        let chamfer_node = serde_json::from_value::<ChamferNode>(data.clone())
            .map_err(|error| error.to_string())?;
        Ok(ChamferSnapshot {
            element_id,
            chamfer_node,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();
        let source = request
            .get("source")
            .and_then(Value::as_u64)
            .map(ElementId)
            .ok_or("Missing 'source' field (u64)")?;
        let distance = request
            .get("distance")
            .and_then(Value::as_f64)
            .unwrap_or(0.05) as f32;
        Ok(ChamferSnapshot {
            element_id,
            chamfer_node: ChamferNode { source, distance },
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Some(evaluated) = world.get::<EvaluatedFillet>(entity) else {
            return;
        };
        draw_evaluated_wireframe(&evaluated.vertices, &evaluated.indices, gizmos, color);
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        world
            .get::<EvaluatedFillet>(entity)
            .map(|evaluated| evaluated.indices.len())
            .unwrap_or(0)
    }

    fn hit_test(&self, world: &World, ray: bevy::math::Ray3d) -> Option<HitCandidate> {
        evaluated_fillet_hit_test(world, ray, true)
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        contribute_chamfer_summary(world, summary);
    }

    fn collect_delete_dependencies(
        &self,
        world: &World,
        requested_ids: &[ElementId],
        out: &mut Vec<ElementId>,
    ) {
        collect_source_dependencies::<ChamferNode>(world, requested_ids, out, |node| node.source);
    }
}

fn restore_source_visibility(world: &mut World, owner: ElementId, source_id: ElementId) {
    if let Some(source_entity) = find_entity_by_element_id(world, source_id) {
        let still_ours = world
            .get::<FilletOperand>(source_entity)
            .map(|operand| operand.owner == owner)
            .unwrap_or(false);
        if still_ours {
            world
                .entity_mut(source_entity)
                .remove::<FilletOperand>()
                .insert(Visibility::Visible);
        }
    }
}

fn evaluated_fillet_hit_test(
    world: &World,
    ray: bevy::math::Ray3d,
    chamfer_only: bool,
) -> Option<HitCandidate> {
    let mut best: Option<HitCandidate> = None;
    let mut query = world
        .try_query::<(Entity, Option<&ChamferNode>, &EvaluatedFillet)>()
        .unwrap();
    for (entity, chamfer, evaluated) in query.iter(world) {
        if chamfer_only && chamfer.is_none() {
            continue;
        }
        if !chamfer_only && chamfer.is_some() {
            continue;
        }
        if evaluated.vertices.is_empty() {
            continue;
        }
        let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for vertex in &evaluated.vertices {
            min = min.min(*vertex);
            max = max.max(*vertex);
        }
        if let Some(distance) = ray_aabb_intersection(ray, min, max) {
            if best.is_none() || distance < best.as_ref().unwrap().distance {
                best = Some(HitCandidate { entity, distance });
            }
        }
    }
    best
}

fn contribute_fillet_summary(world: &World, summary: &mut ModelSummaryAccumulator) {
    let mut count = 0usize;
    let mut query = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    for entity_ref in query.iter(world) {
        if entity_ref.get::<FilletNode>().is_none() {
            continue;
        }
        count += 1;
        if let Some(evaluated) = entity_ref.get::<EvaluatedFillet>() {
            summary
                .bounding_points
                .extend(evaluated.vertices.iter().copied());
        }
    }
    if count > 0 {
        *summary
            .entity_counts
            .entry("fillet".to_string())
            .or_insert(0) += count;
    }
}

fn contribute_chamfer_summary(world: &World, summary: &mut ModelSummaryAccumulator) {
    let mut count = 0usize;
    let mut query = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    for entity_ref in query.iter(world) {
        if entity_ref.get::<ChamferNode>().is_none() {
            continue;
        }
        count += 1;
        if let Some(evaluated) = entity_ref.get::<EvaluatedFillet>() {
            summary
                .bounding_points
                .extend(evaluated.vertices.iter().copied());
        }
    }
    if count > 0 {
        *summary
            .entity_counts
            .entry("chamfer".to_string())
            .or_insert(0) += count;
    }
}

fn collect_source_dependencies<T: Component>(
    world: &World,
    requested_ids: &[ElementId],
    out: &mut Vec<ElementId>,
    source_of: impl Fn(&T) -> ElementId,
) {
    let Some(mut query) = world.try_query::<(&ElementId, &T)>() else {
        return;
    };
    for (feature_id, node) in query.iter(world) {
        if requested_ids.contains(&source_of(node)) {
            out.push(*feature_id);
        }
    }
}

pub fn propagate_fillet_source_changes(
    mut commands: Commands,
    changed_sources: Query<&FilletOperand, With<NeedsMesh>>,
    features: Query<(Entity, &ElementId), Or<(With<FilletNode>, With<ChamferNode>)>>,
) {
    for operand in &changed_sources {
        for (feature_entity, feature_id) in &features {
            if *feature_id == operand.owner {
                commands.entity(feature_entity).try_insert(NeedsEvaluation);
            }
        }
    }
}

pub fn evaluate_fillet_nodes(
    mut commands: Commands,
    dirty: Query<(Entity, &FilletNode), With<NeedsEvaluation>>,
    world: &World,
) {
    for (entity, fillet_node) in &dirty {
        let Some(source_triangles) = get_source_triangles(world, fillet_node.source) else {
            continue;
        };
        let (vertices, normals, indices) = bevel_triangles(
            &source_triangles,
            fillet_node.radius,
            fillet_node.segments.max(1),
            world,
            fillet_node.source,
        );
        commands.entity(entity).try_insert((
            EvaluatedFillet {
                vertices,
                normals,
                indices,
            },
            NeedsMesh,
        ));
        commands.entity(entity).remove::<NeedsEvaluation>();
    }
}

pub fn evaluate_chamfer_nodes(
    mut commands: Commands,
    dirty: Query<(Entity, &ChamferNode), With<NeedsEvaluation>>,
    world: &World,
) {
    for (entity, chamfer_node) in &dirty {
        let Some(source_triangles) = get_source_triangles(world, chamfer_node.source) else {
            continue;
        };
        let (vertices, normals, indices) = bevel_triangles(
            &source_triangles,
            chamfer_node.distance,
            1,
            world,
            chamfer_node.source,
        );
        commands.entity(entity).try_insert((
            EvaluatedFillet {
                vertices,
                normals,
                indices,
            },
            NeedsMesh,
        ));
        commands.entity(entity).remove::<NeedsEvaluation>();
    }
}

fn bevel_triangles(
    triangles: &[bsp_csg::CsgTriangle],
    radius: f32,
    segments: u32,
    world: &World,
    source_id: ElementId,
) -> (Vec<Vec3>, Vec<Vec3>, Vec<u32>) {
    if let Some(result) = try_box_fast_path(world, source_id, radius, segments) {
        return result;
    }
    general_bevel(triangles, radius, segments)
}

fn try_box_fast_path(
    world: &World,
    source_id: ElementId,
    radius: f32,
    segments: u32,
) -> Option<(Vec<Vec3>, Vec<Vec3>, Vec<u32>)> {
    let mut query = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    let entity_ref = query
        .iter(world)
        .find(|entity| entity.get::<ElementId>().copied() == Some(source_id))?;
    let primitive = entity_ref.get::<BoxPrimitive>()?;
    let clamped_radius = radius.min(
        primitive
            .half_extents
            .x
            .min(primitive.half_extents.y)
            .min(primitive.half_extents.z),
    );
    if clamped_radius <= 0.0 {
        return None;
    }

    let (mut vertices, normals, indices) =
        bevelled_box_mesh(primitive.half_extents, clamped_radius, segments.max(1));
    for vertex in &mut vertices {
        *vertex += primitive.centre;
    }
    Some((vertices, normals, indices))
}

pub fn bevelled_box_mesh(
    half: Vec3,
    radius: f32,
    segments: u32,
) -> (Vec<Vec3>, Vec<Vec3>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();

    let hx = half.x - radius;
    let hy = half.y - radius;
    let hz = half.z - radius;

    let push_quad = |vertices: &mut Vec<Vec3>,
                     normals: &mut Vec<Vec3>,
                     indices: &mut Vec<u32>,
                     positions: [Vec3; 4],
                     face_normals: [Vec3; 4]| {
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&positions);
        normals.extend_from_slice(&face_normals);
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    let arc_strip = |n0: Vec3, n1: Vec3, centre_offset: Vec3, p_low: Vec3, p_high: Vec3| {
        let segments = segments as usize;
        let mut points = Vec::with_capacity((segments + 1) * 2);
        for index in 0..=segments {
            let t = index as f32 / segments as f32;
            let angle = t * std::f32::consts::FRAC_PI_2;
            let (sin_angle, cos_angle) = angle.sin_cos();
            let normal = (n0 * cos_angle + n1 * sin_angle).normalize_or_zero();
            points.push((centre_offset + p_low + normal * radius, normal));
            points.push((centre_offset + p_high + normal * radius, normal));
        }
        points
    };

    let flat_faces: [(Vec3, [Vec3; 4]); 6] = [
        (
            Vec3::X,
            [
                Vec3::new(half.x, hy, -hz),
                Vec3::new(half.x, hy, hz),
                Vec3::new(half.x, -hy, hz),
                Vec3::new(half.x, -hy, -hz),
            ],
        ),
        (
            -Vec3::X,
            [
                Vec3::new(-half.x, hy, hz),
                Vec3::new(-half.x, hy, -hz),
                Vec3::new(-half.x, -hy, -hz),
                Vec3::new(-half.x, -hy, hz),
            ],
        ),
        (
            Vec3::Y,
            [
                Vec3::new(-hx, half.y, -hz),
                Vec3::new(hx, half.y, -hz),
                Vec3::new(hx, half.y, hz),
                Vec3::new(-hx, half.y, hz),
            ],
        ),
        (
            -Vec3::Y,
            [
                Vec3::new(-hx, -half.y, hz),
                Vec3::new(hx, -half.y, hz),
                Vec3::new(hx, -half.y, -hz),
                Vec3::new(-hx, -half.y, -hz),
            ],
        ),
        (
            Vec3::Z,
            [
                Vec3::new(-hx, hy, half.z),
                Vec3::new(hx, hy, half.z),
                Vec3::new(hx, -hy, half.z),
                Vec3::new(-hx, -hy, half.z),
            ],
        ),
        (
            -Vec3::Z,
            [
                Vec3::new(hx, hy, -half.z),
                Vec3::new(-hx, hy, -half.z),
                Vec3::new(-hx, -hy, -half.z),
                Vec3::new(hx, -hy, -half.z),
            ],
        ),
    ];

    for (normal, positions) in &flat_faces {
        push_quad(
            &mut vertices,
            &mut normals,
            &mut indices,
            *positions,
            [*normal; 4],
        );
    }

    struct EdgeSpec {
        n0: Vec3,
        n1: Vec3,
        centre: Vec3,
        p_low: Vec3,
        p_high: Vec3,
    }

    let edge_specs = [
        EdgeSpec {
            n0: Vec3::X,
            n1: Vec3::Y,
            centre: Vec3::new(hx, hy, 0.0),
            p_low: Vec3::new(0.0, 0.0, -hz),
            p_high: Vec3::new(0.0, 0.0, hz),
        },
        EdgeSpec {
            n0: Vec3::X,
            n1: -Vec3::Y,
            centre: Vec3::new(hx, -hy, 0.0),
            p_low: Vec3::new(0.0, 0.0, -hz),
            p_high: Vec3::new(0.0, 0.0, hz),
        },
        EdgeSpec {
            n0: -Vec3::X,
            n1: Vec3::Y,
            centre: Vec3::new(-hx, hy, 0.0),
            p_low: Vec3::new(0.0, 0.0, hz),
            p_high: Vec3::new(0.0, 0.0, -hz),
        },
        EdgeSpec {
            n0: -Vec3::X,
            n1: -Vec3::Y,
            centre: Vec3::new(-hx, -hy, 0.0),
            p_low: Vec3::new(0.0, 0.0, hz),
            p_high: Vec3::new(0.0, 0.0, -hz),
        },
        EdgeSpec {
            n0: Vec3::X,
            n1: Vec3::Z,
            centre: Vec3::new(hx, 0.0, hz),
            p_low: Vec3::new(0.0, -hy, 0.0),
            p_high: Vec3::new(0.0, hy, 0.0),
        },
        EdgeSpec {
            n0: Vec3::X,
            n1: -Vec3::Z,
            centre: Vec3::new(hx, 0.0, -hz),
            p_low: Vec3::new(0.0, hy, 0.0),
            p_high: Vec3::new(0.0, -hy, 0.0),
        },
        EdgeSpec {
            n0: -Vec3::X,
            n1: Vec3::Z,
            centre: Vec3::new(-hx, 0.0, hz),
            p_low: Vec3::new(0.0, hy, 0.0),
            p_high: Vec3::new(0.0, -hy, 0.0),
        },
        EdgeSpec {
            n0: -Vec3::X,
            n1: -Vec3::Z,
            centre: Vec3::new(-hx, 0.0, -hz),
            p_low: Vec3::new(0.0, -hy, 0.0),
            p_high: Vec3::new(0.0, hy, 0.0),
        },
        EdgeSpec {
            n0: Vec3::Y,
            n1: Vec3::Z,
            centre: Vec3::new(0.0, hy, hz),
            p_low: Vec3::new(hx, 0.0, 0.0),
            p_high: Vec3::new(-hx, 0.0, 0.0),
        },
        EdgeSpec {
            n0: Vec3::Y,
            n1: -Vec3::Z,
            centre: Vec3::new(0.0, hy, -hz),
            p_low: Vec3::new(-hx, 0.0, 0.0),
            p_high: Vec3::new(hx, 0.0, 0.0),
        },
        EdgeSpec {
            n0: -Vec3::Y,
            n1: Vec3::Z,
            centre: Vec3::new(0.0, -hy, hz),
            p_low: Vec3::new(-hx, 0.0, 0.0),
            p_high: Vec3::new(hx, 0.0, 0.0),
        },
        EdgeSpec {
            n0: -Vec3::Y,
            n1: -Vec3::Z,
            centre: Vec3::new(0.0, -hy, -hz),
            p_low: Vec3::new(hx, 0.0, 0.0),
            p_high: Vec3::new(-hx, 0.0, 0.0),
        },
    ];

    for edge in &edge_specs {
        let strip = arc_strip(edge.n0, edge.n1, edge.centre, edge.p_low, edge.p_high);
        for segment_index in 0..segments as usize {
            let (p0l, n0l) = strip[segment_index * 2];
            let (p0h, n0h) = strip[segment_index * 2 + 1];
            let (p1l, n1l) = strip[(segment_index + 1) * 2];
            let (p1h, n1h) = strip[(segment_index + 1) * 2 + 1];
            push_quad(
                &mut vertices,
                &mut normals,
                &mut indices,
                [p0l, p0h, p1h, p1l],
                [n0l, n0h, n1h, n1l],
            );
        }
    }

    let signs = [1.0, -1.0];
    let segments = segments as usize;
    for &sign_x in &signs {
        for &sign_y in &signs {
            for &sign_z in &signs {
                let corner = Vec3::new(sign_x * hx, sign_y * hy, sign_z * hz);
                let nx = Vec3::new(sign_x, 0.0, 0.0);
                let ny = Vec3::new(0.0, sign_y, 0.0);
                let nz = Vec3::new(0.0, 0.0, sign_z);
                let mut grid = vec![(Vec3::ZERO, Vec3::ZERO); (segments + 1) * (segments + 1)];
                for j in 0..=segments {
                    for i in 0..=segments {
                        let theta = (i as f32 / segments as f32) * std::f32::consts::FRAC_PI_2;
                        let phi = (j as f32 / segments as f32) * std::f32::consts::FRAC_PI_2;
                        let (st, ct) = theta.sin_cos();
                        let (sp, cp) = phi.sin_cos();
                        let equator = (nx * ct + ny * st).normalize_or_zero();
                        let normal = (nz * cp + equator * sp).normalize_or_zero();
                        grid[j * (segments + 1) + i] = (corner + normal * radius, normal);
                    }
                }
                for j in 0..segments {
                    for i in 0..segments {
                        let i00 = j * (segments + 1) + i;
                        let i10 = j * (segments + 1) + i + 1;
                        let i01 = (j + 1) * (segments + 1) + i;
                        let i11 = (j + 1) * (segments + 1) + i + 1;
                        let (p00, n00) = grid[i00];
                        let (p10, n10) = grid[i10];
                        let (p01, n01) = grid[i01];
                        let (p11, n11) = grid[i11];
                        let face_normal = (p10 - p00).cross(p01 - p00);
                        let corner_dir = (p00 - corner).normalize_or_zero();
                        if face_normal.dot(corner_dir) >= 0.0 {
                            push_quad(
                                &mut vertices,
                                &mut normals,
                                &mut indices,
                                [p00, p10, p11, p01],
                                [n00, n10, n11, n01],
                            );
                        } else {
                            push_quad(
                                &mut vertices,
                                &mut normals,
                                &mut indices,
                                [p00, p01, p11, p10],
                                [n00, n01, n11, n10],
                            );
                        }
                    }
                }
            }
        }
    }

    (vertices, normals, indices)
}

fn general_bevel(
    triangles: &[bsp_csg::CsgTriangle],
    radius: f32,
    segments: u32,
) -> (Vec<Vec3>, Vec<Vec3>, Vec<u32>) {
    if triangles.is_empty() || radius <= 0.0 {
        return flat_to_indexed(triangles);
    }

    let (vertex_pool, tri_vertices, tri_normals) = build_vertex_pool(triangles);
    let mut edge_faces: HashMap<(u32, u32), Vec<Vec3>> = HashMap::new();
    for (triangle_index, vertices) in tri_vertices.iter().enumerate() {
        let normal = tri_normals[triangle_index];
        for edge in [(0, 1), (1, 2), (2, 0)] {
            let (a, b) = (vertices[edge.0], vertices[edge.1]);
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(normal);
        }
    }

    const SHARP_ANGLE_COS: f32 = 0.866;

    let mut out_vertices = Vec::new();
    let mut out_normals = Vec::new();
    let mut out_indices = Vec::new();

    for (triangle_index, vertices) in tri_vertices.iter().enumerate() {
        let normal = tri_normals[triangle_index];
        let base = out_vertices.len() as u32;
        for vertex_index in vertices {
            out_vertices.push(vertex_pool[*vertex_index as usize]);
            out_normals.push(normal);
        }
        out_indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    for ((a, b), adjacent_normals) in edge_faces {
        if adjacent_normals.len() < 2 {
            continue;
        }
        let n0 = adjacent_normals[0].normalize_or_zero();
        let n1 = adjacent_normals[1].normalize_or_zero();
        if n0.dot(n1) >= SHARP_ANGLE_COS {
            continue;
        }

        let va = vertex_pool[a as usize];
        let vb = vertex_pool[b as usize];
        for segment_index in 0..segments as usize {
            let t0 = segment_index as f32 / segments as f32;
            let t1 = (segment_index + 1) as f32 / segments as f32;
            let ang0 = t0 * std::f32::consts::FRAC_PI_2;
            let ang1 = t1 * std::f32::consts::FRAC_PI_2;
            let (sin0, cos0) = ang0.sin_cos();
            let (sin1, cos1) = ang1.sin_cos();
            let bv0 = (n0 * cos0 + n1 * sin0).normalize_or_zero();
            let bv1 = (n0 * cos1 + n1 * sin1).normalize_or_zero();
            let p00 = va + bv0 * radius;
            let p01 = vb + bv0 * radius;
            let p10 = va + bv1 * radius;
            let p11 = vb + bv1 * radius;
            let base = out_vertices.len() as u32;
            out_vertices.extend_from_slice(&[p00, p01, p11, p10]);
            out_normals.extend_from_slice(&[bv0, bv0, bv1, bv1]);
            out_indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    (out_vertices, out_normals, out_indices)
}

fn build_vertex_pool(triangles: &[bsp_csg::CsgTriangle]) -> (Vec<Vec3>, Vec<[u32; 3]>, Vec<Vec3>) {
    let mut pool = Vec::new();
    let mut tri_vertices = Vec::with_capacity(triangles.len());
    let mut tri_normals = Vec::with_capacity(triangles.len());

    let find_or_insert = |pool: &mut Vec<Vec3>, vertex: Vec3| -> u32 {
        for (index, existing) in pool.iter().enumerate() {
            if (*existing - vertex).length_squared() < 1e-10 {
                return index as u32;
            }
        }
        let index = pool.len() as u32;
        pool.push(vertex);
        index
    };

    for triangle in triangles {
        let [a, b, c] = triangle.vertices;
        let normal = (b - a).cross(c - a).normalize_or_zero();
        tri_vertices.push([
            find_or_insert(&mut pool, a),
            find_or_insert(&mut pool, b),
            find_or_insert(&mut pool, c),
        ]);
        tri_normals.push(normal);
    }

    (pool, tri_vertices, tri_normals)
}

fn flat_to_indexed(triangles: &[bsp_csg::CsgTriangle]) -> (Vec<Vec3>, Vec<Vec3>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(triangles.len() * 3);
    let mut normals = Vec::with_capacity(triangles.len() * 3);
    let mut indices = Vec::with_capacity(triangles.len() * 3);

    for (triangle_index, triangle) in triangles.iter().enumerate() {
        let [a, b, c] = triangle.vertices;
        let normal = (b - a).cross(c - a).normalize_or_zero();
        let base = (triangle_index * 3) as u32;
        vertices.extend_from_slice(&[a, b, c]);
        normals.extend_from_slice(&[normal, normal, normal]);
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    (vertices, normals, indices)
}

fn get_source_triangles(world: &World, element_id: ElementId) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let mut query = world.try_query::<bevy::ecs::world::EntityRef>().unwrap();
    let entity_ref = query
        .iter(world)
        .find(|entity| entity.get::<ElementId>().copied() == Some(element_id))?;

    try_get_primitive_triangles::<crate::plugins::modeling::primitives::BoxPrimitive>(&entity_ref)
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::primitives::CylinderPrimitive>(
                &entity_ref,
            )
        })
        .or_else(|| {
            try_get_primitive_triangles::<crate::plugins::modeling::primitives::PlanePrimitive>(
                &entity_ref,
            )
        })
        .or_else(|| try_get_primitive_triangles::<ProfileExtrusion>(&entity_ref))
        .or_else(|| try_get_primitive_triangles::<ProfileSweep>(&entity_ref))
        .or_else(|| try_get_primitive_triangles::<ProfileRevolve>(&entity_ref))
        .or_else(|| {
            entity_ref.get::<EvaluatedCsg>().map(|evaluated| {
                evaluated
                    .indices
                    .chunks(3)
                    .filter(|chunk| chunk.len() == 3)
                    .map(|chunk| {
                        bsp_csg::CsgTriangle::new(
                            evaluated.vertices[chunk[0] as usize],
                            evaluated.vertices[chunk[1] as usize],
                            evaluated.vertices[chunk[2] as usize],
                        )
                    })
                    .collect()
            })
        })
        .or_else(|| {
            entity_ref.get::<EvaluatedFeature>().map(|evaluated| {
                evaluated
                    .indices
                    .chunks(3)
                    .filter(|chunk| chunk.len() == 3)
                    .map(|chunk| {
                        bsp_csg::CsgTriangle::new(
                            evaluated.vertices[chunk[0] as usize],
                            evaluated.vertices[chunk[1] as usize],
                            evaluated.vertices[chunk[2] as usize],
                        )
                    })
                    .collect()
            })
        })
        .or_else(|| {
            entity_ref.get::<EvaluatedFillet>().map(|evaluated| {
                evaluated
                    .indices
                    .chunks(3)
                    .filter(|chunk| chunk.len() == 3)
                    .map(|chunk| {
                        bsp_csg::CsgTriangle::new(
                            evaluated.vertices[chunk[0] as usize],
                            evaluated.vertices[chunk[1] as usize],
                            evaluated.vertices[chunk[2] as usize],
                        )
                    })
                    .collect()
            })
        })
}

fn try_get_primitive_triangles<P: Primitive>(
    entity_ref: &bevy::ecs::world::EntityRef,
) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let primitive = entity_ref.get::<P>()?;
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default();
    let mesh = primitive.to_editable_mesh(rotation.0)?;
    Some(bsp_csg::triangles_from_editable_mesh(&mesh))
}

fn draw_evaluated_wireframe(vertices: &[Vec3], indices: &[u32], gizmos: &mut Gizmos, color: Color) {
    for triangle in indices.chunks(3) {
        if triangle.len() != 3 {
            continue;
        }
        let (a, b, c) = (
            vertices[triangle[0] as usize],
            vertices[triangle[1] as usize],
            vertices[triangle[2] as usize],
        );
        gizmos.line(a, b, color);
        gizmos.line(b, c, color);
        gizmos.line(c, a, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bevelled_box_mesh_produces_indexed_geometry() {
        let (vertices, normals, indices) = bevelled_box_mesh(Vec3::splat(1.0), 0.2, 3);
        assert!(!vertices.is_empty());
        assert_eq!(vertices.len(), normals.len());
        assert!(!indices.is_empty());
        assert_eq!(indices.len() % 3, 0);
    }
}
