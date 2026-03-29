pub mod commands;
pub mod components;
pub mod generation;
pub mod reconstruction;
pub mod review;
pub mod snapshots;
pub mod terrain_provider;

use bevy::prelude::*;
use talos3d_capability_api::{
    capabilities::{
        CapabilityDescriptor, CapabilityDistribution, CapabilityRegistryAppExt, RequireSetup,
        SetupDescriptor, TerrainProviderRegistryAppExt,
    },
    modeling::ModelingSetup,
};

use crate::{
    commands::TerrainCommandPlugin,
    generation::TerrainGenerationPlugin,
    review::TerrainReviewPlugin,
    snapshots::{ElevationCurveFactory, TerrainSurfaceFactory},
    terrain_provider::TerrainProviderImpl,
};

pub struct TerrainPlugin;

#[derive(Resource, Default)]
pub struct TerrainSetup;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainSetup>()
            .register_setup(
                SetupDescriptor::new("terrain", "Terrain")
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
            .add_plugins(RequireSetup::<ModelingSetup>::default())
            .register_authored_entity_factory(ElevationCurveFactory)
            .register_authored_entity_factory(TerrainSurfaceFactory)
            .register_terrain_provider(TerrainProviderImpl)
            .add_plugins(TerrainCommandPlugin)
            .add_plugins(TerrainGenerationPlugin)
            .add_plugins(TerrainReviewPlugin);
    }
}
