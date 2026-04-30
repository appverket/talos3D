pub mod commands;
pub mod components;
pub mod cut_fill;
pub mod generation;
pub mod reconstruction;
pub mod review;
pub mod snapshots;
pub mod terrain_provider;
pub mod tools;
pub mod visualization;

use bevy::prelude::*;
use talos3d_capability_api::{
    capabilities::{
        CapabilityDescriptor, CapabilityDistribution, CapabilityRegistryAppExt, RequireWorkbench,
        TerrainProviderRegistryAppExt, WorkbenchDescriptor,
    },
    modeling::ModelingWorkbench,
};

use crate::{
    commands::TerrainCommandPlugin,
    generation::TerrainGenerationPlugin,
    review::TerrainReviewPlugin,
    snapshots::{ElevationCurveFactory, TerrainSurfaceFactory},
    terrain_provider::TerrainProviderImpl,
    tools::TerrainToolPlugin,
};

pub struct TerrainPlugin;

#[derive(Resource, Default)]
pub struct TerrainWorkbench;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainWorkbench>()
            .register_workbench(
                WorkbenchDescriptor::new("terrain", "Terrain")
                    .with_description(
                        "Terrain modeling with elevation curves and surface generation",
                    )
                    .with_capabilities(["modeling", "terrain"]),
            )
            .register_capability(
                CapabilityDescriptor::new("terrain", "Terrain")
                    .with_description("Elevation curves and terrain surface generation")
                    .with_dependencies(["modeling"])
                    .with_distribution(CapabilityDistribution::ReferenceExtension),
            )
            .add_plugins(RequireWorkbench::<ModelingWorkbench>::default())
            .register_authored_entity_factory(ElevationCurveFactory)
            .register_authored_entity_factory(TerrainSurfaceFactory)
            .register_terrain_provider(TerrainProviderImpl)
            .add_plugins(TerrainCommandPlugin)
            .add_plugins(TerrainGenerationPlugin)
            .add_plugins(TerrainReviewPlugin)
            .add_plugins(TerrainToolPlugin);
    }
}
