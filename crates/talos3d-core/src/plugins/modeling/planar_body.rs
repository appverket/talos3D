//! Derived planar picking/evaluation cache. Capabilities build it from their authored
//! geometry when that geometry changes; it never replaces the authored entity.
use super::{bsp_csg::CsgTriangle, snapshots::ray_triangle_intersection};
use crate::{
    capability_registry::{FaceHitCandidate, FaceId, GeneratedFaceRef},
    plugins::identity::ElementId,
};
use bevy::prelude::*;

pub(crate) const PLANAR_SURFACE_DISTANCE_TOLERANCE: f32 = 0.0001;

#[derive(Clone, Debug)]
pub struct PlanarSurface {
    pub reference: Option<GeneratedFaceRef>,
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
                    && s.normal.dot(origin - s.origin).abs() < PLANAR_SURFACE_DISTANCE_TOLERANCE
            }) {
                surface.triangles.push(triangle.vertices);
            } else {
                surfaces.push(PlanarSurface {
                    reference: None,
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
                        generated_face_ref: surface.reference.clone(),
                        normal: surface.normal,
                        centroid: surface.origin,
                    })
                })
            })
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
    }

    /// All surface boundaries, including opening loops, without triangulation
    /// diagonals. Inference caches this bounded set for one hovered authored body.
    pub(crate) fn inference_boundary_segments(&self, limit: usize) -> Option<Vec<(Vec3, Vec3)>> {
        use std::collections::BTreeMap;
        if self
            .surfaces
            .iter()
            .map(|s| s.triangles.len())
            .sum::<usize>()
            > limit.saturating_mul(4)
        {
            return None;
        }
        let key = |point: Vec3| {
            point
                .to_array()
                .map(|v| (v as f64 * 100_000.).round() as i64)
        };
        let mut boundaries = BTreeMap::new();
        for surface in &self.surfaces {
            let mut edges = BTreeMap::new();
            for triangle in &surface.triangles {
                for i in 0..3 {
                    let (a, b) = (triangle[i], triangle[(i + 1) % 3]);
                    if !a.is_finite() || !b.is_finite() {
                        return None;
                    }
                    let (ka, kb) = (key(a), key(b));
                    if ka == kb {
                        continue;
                    }
                    let edge_key = if ka < kb { (ka, kb) } else { (kb, ka) };
                    let entry = edges.entry(edge_key).or_insert((a, b, 0usize));
                    entry.2 += 1;
                }
            }
            for (key, (a, b, count)) in edges {
                if count == 1 {
                    boundaries.entry(key).or_insert((a, b));
                }
            }
            if boundaries.len() > limit {
                return None;
            }
        }
        Some(boundaries.into_values().collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inference_boundaries_keep_holes_and_omit_coplanar_diagonals() {
        let mut triangles = Vec::new();
        let outer = [
            Vec3::new(-2., 0., -2.),
            Vec3::new(2., 0., -2.),
            Vec3::new(2., 0., 2.),
            Vec3::new(-2., 0., 2.),
        ];
        let inner = outer.map(|p| p * 0.5);
        for i in 0..4 {
            let j = (i + 1) % 4;
            triangles.push(CsgTriangle::new(outer[i], outer[j], inner[j]));
            triangles.push(CsgTriangle::new(outer[i], inner[j], inner[i]));
        }
        let body = PlanarBody::from_triangles(triangles);
        let edges = body.inference_boundary_segments(32).unwrap();
        assert_eq!(edges.len(), 8);
        for (a, b) in edges {
            assert!((a.x - b.x).abs() < 1e-6 || (a.z - b.z).abs() < 1e-6);
        }
        assert!(body.inference_boundary_segments(4).is_none());
    }
}
