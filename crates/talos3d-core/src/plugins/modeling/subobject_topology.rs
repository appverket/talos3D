//! Canonical face/edge topology for authored entities.
//!
//! Faces and edges are *generated* subobjects: they are not authored elements,
//! they are derived from the authored primitive or editable mesh. Everything
//! that needs to name one — the Model API, viewport picking, display overrides —
//! must derive it here so that a face or edge means the same thing to an agent
//! over MCP and to a user clicking in the viewport.
//!
//! Before this module the Model API owned a private, `model-api`-gated copy of
//! the derivation and the viewport owned a second partial copy. Two copies of
//! "what is edge 3 of this box" is exactly the second authority the platform
//! rules forbid, so the derivation lives here, unconditionally compiled, and
//! both surfaces call into it.

use bevy::ecs::world::EntityRef;
use bevy::prelude::*;

use crate::capability_registry::{FaceId, GeneratedEdgeRef, GeneratedFaceRef};
use crate::plugins::modeling::{
    editable_mesh::EditableMesh,
    primitive_trait::Primitive,
    primitives::{BoxPrimitive, CylinderPrimitive, PlanePrimitive, ShapeRotation, SpherePrimitive},
    profile::{ProfileExtrusion, ProfileRevolve, ProfileSweep},
};

/// Applies `$body` to the first parametric primitive component present on the
/// entity that yields `Some`.
///
/// The primitive list is written once here. Adding a face/edge-selectable
/// primitive type means editing this macro, not hunting down parallel
/// `or_else` chains in the Model API, the face picker and the edge picker.
macro_rules! first_primitive {
    ($entity_ref:expr, $access:ident, |$primitive:ident| $body:expr) => {{
        let entity_ref = $entity_ref;
        None.or_else(|| {
            entity_ref
                .$access::<BoxPrimitive>()
                .and_then(|$primitive| $body)
        })
        .or_else(|| {
            entity_ref
                .$access::<CylinderPrimitive>()
                .and_then(|$primitive| $body)
        })
        .or_else(|| {
            entity_ref
                .$access::<SpherePrimitive>()
                .and_then(|$primitive| $body)
        })
        .or_else(|| {
            entity_ref
                .$access::<PlanePrimitive>()
                .and_then(|$primitive| $body)
        })
        .or_else(|| {
            entity_ref
                .$access::<ProfileExtrusion>()
                .and_then(|$primitive| $body)
        })
        .or_else(|| {
            entity_ref
                .$access::<ProfileSweep>()
                .and_then(|$primitive| $body)
        })
        .or_else(|| {
            entity_ref
                .$access::<ProfileRevolve>()
                .and_then(|$primitive| $body)
        })
    }};
}

/// The half-edge body used for face/edge interaction on an entity.
///
/// Vertices are in the same authored world space the primitive is authored in,
/// so callers may use them directly for gizmo and overlay geometry.
pub fn evaluated_subobject_mesh(entity_ref: &EntityRef) -> Option<EditableMesh> {
    if let Some(mesh) = entity_ref.get::<EditableMesh>() {
        return Some(mesh.clone());
    }
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default()
        .0;
    first_primitive!(entity_ref, get, |primitive| primitive
        .to_editable_mesh(rotation))
}

/// The stable generated reference for a face index on an entity.
pub fn generated_face_ref_for_entity(
    entity_ref: &EntityRef,
    face_id: FaceId,
) -> Option<GeneratedFaceRef> {
    first_primitive!(entity_ref, get, |primitive| primitive
        .generated_face_ref(face_id))
}

/// Whether the entity's interaction body may have changed since the calling
/// system last ran.
///
/// Lets viewport caches derived from the body — the pickable edge set, for one —
/// rebuild on authored change instead of rebuilding every frame.
pub fn subobject_body_changed(entity_ref: &EntityRef) -> bool {
    if entity_ref
        .get_ref::<EditableMesh>()
        .is_some_and(|mesh| mesh.is_changed())
    {
        return true;
    }
    if entity_ref
        .get_ref::<ShapeRotation>()
        .is_some_and(|rotation| rotation.is_changed())
    {
        return true;
    }
    first_primitive!(entity_ref, get_ref, |primitive| primitive
        .is_changed()
        .then_some(()))
    .is_some()
}

/// Collapses a half-edge pair to the lower of the two indices so that both
/// sides of an edge name the same edge.
pub fn canonical_half_edge_index(mesh: &EditableMesh, half_edge_index: u32) -> u32 {
    let half_edge = &mesh.half_edges[half_edge_index as usize];
    if half_edge.twin == u32::MAX {
        half_edge_index
    } else {
        half_edge_index.min(half_edge.twin)
    }
}

/// The stable generated reference for an edge on an entity.
///
/// An edge is named by the faces it separates rather than by a raw index
/// wherever the adjacent faces themselves have stable names, so the reference
/// survives re-evaluation of the primitive.
pub fn generated_edge_ref_for_half_edge(
    mesh: &EditableMesh,
    entity_ref: &EntityRef,
    half_edge_index: u32,
) -> GeneratedEdgeRef {
    let canonical = canonical_half_edge_index(mesh, half_edge_index);
    let (face_a, face_b) = mesh.faces_adjacent_to_edge(canonical);
    let first = generated_face_ref_for_entity(entity_ref, FaceId(face_a));
    let second =
        face_b.and_then(|face_id| generated_face_ref_for_entity(entity_ref, FaceId(face_id)));
    match (first, second) {
        (Some(a), Some(b)) => {
            let (first, second) = if a.label() <= b.label() {
                (a, b)
            } else {
                (b, a)
            };
            GeneratedEdgeRef::BetweenFaces {
                first,
                second,
                edge_index: canonical,
            }
        }
        (Some(face), None) | (None, Some(face)) => GeneratedEdgeRef::BoundaryOfFace {
            face,
            edge_index: canonical,
        },
        (None, None) => GeneratedEdgeRef::EditableMeshEdge(canonical),
    }
}

/// Every edge of the body exactly once, in canonical half-edge order.
pub fn canonical_edge_indices(mesh: &EditableMesh) -> Vec<u32> {
    let mut seen = vec![false; mesh.half_edges.len()];
    let mut out = Vec::new();
    for half_edge_index in 0..mesh.half_edges.len() as u32 {
        let canonical = canonical_half_edge_index(mesh, half_edge_index);
        if seen[canonical as usize] {
            continue;
        }
        seen[canonical as usize] = true;
        out.push(canonical);
    }
    out
}

/// World-space endpoints of a canonical edge.
pub fn edge_endpoints(mesh: &EditableMesh, half_edge_index: u32) -> Option<(Vec3, Vec3)> {
    let half_edge = mesh.half_edges.get(half_edge_index as usize)?;
    let destination = mesh.half_edges.get(half_edge.next as usize)?.origin;
    Some((
        *mesh.vertices.get(half_edge.origin as usize)?,
        *mesh.vertices.get(destination as usize)?,
    ))
}

/// Resolves a stable face reference back to the face index in the body.
pub fn resolve_face_ref(
    entity_ref: &EntityRef,
    mesh: &EditableMesh,
    target: &GeneratedFaceRef,
) -> Option<FaceId> {
    (0..mesh.faces.len() as u32).find_map(|face_index| {
        (mesh.faces[face_index as usize].half_edge != u32::MAX
            && generated_face_ref_for_entity(entity_ref, FaceId(face_index)).as_ref()
                == Some(target))
        .then_some(FaceId(face_index))
    })
}

/// Resolves a stable edge reference back to the canonical half-edge index.
pub fn resolve_edge_ref(
    entity_ref: &EntityRef,
    mesh: &EditableMesh,
    target: &GeneratedEdgeRef,
) -> Option<u32> {
    canonical_edge_indices(mesh)
        .into_iter()
        .find(|&canonical| &generated_edge_ref_for_half_edge(mesh, entity_ref, canonical) == target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn box_world() -> (World, Entity) {
        let mut world = World::new();
        let entity = world
            .spawn((
                BoxPrimitive {
                    centre: Vec3::new(1.0, 2.0, 3.0),
                    half_extents: Vec3::new(0.5, 1.0, 1.5),
                },
                ShapeRotation::default(),
            ))
            .id();
        (world, entity)
    }

    #[test]
    fn box_body_has_twelve_canonical_edges_with_unique_stable_labels() {
        let (world, entity) = box_world();
        let entity_ref = world.entity(entity);
        let mesh = evaluated_subobject_mesh(&entity_ref).expect("box has an interaction body");

        let edges = canonical_edge_indices(&mesh);
        assert_eq!(edges.len(), 12, "a box has twelve edges");

        let labels: HashSet<String> = edges
            .iter()
            .map(|&canonical| {
                generated_edge_ref_for_half_edge(&mesh, &entity_ref, canonical).label()
            })
            .collect();
        assert_eq!(labels.len(), 12, "every edge names itself distinctly");
    }

    #[test]
    fn box_edges_are_named_by_the_two_faces_they_separate() {
        let (world, entity) = box_world();
        let entity_ref = world.entity(entity);
        let mesh = evaluated_subobject_mesh(&entity_ref).expect("box has an interaction body");

        for canonical in canonical_edge_indices(&mesh) {
            let edge = generated_edge_ref_for_half_edge(&mesh, &entity_ref, canonical);
            assert!(
                matches!(edge, GeneratedEdgeRef::BetweenFaces { .. }),
                "a closed box edge always separates two named faces, got {edge:?}"
            );
        }
    }

    #[test]
    fn edge_references_round_trip_through_resolution() {
        let (world, entity) = box_world();
        let entity_ref = world.entity(entity);
        let mesh = evaluated_subobject_mesh(&entity_ref).expect("box has an interaction body");

        for canonical in canonical_edge_indices(&mesh) {
            let edge = generated_edge_ref_for_half_edge(&mesh, &entity_ref, canonical);
            assert_eq!(
                resolve_edge_ref(&entity_ref, &mesh, &edge),
                Some(canonical),
                "edge {} must resolve back to the half-edge it came from",
                edge.label()
            );
        }
    }

    #[test]
    fn edge_endpoints_are_authored_world_space_corners() {
        let (world, entity) = box_world();
        let entity_ref = world.entity(entity);
        let mesh = evaluated_subobject_mesh(&entity_ref).expect("box has an interaction body");

        let centre = Vec3::new(1.0, 2.0, 3.0);
        let half_extents = Vec3::new(0.5, 1.0, 1.5);
        for canonical in canonical_edge_indices(&mesh) {
            let (start, end) = edge_endpoints(&mesh, canonical).expect("edge has endpoints");
            for point in [start, end] {
                let offset = (point - centre).abs();
                assert!(
                    (offset - half_extents).abs().max_element() < 1e-5,
                    "edge endpoint {point:?} is not a corner of the authored box"
                );
            }
            assert!(
                start.distance(end) > 1e-5,
                "an edge must have two distinct endpoints"
            );
        }
    }

    #[test]
    fn face_references_round_trip_through_resolution() {
        let (world, entity) = box_world();
        let entity_ref = world.entity(entity);
        let mesh = evaluated_subobject_mesh(&entity_ref).expect("box has an interaction body");

        for face_index in 0..mesh.faces.len() as u32 {
            let face_ref = generated_face_ref_for_entity(&entity_ref, FaceId(face_index))
                .expect("box faces are named");
            assert_eq!(
                resolve_face_ref(&entity_ref, &mesh, &face_ref),
                Some(FaceId(face_index))
            );
        }
    }
}
