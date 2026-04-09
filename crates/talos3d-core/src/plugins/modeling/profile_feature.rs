use std::any::Any;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        scalar_from_json, vec3_from_json, AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo,
        HandleKind, PropertyFieldDef, PropertyValue, PropertyValueKind, PushPullAffordance,
        PushPullBlockReason,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityRegistry, FaceHitCandidate, FaceId, GeneratedFaceRef,
        HitCandidate,
    },
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        modeling::{
            bsp_csg::{self, BooleanOp},
            csg::EvaluatedCsg,
            mesh_generation::{NeedsEvaluation, NeedsMesh},
            primitive_trait::Primitive,
            primitives::ShapeRotation,
            profile::{Profile2d, ProfileExtrusion, ProfileRevolve, ProfileSegment, ProfileSweep},
            snapshots::ray_triangle_intersection,
        },
    },
};

const DEFAULT_FEATURE_DEPTH: f32 = 0.02;
const MIN_FEATURE_DEPTH: f32 = 0.005;
const FEATURE_SELECTION_SURFACE_EPSILON: f32 = 0.001;

fn parse_optional_rotation(value: Option<&Value>) -> Result<Quat, String> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Ok(Quat::IDENTITY);
    };
    if values.len() != 4 {
        return Err("rotation must be [x, y, z, w]".to_string());
    }
    Ok(Quat::from_xyzw(
        scalar_from_json(&values[0])?,
        scalar_from_json(&values[1])?,
        scalar_from_json(&values[2])?,
        scalar_from_json(&values[3])?,
    ))
}

fn canonicalize_feature_profile(profile: Profile2d) -> Profile2d {
    if profile.is_ccw() {
        profile
    } else {
        profile.reversed()
    }
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceProfileFeature {
    pub parent: ElementId,
    pub anchor_origin: Vec3,
    pub profile: Profile2d,
    pub depth: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_face: Option<GeneratedFaceRef>,
}

#[derive(Component, Debug, Clone)]
pub struct FeatureOperand {
    pub owner: ElementId,
}

#[derive(Component, Debug, Clone)]
pub struct EvaluatedFeature {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct FaceProfileFeatureSnapshot {
    pub element_id: ElementId,
    pub feature: FaceProfileFeature,
    pub rotation: ShapeRotation,
}

impl PartialEq for FaceProfileFeatureSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id
            && self.feature == other.feature
            && self.rotation == other.rotation
    }
}

impl FaceProfileFeature {
    pub fn side_face_count(&self) -> u32 {
        self.profile.segment_count() as u32
    }

    pub fn current_operand(&self, rotation: Quat) -> ProfileExtrusion {
        ProfileExtrusion {
            centre: self.anchor_origin + rotation * Vec3::Y * (self.depth * 0.5),
            profile: self.profile.clone(),
            height: self.depth.abs().max(MIN_FEATURE_DEPTH),
        }
    }

    fn cap_operand_face_id(&self) -> FaceId {
        if self.depth >= 0.0 {
            FaceId(0)
        } else {
            FaceId(1)
        }
    }

    fn operand_face_for_feature_face(&self, feature_face_id: FaceId) -> Option<FaceId> {
        match feature_face_id.0 {
            0 => Some(self.cap_operand_face_id()),
            2.. => {
                let segment_index = feature_face_id.0 - 2;
                (segment_index < self.side_face_count()).then_some(FaceId(feature_face_id.0))
            }
            _ => None,
        }
    }

    pub fn generated_face_ref(&self, feature_face_id: FaceId) -> Option<GeneratedFaceRef> {
        match feature_face_id.0 {
            0 => Some(GeneratedFaceRef::FeatureCap),
            1 => Some(GeneratedFaceRef::FeatureAnchor),
            side_face if side_face >= 2 => {
                let segment_index = (side_face - 2) as usize;
                if segment_index < self.profile.segments.len() {
                    match self.profile.segments[segment_index] {
                        ProfileSegment::LineTo { .. } => {
                            Some(GeneratedFaceRef::FeatureSideSegment(segment_index as u32))
                        }
                        ProfileSegment::ArcTo { .. } => Some(
                            GeneratedFaceRef::FeatureSideArcSegment(segment_index as u32),
                        ),
                    }
                } else if segment_index == self.profile.segments.len()
                    && self.profile.segment_count() > self.profile.segments.len()
                {
                    Some(GeneratedFaceRef::FeatureSideClosingSegment)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn visible_face_normal(&self, operand_face_id: FaceId, operand_normal: Vec3) -> Vec3 {
        match operand_face_id.0 {
            0 | 1 | 2.. if self.depth < 0.0 => -operand_normal,
            _ => operand_normal,
        }
    }

    fn canonical_face_normal(
        &self,
        rotation: Quat,
        feature_face_id: FaceId,
        face_centroid: Vec3,
        mesh_normal: Vec3,
    ) -> Vec3 {
        match feature_face_id.0 {
            0 => {
                let extrusion_axis = rotation * Vec3::Y;
                if self.depth >= 0.0 {
                    extrusion_axis
                } else {
                    -extrusion_axis
                }
            }
            side_face if side_face >= 2 => {
                let mut normal = mesh_normal.normalize_or_zero();
                let feature_center = self.current_operand(rotation).centre;
                let radial = (face_centroid - feature_center)
                    - (rotation * Vec3::Y)
                        * (face_centroid - feature_center).dot(rotation * Vec3::Y);
                if radial.length_squared() > 1e-4 && normal.dot(radial) < 0.0 {
                    normal = -normal;
                }
                if self.depth < 0.0 {
                    -normal
                } else {
                    normal
                }
            }
            _ => mesh_normal.normalize_or_zero(),
        }
    }
}

impl AuthoredEntity for FaceProfileFeatureSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "face_profile_feature"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!(
            "Profile feature on {} ({:.2}m)",
            self.feature.parent.0, self.feature.depth
        )
    }

    fn center(&self) -> Vec3 {
        self.feature.current_operand(self.rotation.0).centre
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        Self {
            element_id: self.element_id,
            feature: FaceProfileFeature {
                anchor_origin: self.feature.anchor_origin + delta,
                ..self.feature.clone()
            },
            rotation: self.rotation,
        }
        .into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        Self {
            element_id: self.element_id,
            feature: FaceProfileFeature {
                anchor_origin: rotation * self.feature.anchor_origin,
                ..self.feature.clone()
            },
            rotation: ShapeRotation(rotation * self.rotation.0),
        }
        .into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let operand = self.feature.current_operand(self.rotation.0);
        let scaled_depth = self.feature.depth.signum() * self.feature.depth.abs() * factor.y.abs();
        Self {
            element_id: self.element_id,
            feature: FaceProfileFeature {
                parent: self.feature.parent,
                anchor_origin: crate::plugins::math::scale_point_around_center(
                    self.feature.anchor_origin,
                    center,
                    factor,
                ),
                profile: operand
                    .profile
                    .scaled(Vec2::new(factor.x.abs(), factor.z.abs()), Vec2::ZERO),
                depth: scaled_depth,
                support_face: self.feature.support_face.clone(),
            },
            rotation: self.rotation,
        }
        .into()
    }

    fn push_pull(&self, face_id: FaceId, distance: f32) -> Option<BoxedEntity> {
        match face_id.0 {
            0 => Some(
                Self {
                    element_id: self.element_id,
                    feature: FaceProfileFeature {
                        depth: self.feature.depth + distance,
                        ..self.feature.clone()
                    },
                    rotation: self.rotation,
                }
                .into(),
            ),
            // Side-face push/pull is intentionally unsupported for face profile features.
            // The authored feature model is cap-depth based, not arbitrary side-face splitting.
            _ => None,
        }
    }

    fn push_pull_affordance(&self, face_id: FaceId) -> PushPullAffordance {
        match self.feature.generated_face_ref(face_id) {
            Some(GeneratedFaceRef::FeatureCap) => PushPullAffordance::Allowed,
            Some(
                GeneratedFaceRef::FeatureAnchor
                | GeneratedFaceRef::FeatureSideSegment(_)
                | GeneratedFaceRef::FeatureSideArcSegment(_)
                | GeneratedFaceRef::FeatureSideClosingSegment,
            ) => PushPullAffordance::Blocked(PushPullBlockReason::CapOnly),
            None => PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace),
            _ => PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace),
        }
    }

    fn transform_parent(&self) -> Option<ElementId> {
        Some(self.feature.parent)
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "depth",
                "depth",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.feature.depth)),
                true,
            ),
            read_only_property_field(
                "parent",
                "parent",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.feature.parent.0.to_string())),
            ),
            read_only_property_field(
                "support_face",
                "support_face",
                PropertyValueKind::Text,
                self.feature
                    .support_face
                    .as_ref()
                    .map(|face| PropertyValue::Text(face.label())),
            ),
            property_field(
                "anchor_origin",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.feature.anchor_origin)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "depth" => snapshot.feature.depth = scalar_from_json(value)?,
            "anchor_origin" => snapshot.feature.anchor_origin = vec3_from_json(value)?,
            "profile" => {
                snapshot.feature.profile = serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid profile: {e}"))?;
            }
            _ => {
                return Err(invalid_property_error(
                    "face_profile_feature",
                    &["depth", "anchor_origin", "profile"],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        let operand = self.feature.current_operand(self.rotation.0);
        let cap_center = operand.centre
            + self.rotation.0 * Vec3::Y * (self.feature.depth.signum() * operand.height * 0.5);
        vec![
            HandleInfo {
                id: "anchor".to_string(),
                position: self.feature.anchor_origin,
                kind: HandleKind::Center,
                label: "Anchor".to_string(),
            },
            HandleInfo {
                id: "cap".to_string(),
                position: cap_center,
                kind: HandleKind::Parameter,
                label: "Feature cap".to_string(),
            },
        ]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        self.feature
            .current_operand(self.rotation.0)
            .bounds(self.rotation.0)
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "element_id": self.element_id.0,
            "parent": self.feature.parent.0,
            "anchor_origin": [self.feature.anchor_origin.x, self.feature.anchor_origin.y, self.feature.anchor_origin.z],
            "depth": self.feature.depth,
            "profile": self.feature.profile,
            "support_face": self.feature.support_face,
            "rotation": [
                self.rotation.0.x,
                self.rotation.0.y,
                self.rotation.0.z,
                self.rotation.0.w
            ],
        })
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.feature.clone(),
                self.rotation,
                NeedsEvaluation,
                Visibility::Visible,
            ));
        } else {
            world.spawn((
                self.element_id,
                self.feature.clone(),
                self.rotation,
                NeedsEvaluation,
                Visibility::Visible,
            ));
        }
        if let Some(parent_entity) = find_entity_by_element_id(world, self.feature.parent) {
            world.entity_mut(parent_entity).insert((
                FeatureOperand {
                    owner: self.element_id,
                },
                Visibility::Hidden,
            ));
        }
    }

    fn apply_with_previous(&self, world: &mut World, previous: Option<&dyn AuthoredEntity>) {
        if let Some(previous) =
            previous.and_then(|snapshot| snapshot.as_any().downcast_ref::<Self>())
        {
            if previous.feature.parent != self.feature.parent {
                if let Some(previous_parent_entity) =
                    find_entity_by_element_id(world, previous.feature.parent)
                {
                    world
                        .entity_mut(previous_parent_entity)
                        .remove::<FeatureOperand>()
                        .insert(Visibility::Visible);
                }
            }
        }
        self.apply_to(world);
    }

    fn remove_from(&self, world: &mut World) {
        if let Some(parent_entity) = find_entity_by_element_id(world, self.feature.parent) {
            world
                .entity_mut(parent_entity)
                .remove::<FeatureOperand>()
                .insert(Visibility::Visible);
        }
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        None
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        self.feature
            .current_operand(self.rotation.0)
            .draw_wireframe(gizmos, self.rotation.0, color);
    }

    fn preview_line_count(&self) -> usize {
        self.feature
            .current_operand(self.rotation.0)
            .wireframe_line_count()
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl From<FaceProfileFeatureSnapshot> for BoxedEntity {
    fn from(snapshot: FaceProfileFeatureSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

pub struct FaceProfileFeatureFactory;

impl AuthoredEntityFactory for FaceProfileFeatureFactory {
    fn type_name(&self) -> &'static str {
        "face_profile_feature"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        Some(
            FaceProfileFeatureSnapshot {
                element_id: *entity_ref.get::<ElementId>()?,
                feature: entity_ref.get::<FaceProfileFeature>()?.clone(),
                rotation: entity_ref
                    .get::<ShapeRotation>()
                    .copied()
                    .unwrap_or_default(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let parent = data
            .get("parent")
            .and_then(Value::as_u64)
            .map(ElementId)
            .ok_or("Missing parent")?;
        let anchor_origin =
            vec3_from_json(data.get("anchor_origin").ok_or("Missing anchor_origin")?)?;
        let depth = scalar_from_json(data.get("depth").ok_or("Missing depth")?)?;
        let profile: Profile2d =
            serde_json::from_value(data.get("profile").cloned().ok_or("Missing profile")?)
                .map_err(|e| format!("Invalid profile: {e}"))?;
        let rotation = parse_optional_rotation(data.get("rotation"))?;
        let support_face = data
            .get("support_face")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| format!("Invalid support_face: {e}"))?;
        let element_id = data
            .get("element_id")
            .and_then(Value::as_u64)
            .map(ElementId)
            .unwrap_or(ElementId(0));
        Ok(FaceProfileFeatureSnapshot {
            element_id,
            feature: FaceProfileFeature {
                parent,
                anchor_origin,
                profile: canonicalize_feature_profile(profile),
                depth,
                support_face,
            },
            rotation: ShapeRotation(rotation),
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or("ElementIdAllocator not available")?
            .next_id();
        let parent = request
            .get("parent")
            .and_then(Value::as_u64)
            .map(ElementId)
            .ok_or("Missing parent")?;
        let anchor_origin = vec3_from_json(
            request
                .get("anchor_origin")
                .ok_or("Missing anchor_origin")?,
        )?;
        let depth = request
            .get("depth")
            .map(scalar_from_json)
            .transpose()?
            .unwrap_or(DEFAULT_FEATURE_DEPTH);
        let profile: Profile2d =
            serde_json::from_value(request.get("profile").cloned().ok_or("Missing profile")?)
                .map_err(|e| format!("Invalid profile: {e}"))?;
        let rotation = parse_optional_rotation(request.get("rotation"))?;
        let support_face = request
            .get("support_face")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| format!("Invalid support_face: {e}"))?;
        Ok(FaceProfileFeatureSnapshot {
            element_id,
            feature: FaceProfileFeature {
                parent,
                anchor_origin,
                profile: canonicalize_feature_profile(profile),
                depth,
                support_face,
            },
            rotation: ShapeRotation(rotation),
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Some(entity_ref) = world.get_entity(entity).ok() else {
            return;
        };
        let Some(feature) = entity_ref.get::<FaceProfileFeature>() else {
            return;
        };
        let rotation = entity_ref
            .get::<ShapeRotation>()
            .copied()
            .unwrap_or_default();

        if let Some(parent_entity) = find_entity_by_element_id_ref(world, feature.parent) {
            let registry = world.resource::<CapabilityRegistry>();
            if let Ok(parent_ref) = world.get_entity(parent_entity) {
                if let Some(parent_snapshot) = registry.capture_snapshot(&parent_ref, world) {
                    if let Some(parent_factory) = registry.factory_for(parent_snapshot.type_name())
                    {
                        parent_factory.draw_selection(world, parent_entity, gizmos, color);
                    }
                }
            }
        }

        feature
            .current_operand(rotation.0)
            .draw_wireframe(gizmos, rotation.0, color);
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Some(entity_ref) = world.get_entity(entity).ok() else {
            return 0;
        };
        let Some(feature) = entity_ref.get::<FaceProfileFeature>() else {
            return 0;
        };
        let rotation = entity_ref
            .get::<ShapeRotation>()
            .copied()
            .unwrap_or_default();

        let parent_lines = find_entity_by_element_id_ref(world, feature.parent)
            .and_then(|parent_entity| {
                let registry = world.resource::<CapabilityRegistry>();
                let parent_ref = world.get_entity(parent_entity).ok()?;
                let parent_snapshot = registry.capture_snapshot(&parent_ref, world)?;
                let parent_factory = registry.factory_for(parent_snapshot.type_name())?;
                Some(parent_factory.selection_line_count(world, parent_entity))
            })
            .unwrap_or(0);

        parent_lines + feature.current_operand(rotation.0).wireframe_line_count()
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let mut best: Option<HitCandidate> = None;
        let mut query = world.try_query::<(Entity, &FaceProfileFeature)>().unwrap();
        for (entity, feature) in query.iter(world) {
            let Some(surface_distance) = evaluated_feature_hit_distance(world, entity, ray) else {
                continue;
            };

            let local_feature_distance =
                face_profile_feature_hit_test(world, entity, ray).map(|hit| hit.distance);
            let selects_feature = local_feature_distance.is_some_and(|distance| {
                (distance - surface_distance).abs() <= FEATURE_SELECTION_SURFACE_EPSILON
            });
            let hit_entity = if selects_feature {
                entity
            } else {
                find_entity_by_element_id_ref(world, feature.parent).unwrap_or(entity)
            };
            let hit_distance = local_feature_distance
                .filter(|distance| {
                    (*distance - surface_distance).abs() <= FEATURE_SELECTION_SURFACE_EPSILON
                })
                .unwrap_or(surface_distance);

            if best.is_none() || hit_distance < best.as_ref().unwrap().distance {
                best = Some(HitCandidate {
                    entity: hit_entity,
                    distance: hit_distance,
                });
            }
        }
        best
    }

    fn hit_test_face(&self, world: &World, entity: Entity, ray: Ray3d) -> Option<FaceHitCandidate> {
        face_profile_feature_hit_test(world, entity, ray)
    }

    fn collect_delete_dependencies(
        &self,
        world: &World,
        requested_ids: &[ElementId],
        out: &mut Vec<ElementId>,
    ) {
        let Some(mut q) = world.try_query::<(&ElementId, &FaceProfileFeature)>() else {
            return;
        };
        for (feature_id, feature) in q.iter(world) {
            if requested_ids.contains(&feature.parent) {
                out.push(*feature_id);
            }
        }
    }
}

pub fn make_face_profile_feature_snapshot(
    element_id: ElementId,
    parent: ElementId,
    profile: Profile2d,
    anchor_origin: Vec3,
    rotation: Quat,
    support_face: Option<GeneratedFaceRef>,
) -> FaceProfileFeatureSnapshot {
    FaceProfileFeatureSnapshot {
        element_id,
        feature: FaceProfileFeature {
            parent,
            anchor_origin,
            profile: canonicalize_feature_profile(profile),
            depth: DEFAULT_FEATURE_DEPTH,
            support_face,
        },
        rotation: ShapeRotation(rotation),
    }
}

pub fn feature_face_vertices(world: &World, entity: Entity, face_id: FaceId) -> Option<Vec<Vec3>> {
    let entity_ref = world.get_entity(entity).ok()?;
    let feature = entity_ref.get::<FaceProfileFeature>()?;
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default();
    let operand = feature.current_operand(rotation.0);
    let mesh = operand.to_editable_mesh(rotation.0)?;
    let operand_face_id = feature.operand_face_for_feature_face(face_id)?;
    let fi = operand_face_id.0;
    if (fi as usize) >= mesh.faces.len() || mesh.faces[fi as usize].half_edge == u32::MAX {
        return None;
    }
    let vis = mesh.vertices_of_face(fi);
    Some(vis.iter().map(|&vi| mesh.vertices[vi as usize]).collect())
}

pub fn face_profile_feature_hit_test(
    world: &World,
    entity: Entity,
    ray: Ray3d,
) -> Option<FaceHitCandidate> {
    let entity_ref = world.get_entity(entity).ok()?;
    let feature = entity_ref.get::<FaceProfileFeature>()?;
    let element_id = *entity_ref.get::<ElementId>()?;
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default();
    let operand = feature.current_operand(rotation.0);
    let mesh = operand.to_editable_mesh(rotation.0)?;

    let mut best: Option<(f32, FaceId, FaceId)> = None;
    let max_side_face_id = 2 + feature.side_face_count();

    for feature_face_index in 0..max_side_face_id {
        if feature_face_index == 1 {
            continue;
        }
        let feature_face_id = FaceId(feature_face_index);
        let operand_face_id = match feature.operand_face_for_feature_face(feature_face_id) {
            Some(face_id) => face_id,
            None => continue,
        };
        let face = &mesh.faces[operand_face_id.0 as usize];
        if face.half_edge == u32::MAX {
            continue;
        }
        for tri in mesh.triangulate_face(operand_face_id.0) {
            let v0 = mesh.vertices[tri[0] as usize];
            let v1 = mesh.vertices[tri[1] as usize];
            let v2 = mesh.vertices[tri[2] as usize];
            if let Some(t) = ray_triangle_intersection(ray, v0, v1, v2) {
                if best.is_none() || t < best.unwrap().0 {
                    best = Some((t, feature_face_id, operand_face_id));
                }
            }
        }
    }

    let (distance, feature_face_id, operand_face_id) = best?;
    let operand_face = &mesh.faces[operand_face_id.0 as usize];
    let centroid = mesh.face_centroid(operand_face_id.0);
    let normal = feature.canonical_face_normal(
        rotation.0,
        feature_face_id,
        centroid,
        feature.visible_face_normal(operand_face_id, operand_face.normal),
    );

    Some(FaceHitCandidate {
        entity,
        element_id,
        distance,
        face_id: feature_face_id,
        generated_face_ref: feature.generated_face_ref(feature_face_id),
        normal,
        centroid,
    })
}

fn evaluated_feature_hit_distance(world: &World, entity: Entity, ray: Ray3d) -> Option<f32> {
    let entity_ref = world.get_entity(entity).ok()?;
    let evaluated = entity_ref.get::<EvaluatedFeature>()?;
    let mut nearest = f32::INFINITY;

    for tri in evaluated.indices.chunks(3) {
        if tri.len() != 3 {
            continue;
        }
        let v0 = evaluated.vertices[tri[0] as usize];
        let v1 = evaluated.vertices[tri[1] as usize];
        let v2 = evaluated.vertices[tri[2] as usize];
        if let Some(distance) = ray_triangle_intersection(ray, v0, v1, v2) {
            nearest = nearest.min(distance);
        }
    }

    (nearest.is_finite()).then_some(nearest)
}

pub fn propagate_feature_parent_changes(
    mut commands: Commands,
    changed_parents: Query<&FeatureOperand, With<NeedsMesh>>,
    features: Query<(Entity, &ElementId), With<FaceProfileFeature>>,
) {
    for operand in &changed_parents {
        for (feature_entity, feature_id) in &features {
            if *feature_id == operand.owner {
                commands.entity(feature_entity).try_insert(NeedsEvaluation);
            }
        }
    }
}

pub fn evaluate_face_profile_features(
    mut commands: Commands,
    dirty_features: Query<
        (Entity, &FaceProfileFeature, Option<&ShapeRotation>),
        With<NeedsEvaluation>,
    >,
    world: &World,
) {
    for (entity, feature, rotation) in &dirty_features {
        let Some(parent_entity) = find_entity_by_element_id_ref(world, feature.parent) else {
            continue;
        };
        let Some(parent_tris) = get_entity_triangles(world, parent_entity) else {
            continue;
        };
        let operand = feature.current_operand(rotation.copied().unwrap_or_default().0);
        let Some(operand_mesh) = operand.to_editable_mesh(rotation.copied().unwrap_or_default().0)
        else {
            continue;
        };
        let operand_tris = bsp_csg::triangles_from_editable_mesh(&operand_mesh);
        let op = if feature.depth < 0.0 {
            BooleanOp::Difference
        } else {
            BooleanOp::Union
        };
        let result = bsp_csg::boolean(&parent_tris, &operand_tris, op);
        commands.entity(entity).try_insert((
            EvaluatedFeature {
                vertices: result.vertices,
                normals: result.normals,
                indices: result.indices,
            },
            NeedsMesh,
        ));
        commands.entity(entity).remove::<NeedsEvaluation>();
    }
}

fn find_entity_by_element_id_ref(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
    q.iter(world)
        .find_map(|(entity, eid)| (*eid == element_id).then_some(entity))
}

fn get_entity_triangles(world: &World, entity: Entity) -> Option<Vec<bsp_csg::CsgTriangle>> {
    let entity_ref = world.get_entity(entity).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        cursor::DrawingPlane,
        modeling::{bsp_csg, primitives::BoxPrimitive},
    };

    #[test]
    fn feature_cap_ref_is_stable_across_depth_sign() {
        let feature = FaceProfileFeature {
            parent: ElementId(1),
            anchor_origin: Vec3::ZERO,
            profile: Profile2d::rectangle(1.0, 1.0),
            depth: -0.5,
            support_face: None,
        };
        assert_eq!(
            feature.generated_face_ref(FaceId(0)),
            Some(GeneratedFaceRef::FeatureCap)
        );
        assert_eq!(
            feature.generated_face_ref(FaceId(2)),
            Some(GeneratedFaceRef::FeatureSideSegment(0))
        );
    }

    #[test]
    fn pushing_feature_cap_updates_signed_depth() {
        let snapshot = FaceProfileFeatureSnapshot {
            element_id: ElementId(10),
            feature: FaceProfileFeature {
                parent: ElementId(1),
                anchor_origin: Vec3::ZERO,
                profile: Profile2d::rectangle(1.0, 1.0),
                depth: -0.4,
                support_face: None,
            },
            rotation: ShapeRotation::default(),
        };
        let result = snapshot.push_pull(FaceId(0), 0.1).unwrap();
        let result = result
            .0
            .as_any()
            .downcast_ref::<FaceProfileFeatureSnapshot>()
            .unwrap();
        assert!((result.feature.depth + 0.3).abs() < 1e-6);
    }

    #[test]
    fn feature_cap_normal_tracks_positive_extrusion_axis() {
        let feature = FaceProfileFeature {
            parent: ElementId(1),
            anchor_origin: Vec3::ZERO,
            profile: Profile2d::rectangle(1.0, 1.0),
            depth: 0.5,
            support_face: None,
        };
        let rotation = Quat::from_rotation_z(0.37) * Quat::from_rotation_x(-0.21);
        let normal = feature.canonical_face_normal(rotation, FaceId(0), Vec3::ZERO, Vec3::NEG_Y);
        assert!(normal.abs_diff_eq(rotation * Vec3::Y, 1e-6));
    }

    #[test]
    fn feature_cap_normal_tracks_negative_extrusion_axis() {
        let feature = FaceProfileFeature {
            parent: ElementId(1),
            anchor_origin: Vec3::ZERO,
            profile: Profile2d::rectangle(1.0, 1.0),
            depth: -0.5,
            support_face: None,
        };
        let rotation = Quat::from_rotation_y(0.29) * Quat::from_rotation_x(0.13);
        let normal = feature.canonical_face_normal(rotation, FaceId(0), Vec3::ZERO, Vec3::Y);
        assert!(normal.abs_diff_eq(-(rotation * Vec3::Y), 1e-6));
    }

    #[test]
    fn make_face_profile_feature_snapshot_canonicalizes_profile_winding() {
        let cw_profile = Profile2d::rectangle(1.0, 1.0).reversed();
        assert!(!cw_profile.is_ccw());

        let snapshot = make_face_profile_feature_snapshot(
            ElementId(11),
            ElementId(1),
            cw_profile,
            Vec3::ZERO,
            Quat::IDENTITY,
            None,
        );

        assert!(
            snapshot.feature.profile.is_ccw(),
            "face profile features should canonicalize authored profiles to CCW winding"
        );
    }

    #[test]
    fn feature_side_push_pull_is_unsupported() {
        let snapshot = FaceProfileFeatureSnapshot {
            element_id: ElementId(12),
            feature: FaceProfileFeature {
                parent: ElementId(1),
                anchor_origin: Vec3::ZERO,
                profile: Profile2d::rectangle(1.0, 1.0),
                depth: 0.5,
                support_face: None,
            },
            rotation: ShapeRotation::default(),
        };

        assert_eq!(
            snapshot.push_pull_affordance(FaceId(2)),
            PushPullAffordance::Blocked(PushPullBlockReason::CapOnly)
        );
        assert!(
            snapshot.push_pull(FaceId(2), 0.2).is_none(),
            "face profile features should support cap-only push/pull"
        );
    }

    #[test]
    fn feature_hit_test_proxies_inherited_parent_surface_to_parent_entity() {
        let mut world = World::new();

        let parent_entity = world
            .spawn((
                ElementId(1),
                BoxPrimitive {
                    centre: Vec3::ZERO,
                    half_extents: Vec3::ONE,
                },
                ShapeRotation::default(),
                Visibility::Hidden,
                FeatureOperand {
                    owner: ElementId(2),
                },
            ))
            .id();

        let face_centroid = Vec3::new(1.0, 0.0, 0.0);
        let face_normal = Vec3::X;
        let plane = DrawingPlane::from_face(face_centroid, face_normal);
        let world_points = [
            Vec3::new(1.0, -0.3, -0.3),
            Vec3::new(1.0, -0.3, 0.3),
            Vec3::new(1.0, 0.3, 0.3),
            Vec3::new(1.0, 0.3, -0.3),
        ];
        let points_2d: Vec<Vec2> = world_points
            .iter()
            .map(|point| plane.project_to_2d(*point))
            .collect();
        let profile = Profile2d {
            start: points_2d[0],
            segments: points_2d[1..]
                .iter()
                .map(|&to| ProfileSegment::LineTo { to })
                .collect(),
        };
        let (pmin, pmax) = profile.bounds_2d();
        let mid_2d = (pmin + pmax) * 0.5;
        let centred_profile = canonicalize_feature_profile(profile.translated(-mid_2d));
        let rotation = Quat::from_mat3(&Mat3::from_cols(
            plane.tangent,
            plane.normal,
            plane.bitangent,
        ));
        let feature = FaceProfileFeature {
            parent: ElementId(1),
            anchor_origin: plane.to_world(mid_2d),
            profile: centred_profile,
            depth: 0.5,
            support_face: Some(GeneratedFaceRef::BoxFace {
                axis: 1,
                positive: true,
            }),
        };
        let feature_entity = world
            .spawn((
                ElementId(2),
                feature.clone(),
                ShapeRotation(rotation),
                Visibility::Visible,
            ))
            .id();

        let parent_mesh = world
            .get::<BoxPrimitive>(parent_entity)
            .unwrap()
            .to_editable_mesh(Quat::IDENTITY)
            .unwrap();
        let operand_mesh = feature
            .current_operand(rotation)
            .to_editable_mesh(rotation)
            .unwrap();
        let result = bsp_csg::boolean(
            &bsp_csg::triangles_from_editable_mesh(&parent_mesh),
            &bsp_csg::triangles_from_editable_mesh(&operand_mesh),
            BooleanOp::Union,
        );
        world.entity_mut(feature_entity).insert(EvaluatedFeature {
            vertices: result.vertices,
            normals: result.normals,
            indices: result.indices,
        });

        let ray_origin = Vec3::new(3.0, 0.8, 0.0);
        let ray_target = Vec3::new(1.0, 0.8, 0.0);
        let ray = Ray3d::new(ray_origin, Dir3::new(ray_target - ray_origin).unwrap());
        let hit = FaceProfileFeatureFactory.hit_test(&world, ray).unwrap();

        assert_eq!(
            hit.entity, parent_entity,
            "a hit on the inherited box surface should proxy selection back to the parent"
        );
    }
}
