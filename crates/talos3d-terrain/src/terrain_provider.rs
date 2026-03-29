use std::cmp::Ordering;

use bevy::{ecs::world::EntityRef, prelude::*};
use talos3d_core::{
    capability_registry::TerrainProvider,
    plugins::{modeling::primitives::TriangleMesh, tools::Preview},
};

use crate::{
    components::TerrainMeshCache,
    generation::{clip_mesh_to_boundary, sample_surface_elevation, volume_above_datum},
};

#[derive(Default)]
pub struct TerrainProviderImpl;

impl TerrainProvider for TerrainProviderImpl {
    fn elevation_at(&self, world: &World, x: f32, z: f32) -> Option<f32> {
        iter_visible_surface_meshes(world)
            .into_iter()
            .filter_map(|mesh| sample_surface_elevation(mesh, x, z))
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
            if entity_ref.contains::<Preview>() {
                return None;
            }
            if entity_ref
                .get::<Visibility>()
                .is_some_and(|visibility| *visibility == Visibility::Hidden)
            {
                return None;
            }
            entity_ref
                .get::<TerrainMeshCache>()
                .map(|cache| &cache.mesh)
        })
        .collect()
}
