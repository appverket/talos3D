//! Derived planar picking/evaluation cache. Capabilities build it from their authored
//! geometry when that geometry changes; it never replaces the authored entity.
use super::{bsp_csg::CsgTriangle, snapshots::ray_triangle_intersection};
use crate::{
    capability_registry::{FaceHitCandidate, FaceId},
    plugins::identity::ElementId,
};
use bevy::prelude::*;

#[derive(Clone, Debug)]
pub struct PlanarSurface {
    pub normal: Vec3,
    pub origin: Vec3,
    pub triangles: Vec<[Vec3; 3]>,
}

#[derive(Component, Clone, Debug, Default)]
pub struct PlanarBody {
    pub surfaces: Vec<PlanarSurface>,
    pub triangles: Vec<CsgTriangle>,
}

impl PlanarBody {
    pub fn from_triangles(triangles: Vec<CsgTriangle>) -> Self {
        let mut surfaces: Vec<PlanarSurface> = Vec::new();
        for triangle in &triangles {
            if triangle.normal.length_squared() < 0.5 {
                continue;
            }
            let origin = triangle.vertices[0];
            if let Some(surface) = surfaces.iter_mut().find(|s| {
                s.normal.dot(triangle.normal) > 0.99999
                    && s.normal.dot(origin - s.origin).abs() < 0.0001
            }) {
                surface.triangles.push(triangle.vertices);
            } else {
                surfaces.push(PlanarSurface {
                    normal: triangle.normal,
                    origin,
                    triangles: vec![triangle.vertices],
                });
            }
        }
        Self {
            surfaces,
            triangles,
        }
    }

    pub fn hit_test(
        &self,
        entity: Entity,
        element_id: ElementId,
        ray: Ray3d,
    ) -> Option<FaceHitCandidate> {
        self.surfaces
            .iter()
            .enumerate()
            .flat_map(|(index, surface)| {
                surface.triangles.iter().filter_map(move |t| {
                    let distance = ray_triangle_intersection(ray, t[0], t[1], t[2])?;
                    Some(FaceHitCandidate {
                        entity,
                        element_id,
                        distance,
                        face_id: FaceId(index as u32),
                        generated_face_ref: None,
                        normal: surface.normal,
                        centroid: surface.origin,
                    })
                })
            })
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
    }

    /// Ordered outer boundary for outline/plane consumers. Overlay consumers use
    /// triangles directly, preserving holes and non-convex boundaries.
    pub fn boundary(&self, face: FaceId) -> Option<Vec<Vec3>> {
        let surface = self.surfaces.get(face.0 as usize)?;
        let mut edges: Vec<(Vec3, Vec3)> = Vec::new();
        for t in &surface.triangles {
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                if let Some(j) = edges.iter().position(|(c, d)| {
                    a.distance_squared(*d) < 1e-8 && b.distance_squared(*c) < 1e-8
                }) {
                    edges.swap_remove(j);
                } else {
                    edges.push((a, b));
                }
            }
        }
        let (first, mut next) = edges.pop()?;
        let mut result = vec![first];
        while next.distance_squared(first) > 1e-8 {
            result.push(next);
            let Some(index) = edges
                .iter()
                .position(|(a, _)| a.distance_squared(next) < 1e-8)
            else {
                break;
            };
            next = edges.swap_remove(index).1;
        }
        Some(result)
    }
}
