use std::collections::{HashMap, HashSet, VecDeque};

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};

use crate::{
    authored_entity::{BoxedEntity, PushPullAffordance, PushPullBlockReason},
    capability_registry::GeneratedFaceRef,
    plugins::{
        identity::ElementId,
        modeling::{
            bsp_csg::{self, BooleanOp, CsgTriangle},
            csg::{CsgNode, EvaluatedCsg},
            primitive_trait::Primitive,
            primitives::{
                BoxPrimitive, CylinderPrimitive, PlanePrimitive, ShapeRotation, SpherePrimitive,
            },
            profile::{ProfileExtrusion, ProfileRevolve, ProfileSweep},
            profile_feature::FaceProfileFeature,
        },
    },
};

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GeometryRole {
    SolidRoot,
    FeatureOperation,
    CompositeDefinition,
    SurfaceBody,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TopologyIntent {
    SingleClosedSolid,
    CompositeAssembly,
    OpenSurface,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SemanticInvariant {
    PreserveConnectedness,
    NoImplicitSplit,
    FeatureMustRemainAttached,
    SupportFaceMustRemainValid,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DefinitionInputRef {
    pub element_id: u64,
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_face: Option<GeneratedFaceRef>,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvaluatedBodySummary {
    pub triangle_count: usize,
    pub connected_components: usize,
    pub is_closed_manifold: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounding_box: Option<SemanticBoundingBox>,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticBoundingBox {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeometrySemantics {
    pub role: GeometryRole,
    pub topology_intent: TopologyIntent,
    pub definition_inputs: Vec<DefinitionInputRef>,
    pub feature_ids: Vec<u64>,
    pub invariants: Vec<SemanticInvariant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluated_body: Option<EvaluatedBodySummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub fn geometry_semantics_for_snapshot(
    world: &World,
    snapshot: &BoxedEntity,
) -> Option<GeometrySemantics> {
    match snapshot.type_name() {
        "box" | "cylinder" | "sphere" | "profile_extrusion" | "profile_sweep"
        | "profile_revolve" => geometry_semantics_for_solid_root(world, snapshot),
        "plane" => Some(GeometrySemantics {
            role: GeometryRole::SurfaceBody,
            topology_intent: TopologyIntent::OpenSurface,
            definition_inputs: Vec::new(),
            feature_ids: Vec::new(),
            invariants: Vec::new(),
            evaluated_body: None,
            notes: vec!["Plane is an open surface body, not a closed solid".to_string()],
        }),
        "face_profile_feature" => geometry_semantics_for_feature(world, snapshot),
        "csg" => geometry_semantics_for_csg(world, snapshot),
        _ => None,
    }
}

pub fn semantic_push_pull_affordance(
    world: &World,
    element_id: ElementId,
    generated_face_ref: Option<&GeneratedFaceRef>,
) -> Option<PushPullAffordance> {
    let face_ref = generated_face_ref?;
    let inputs = attached_feature_inputs(world, element_id);
    inputs.iter().find_map(|input| {
        (input.support_face.as_ref() == Some(face_ref)).then_some(PushPullAffordance::Blocked(
            PushPullBlockReason::PreserveSolidIntent,
        ))
    })
}

fn geometry_semantics_for_solid_root(
    world: &World,
    snapshot: &BoxedEntity,
) -> Option<GeometrySemantics> {
    let attached_features = attached_feature_inputs(world, snapshot.element_id());
    let evaluated_body = evaluate_entity_body_summary(world, snapshot.element_id());
    let mut invariants = vec![SemanticInvariant::PreserveConnectedness];
    if !attached_features.is_empty() {
        invariants.push(SemanticInvariant::NoImplicitSplit);
        invariants.push(SemanticInvariant::FeatureMustRemainAttached);
        invariants.push(SemanticInvariant::SupportFaceMustRemainValid);
    }
    Some(GeometrySemantics {
        role: GeometryRole::SolidRoot,
        topology_intent: TopologyIntent::SingleClosedSolid,
        feature_ids: attached_features
            .iter()
            .map(|input| input.element_id)
            .collect(),
        definition_inputs: attached_features,
        invariants,
        evaluated_body,
        notes: Vec::new(),
    })
}

fn geometry_semantics_for_feature(
    world: &World,
    snapshot: &BoxedEntity,
) -> Option<GeometrySemantics> {
    let entity_ref = entity_ref_by_element_id(world, snapshot.element_id())?;
    let feature = entity_ref.get::<FaceProfileFeature>()?;
    Some(GeometrySemantics {
        role: GeometryRole::FeatureOperation,
        topology_intent: TopologyIntent::SingleClosedSolid,
        definition_inputs: vec![DefinitionInputRef {
            element_id: feature.parent.0,
            relation: "parent_solid".to_string(),
            support_face: feature.support_face.clone(),
        }],
        feature_ids: Vec::new(),
        invariants: vec![
            SemanticInvariant::FeatureMustRemainAttached,
            SemanticInvariant::SupportFaceMustRemainValid,
        ],
        evaluated_body: None,
        notes: vec![
            "Feature operation contributes to its parent solid rather than defining an independent body"
                .to_string(),
        ],
    })
}

fn geometry_semantics_for_csg(world: &World, snapshot: &BoxedEntity) -> Option<GeometrySemantics> {
    let entity_ref = entity_ref_by_element_id(world, snapshot.element_id())?;
    let csg = entity_ref.get::<CsgNode>()?;
    Some(GeometrySemantics {
        role: GeometryRole::CompositeDefinition,
        topology_intent: TopologyIntent::SingleClosedSolid,
        definition_inputs: vec![
            DefinitionInputRef {
                element_id: csg.operand_a.0,
                relation: "operand_a".to_string(),
                support_face: None,
            },
            DefinitionInputRef {
                element_id: csg.operand_b.0,
                relation: "operand_b".to_string(),
                support_face: None,
            },
        ],
        feature_ids: Vec::new(),
        invariants: vec![SemanticInvariant::PreserveConnectedness],
        evaluated_body: evaluate_entity_body_summary(world, snapshot.element_id()),
        notes: vec![
            "CSG node is a definition-graph root and stays compatible with future DAG-based modeling"
                .to_string(),
        ],
    })
}

fn attached_feature_inputs(world: &World, parent: ElementId) -> Vec<DefinitionInputRef> {
    let Some(mut query) = world.try_query::<(&ElementId, &FaceProfileFeature)>() else {
        return Vec::new();
    };
    let mut features = query
        .iter(world)
        .filter(|(_, feature)| feature.parent == parent)
        .map(|(feature_id, feature)| DefinitionInputRef {
            element_id: feature_id.0,
            relation: "face_profile_feature".to_string(),
            support_face: feature.support_face.clone(),
        })
        .collect::<Vec<_>>();
    features.sort_by_key(|input| input.element_id);
    features
}

fn evaluate_entity_body_summary(
    world: &World,
    element_id: ElementId,
) -> Option<EvaluatedBodySummary> {
    let triangles = evaluated_entity_triangles(world, element_id)?;
    Some(body_summary_from_triangles(&triangles))
}

pub fn body_summary_from_triangles(triangles: &[CsgTriangle]) -> EvaluatedBodySummary {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut indexed_vertices = Vec::<Vec3>::new();
    let mut indexed_triangles = Vec::<[u32; 3]>::new();
    let mut vertex_map = HashMap::<[u32; 3], u32>::new();
    let mut signed_volume = 0.0f32;

    for triangle in triangles {
        let mut tri = [0u32; 3];
        for (index, vertex) in triangle.vertices.iter().enumerate() {
            min = min.min(*vertex);
            max = max.max(*vertex);
            let quantized = quantize_vec3(*vertex);
            let next_index = if let Some(existing) = vertex_map.get(&quantized) {
                *existing
            } else {
                let next_index = indexed_vertices.len() as u32;
                indexed_vertices.push(*vertex);
                vertex_map.insert(quantized, next_index);
                next_index
            };
            tri[index] = next_index;
        }
        indexed_triangles.push(tri);
        signed_volume +=
            triangle.vertices[0].dot(triangle.vertices[1].cross(triangle.vertices[2])) / 6.0;
    }

    let connected_components = connected_components(&indexed_triangles, indexed_vertices.len());
    let is_closed_manifold = is_closed_manifold(&indexed_triangles);

    EvaluatedBodySummary {
        triangle_count: indexed_triangles.len(),
        connected_components,
        is_closed_manifold,
        volume: is_closed_manifold.then_some(signed_volume.abs()),
        bounding_box: min.is_finite().then_some(SemanticBoundingBox {
            min: [min.x, min.y, min.z],
            max: [max.x, max.y, max.z],
        }),
    }
}

fn quantize_vec3(value: Vec3) -> [u32; 3] {
    const SCALE: f32 = 10_000.0;
    [
        (value.x * SCALE).round().to_bits(),
        (value.y * SCALE).round().to_bits(),
        (value.z * SCALE).round().to_bits(),
    ]
}

fn connected_components(triangles: &[[u32; 3]], vertex_count: usize) -> usize {
    if triangles.is_empty() || vertex_count == 0 {
        return 0;
    }

    let mut adjacency = vec![Vec::<u32>::new(); vertex_count];
    let mut active_vertices = HashSet::<u32>::new();
    for triangle in triangles {
        let [a, b, c] = *triangle;
        adjacency[a as usize].extend([b, c]);
        adjacency[b as usize].extend([a, c]);
        adjacency[c as usize].extend([a, b]);
        active_vertices.extend([a, b, c]);
    }

    let mut visited = HashSet::<u32>::new();
    let mut components = 0usize;
    for start in active_vertices {
        if !visited.insert(start) {
            continue;
        }
        components += 1;
        let mut queue = VecDeque::from([start]);
        while let Some(current) = queue.pop_front() {
            for neighbor in &adjacency[current as usize] {
                if visited.insert(*neighbor) {
                    queue.push_back(*neighbor);
                }
            }
        }
    }
    components
}

fn is_closed_manifold(triangles: &[[u32; 3]]) -> bool {
    if triangles.is_empty() {
        return false;
    }
    let mut edges = HashMap::<(u32, u32), usize>::new();
    for triangle in triangles {
        for (a, b) in [
            (triangle[0], triangle[1]),
            (triangle[1], triangle[2]),
            (triangle[2], triangle[0]),
        ] {
            let edge = if a < b { (a, b) } else { (b, a) };
            *edges.entry(edge).or_insert(0) += 1;
        }
    }
    edges.values().all(|count| *count == 2)
}

pub fn evaluated_entity_triangles(
    world: &World,
    element_id: ElementId,
) -> Option<Vec<CsgTriangle>> {
    let entity_ref = entity_ref_by_element_id(world, element_id)?;
    if let Some(csg) = entity_ref.get::<CsgNode>() {
        return evaluated_csg_triangles(world, csg);
    }
    primitive_or_evaluated_triangles(world, &entity_ref, true)
}

fn evaluated_csg_triangles(world: &World, csg: &CsgNode) -> Option<Vec<CsgTriangle>> {
    let tris_a = evaluated_entity_triangles(world, csg.operand_a)?;
    let tris_b = evaluated_entity_triangles(world, csg.operand_b)?;
    let result = bsp_csg::boolean(&tris_a, &tris_b, csg.op);
    Some(indexed_triangles_to_csg(&result.vertices, &result.indices))
}

fn primitive_or_evaluated_triangles(
    world: &World,
    entity_ref: &EntityRef,
    include_attached_features: bool,
) -> Option<Vec<CsgTriangle>> {
    let mut base = primitive_triangles(entity_ref).or_else(|| {
        entity_ref
            .get::<EvaluatedCsg>()
            .map(|evaluated| indexed_triangles_to_csg(&evaluated.vertices, &evaluated.indices))
    })?;

    if include_attached_features {
        let element_id = entity_ref.get::<ElementId>().copied()?;
        let Some(mut feature_query) =
            world.try_query::<(&ElementId, &FaceProfileFeature, Option<&ShapeRotation>)>()
        else {
            return Some(base);
        };
        let mut features = feature_query
            .iter(world)
            .filter(|(_, feature, _)| feature.parent == element_id)
            .map(|(feature_id, feature, rotation)| {
                (
                    *feature_id,
                    feature.clone(),
                    rotation.copied().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        features.sort_by_key(|(feature_id, _, _)| feature_id.0);
        for (_, feature, rotation) in features {
            let operand = feature.current_operand(rotation.0);
            let operand_mesh = operand.to_editable_mesh(rotation.0)?;
            let operand_tris = bsp_csg::triangles_from_editable_mesh(&operand_mesh);
            let op = if feature.depth < 0.0 {
                BooleanOp::Difference
            } else {
                BooleanOp::Union
            };
            let result = bsp_csg::boolean(&base, &operand_tris, op);
            base = indexed_triangles_to_csg(&result.vertices, &result.indices);
        }
    }

    Some(base)
}

fn primitive_triangles(entity_ref: &EntityRef) -> Option<Vec<CsgTriangle>> {
    try_primitive_triangles::<BoxPrimitive>(entity_ref)
        .or_else(|| try_primitive_triangles::<CylinderPrimitive>(entity_ref))
        .or_else(|| try_primitive_triangles::<SpherePrimitive>(entity_ref))
        .or_else(|| try_primitive_triangles::<PlanePrimitive>(entity_ref))
        .or_else(|| try_primitive_triangles::<ProfileExtrusion>(entity_ref))
        .or_else(|| try_primitive_triangles::<ProfileSweep>(entity_ref))
        .or_else(|| try_primitive_triangles::<ProfileRevolve>(entity_ref))
}

fn try_primitive_triangles<P: crate::plugins::modeling::primitive_trait::Primitive>(
    entity_ref: &EntityRef,
) -> Option<Vec<CsgTriangle>> {
    let primitive = entity_ref.get::<P>()?;
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default();
    let mesh = primitive.to_editable_mesh(rotation.0)?;
    Some(bsp_csg::triangles_from_editable_mesh(&mesh))
}

fn indexed_triangles_to_csg(vertices: &[Vec3], indices: &[u32]) -> Vec<CsgTriangle> {
    indices
        .chunks(3)
        .filter(|chunk| chunk.len() == 3)
        .map(|chunk| {
            CsgTriangle::new(
                vertices[chunk[0] as usize],
                vertices[chunk[1] as usize],
                vertices[chunk[2] as usize],
            )
        })
        .collect()
}

fn entity_ref_by_element_id<'w>(world: &'w World, element_id: ElementId) -> Option<EntityRef<'w>> {
    let mut query = world.try_query::<EntityRef>().unwrap();
    query.iter(world).find(|entity_ref: &EntityRef<'_>| {
        entity_ref.get::<ElementId>().copied() == Some(element_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authored_entity::BoxedEntity,
        plugins::{
            identity::ElementId,
            modeling::{
                generic_snapshot::PrimitiveSnapshot,
                primitives::{BoxPrimitive, ShapeRotation},
                profile::Profile2d,
                profile_feature::FaceProfileFeature,
            },
        },
    };

    #[test]
    fn body_summary_reports_volume_and_components_for_closed_mesh() {
        let triangles = vec![
            CsgTriangle::new(Vec3::ZERO, Vec3::X, Vec3::Y),
            CsgTriangle::new(Vec3::ZERO, Vec3::Y, Vec3::Z),
            CsgTriangle::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            CsgTriangle::new(Vec3::X, Vec3::Z, Vec3::Y),
        ];

        let summary = body_summary_from_triangles(&triangles);
        assert_eq!(summary.connected_components, 1);
        assert!(summary.is_closed_manifold);
        assert!(summary.volume.unwrap() > 0.0);
    }

    #[test]
    fn box_semantics_include_attached_features_and_evaluated_body() {
        let mut world = World::new();
        world.spawn((
            ElementId(1),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::splat(0.5),
            },
            ShapeRotation::default(),
            Visibility::Visible,
        ));
        world.spawn((
            ElementId(2),
            FaceProfileFeature {
                parent: ElementId(1),
                anchor_origin: Vec3::new(0.0, 0.5, 0.0),
                profile: Profile2d::rectangle(0.4, 0.4),
                depth: 0.25,
                support_face: Some(GeneratedFaceRef::BoxFace {
                    axis: 1,
                    positive: true,
                }),
            },
            ShapeRotation::default(),
            Visibility::Visible,
        ));

        let snapshot: BoxedEntity = PrimitiveSnapshot {
            element_id: ElementId(1),
            primitive: BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::splat(0.5),
            },
            rotation: ShapeRotation::default(),
        }
        .into();

        let semantics = geometry_semantics_for_snapshot(&world, &snapshot).unwrap();
        assert_eq!(semantics.role, GeometryRole::SolidRoot);
        assert_eq!(semantics.feature_ids, vec![2]);
        assert!(semantics
            .invariants
            .contains(&SemanticInvariant::NoImplicitSplit));
        assert_eq!(
            semantics.definition_inputs[0].support_face,
            Some(GeneratedFaceRef::BoxFace {
                axis: 1,
                positive: true,
            })
        );
        assert_eq!(
            semantics
                .evaluated_body
                .as_ref()
                .map(|body| body.connected_components),
            Some(1)
        );
    }

    #[test]
    fn support_face_is_semantically_locked_for_parent_push_pull() {
        let mut world = World::new();
        world.spawn((
            ElementId(1),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::splat(0.5),
            },
            ShapeRotation::default(),
            Visibility::Visible,
        ));
        world.spawn((
            ElementId(2),
            FaceProfileFeature {
                parent: ElementId(1),
                anchor_origin: Vec3::new(0.0, 0.5, 0.0),
                profile: Profile2d::rectangle(0.4, 0.4),
                depth: 0.25,
                support_face: Some(GeneratedFaceRef::BoxFace {
                    axis: 1,
                    positive: true,
                }),
            },
            ShapeRotation::default(),
            Visibility::Visible,
        ));

        assert_eq!(
            semantic_push_pull_affordance(
                &world,
                ElementId(1),
                Some(&GeneratedFaceRef::BoxFace {
                    axis: 1,
                    positive: true,
                }),
            ),
            Some(PushPullAffordance::Blocked(
                PushPullBlockReason::PreserveSolidIntent
            ))
        );
    }
}
