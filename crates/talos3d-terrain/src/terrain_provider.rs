use std::cmp::Ordering;

use bevy::{ecs::world::EntityRef, prelude::*};
use talos3d_core::{
    capability_registry::TerrainProvider,
    plugins::{modeling::primitives::TriangleMesh, tools::Preview},
};

use crate::{
    components::{TerrainMeshCache, TerrainSurface},
    generation::{clip_mesh_to_boundary, sample_surface_elevation, volume_above_datum},
};

#[derive(Default)]
pub struct TerrainProviderImpl;

impl TerrainProvider for TerrainProviderImpl {
    fn elevation_at(&self, world: &World, x: f32, z: f32) -> Option<f32> {
        iter_visible_surface_meshes(world)
            .into_iter()
            .filter_map(|mesh| sample_surface_elevation(mesh, x, z))
            .chain(flat_surface_elevations(world, x, z))
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
    }

    fn surface_within_boundary(&self, world: &World, boundary: &[Vec2]) -> Option<TriangleMesh> {
        iter_visible_surface_meshes(world)
            .into_iter()
            .filter_map(|mesh| clip_mesh_to_boundary(mesh, boundary))
            .max_by_key(|mesh| mesh.faces.len())
    }

    fn volume_above_datum(&self, world: &World, boundary: &[Vec2], datum_y: f32) -> Option<f64> {
        let mesh = self.surface_within_boundary(world, boundary)?;
        volume_above_datum(&mesh, datum_y)
    }
}

fn iter_visible_surface_meshes(world: &World) -> Vec<&TriangleMesh> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .filter_map(|entity_ref| {
            if is_hidden_or_preview(&entity_ref) {
                return None;
            }
            entity_ref
                .get::<TerrainMeshCache>()
                .map(|cache| &cache.mesh)
        })
        .collect()
}

fn flat_surface_elevations(world: &World, x: f32, z: f32) -> Vec<f32> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .filter_map(|entity_ref| {
            if is_hidden_or_preview(&entity_ref) {
                return None;
            }
            let surface = entity_ref.get::<TerrainSurface>()?;
            surface_contains_sample(surface, x, z)
                .then_some(surface.datum_elevation + surface.offset.y)
        })
        .collect()
}

fn is_hidden_or_preview(entity_ref: &EntityRef<'_>) -> bool {
    entity_ref.contains::<Preview>()
        || entity_ref
            .get::<Visibility>()
            .is_some_and(|visibility| *visibility == Visibility::Hidden)
}

fn surface_contains_sample(surface: &TerrainSurface, x: f32, z: f32) -> bool {
    if surface.boundary.is_empty() {
        return (x - surface.offset.x).abs() <= 5.0 && (z - surface.offset.z).abs() <= 5.0;
    }
    point_in_polygon(Vec2::new(x, z), &surface.boundary)
}

fn point_in_polygon(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        let crosses_z = (current.y > point.y) != (previous.y > point.y);
        if crosses_z {
            let x_intersection = (previous.x - current.x) * (point.y - current.y)
                / (previous.y - current.y)
                + current.x;
            if point.x < x_intersection {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use talos3d_core::plugins::identity::ElementId;

    #[test]
    fn elevation_at_uses_flat_surface_datum_when_mesh_is_absent() {
        let mut world = World::new();
        let mut surface = TerrainSurface::new("Flat site".into(), Vec::new());
        surface.datum_elevation = 1.25;
        surface.offset = Vec3::new(2.0, 0.5, 3.0);
        world.spawn((ElementId(1), surface));

        let provider = TerrainProviderImpl;

        assert_eq!(provider.elevation_at(&world, 2.0, 3.0), Some(1.75));
        assert_eq!(provider.elevation_at(&world, 100.0, 100.0), None);
    }
}
