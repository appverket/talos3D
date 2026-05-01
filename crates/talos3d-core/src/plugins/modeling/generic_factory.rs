use std::marker::PhantomData;

use bevy::{ecs::world::EntityRef, math::Ray3d, prelude::*};
use serde_json::Value;

use crate::{
    authored_entity::BoxedEntity,
    capability_registry::{
        AuthoredEntityFactory, FaceHitCandidate, FaceId, HitCandidate, ModelSummaryAccumulator,
    },
    plugins::{
        identity::ElementId,
        materials::{material_assignment_from_value, MaterialAssignment},
        modeling::{primitives::ShapeRotation, void_declaration::OpeningContext},
    },
};

use super::{
    generic_snapshot::PrimitiveSnapshot, primitive_trait::Primitive,
    snapshots::ray_triangle_intersection,
};

/// Generic factory for any `Primitive` type.
///
/// Implements `AuthoredEntityFactory` by reading the `P` component from ECS
/// entities and wrapping them in `PrimitiveSnapshot<P>`.
pub struct PrimitiveFactory<P: Primitive>(PhantomData<P>);

impl<P: Primitive> PrimitiveFactory<P> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<P: Primitive> Default for PrimitiveFactory<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Primitive + PartialEq> AuthoredEntityFactory for PrimitiveFactory<P> {
    fn type_name(&self) -> &'static str {
        P::TYPE_NAME
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let primitive = entity_ref.get::<P>()?;
        Some(
            PrimitiveSnapshot {
                element_id,
                primitive: primitive.clone(),
                rotation: entity_ref
                    .get::<ShapeRotation>()
                    .copied()
                    .unwrap_or_default(),
                material_assignment: entity_ref.get::<MaterialAssignment>().cloned(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let primitive = P::from_json(data)?;
        // The JSON must also contain element_id and rotation at the envelope level.
        let element_id_val = data
            .get("element_id")
            .ok_or_else(|| "Missing 'element_id' in persisted JSON".to_string())?;
        let element_id: ElementId =
            serde_json::from_value(element_id_val.clone()).map_err(|e| e.to_string())?;
        let rotation = data
            .get("rotation")
            .map(|v| serde_json::from_value::<ShapeRotation>(v.clone()))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let material_assignment = data
            .get("material_assignment")
            .and_then(material_assignment_from_value);
        Ok(PrimitiveSnapshot {
            element_id,
            primitive,
            rotation,
            material_assignment,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let primitive = P::from_json(request)?;
        let element_id = world
            .get_resource::<crate::plugins::identity::ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();
        let rotation = request
            .get("rotation")
            .and_then(|v| serde_json::from_value::<ShapeRotation>(v.clone()).ok())
            .unwrap_or_default();
        let material_assignment = request
            .get("material_assignment")
            .and_then(material_assignment_from_value);
        Ok(PrimitiveSnapshot {
            element_id,
            primitive,
            rotation,
            material_assignment,
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return;
        };
        let Some(primitive) = entity_ref.get::<P>() else {
            return;
        };
        let rotation = entity_ref
            .get::<ShapeRotation>()
            .copied()
            .unwrap_or_default();
        primitive.draw_wireframe(gizmos, rotation.0, color);
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Ok(entity_ref) = world.get_entity(entity) else {
            return 0;
        };
        entity_ref
            .get::<P>()
            .map(|p| p.wireframe_line_count())
            .unwrap_or(0)
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let mut best: Option<HitCandidate> = None;

        let mut query = world.try_query::<EntityRef>().unwrap();
        for entity_ref in query.iter(world) {
            if entity_ref.contains::<OpeningContext>() {
                continue;
            }
            let Some(primitive) = entity_ref.get::<P>() else {
                continue;
            };
            let rotation = entity_ref
                .get::<ShapeRotation>()
                .copied()
                .unwrap_or_default();
            if let Some(distance) = primitive.hit_test_ray(rotation.0, ray) {
                if best.is_none() || distance < best.as_ref().unwrap().distance {
                    best = Some(HitCandidate {
                        entity: entity_ref.id(),
                        distance,
                    });
                }
            }
        }

        best
    }

    fn hit_test_face(&self, world: &World, entity: Entity, ray: Ray3d) -> Option<FaceHitCandidate> {
        let entity_ref = world.get_entity(entity).ok()?;
        let element_id = *entity_ref.get::<ElementId>()?;
        let primitive = entity_ref.get::<P>()?;
        let rotation = entity_ref
            .get::<ShapeRotation>()
            .copied()
            .unwrap_or_default();

        let mesh = primitive.to_editable_mesh(rotation.0)?;

        let mut best: Option<(f32, u32)> = None;
        for (face_idx, face) in mesh.faces.iter().enumerate() {
            if face.half_edge == u32::MAX {
                continue;
            }
            let tris = mesh.triangulate_face(face_idx as u32);
            for tri in &tris {
                let v0 = mesh.vertices[tri[0] as usize];
                let v1 = mesh.vertices[tri[1] as usize];
                let v2 = mesh.vertices[tri[2] as usize];
                if let Some(t) = ray_triangle_intersection(ray, v0, v1, v2) {
                    if best.is_none() || t < best.unwrap().0 {
                        best = Some((t, face_idx as u32));
                    }
                }
            }
        }

        let (distance, face_idx) = best?;
        let face = &mesh.faces[face_idx as usize];
        let face_id = FaceId(face_idx);
        Some(FaceHitCandidate {
            entity,
            element_id,
            distance,
            face_id,
            generated_face_ref: primitive.generated_face_ref(face_id),
            normal: face.normal,
            centroid: mesh.face_centroid(face_idx),
        })
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut query = world.try_query::<EntityRef>().unwrap();
        for entity_ref in query.iter(world) {
            if entity_ref.contains::<OpeningContext>() {
                continue;
            }
            let Some(primitive) = entity_ref.get::<P>() else {
                continue;
            };
            *summary
                .entity_counts
                .entry(P::TYPE_NAME.to_string())
                .or_insert(0) += 1;
            summary.bounding_points.push(primitive.centre());
        }
    }
}
