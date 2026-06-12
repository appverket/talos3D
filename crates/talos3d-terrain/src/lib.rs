pub mod commands;
pub mod components;
pub mod conforming;
pub mod cut_fill;
pub mod fairing;
pub mod generation;
pub mod heightfield;
pub mod planting;
pub mod reconstruction;
pub mod review;
pub mod snapshots;
pub mod terrain_provider;
pub mod tools;
pub mod visualization;

use bevy::prelude::*;
use talos3d_core::plugins::agent_skills::{
    AgentSkill, AgentSkillAppExt, AgentSkillId, AgentSkillTrustLevel,
};
use talos3d_capability_api::{
    capabilities::{
        CapabilityDescriptor, CapabilityDistribution, CapabilityRegistryAppExt, RequireWorkbench,
        TerrainProviderRegistryAppExt, WorkbenchDescriptor,
    },
    modeling::ModelingWorkbench,
};

use crate::{
    commands::TerrainCommandPlugin,
    conforming::{ConformingSolidFactory, ConformingSolidPlugin},
    generation::TerrainGenerationPlugin,
    planting::PlantingPlugin,
    review::TerrainReviewPlugin,
    snapshots::{ElevationCurveFactory, TerrainSurfaceFactory},
    terrain_provider::TerrainProviderImpl,
    tools::TerrainToolPlugin,
};

/// Discoverable how-to (ADR-059, PP-PLANT-E) routing agents to the real,
/// executable terrain-conforming-foundation tools. Registered as an agent skill
/// rather than a curated recipe/assembly-pattern because a conforming solid is a
/// single terrain-derived solid, not a layered material assembly or a
/// (currently no-op) schematic recipe — so this is the honest, non-bluffing
/// discovery surface per ADR-042.
fn terrain_conforming_foundation_skill() -> AgentSkill {
    AgentSkill {
        id: AgentSkillId("terrain.conforming_foundation".to_string()),
        title: "Terrain-conforming (hugging) foundation".to_string(),
        summary: "Place a foundation whose flat top sits at minimum clearance over grade and whose \
                  underside hugs the terrain; optionally plant an existing building onto it."
            .to_string(),
        task_tags: vec![
            "foundation".to_string(),
            "terrain".to_string(),
            "hugging foundation".to_string(),
            "plant building".to_string(),
            "site".to_string(),
        ],
        referenced_tool_ids: vec![
            "create_entity".to_string(),
            "terrain.plant_on_surface".to_string(),
            "terrain.unplant_on_surface".to_string(),
            "set_property".to_string(),
        ],
        next_skill_ids: vec![],
        body_markdown: "A *conforming solid* hugs a terrain surface: flat horizontal top at \
            `Y_top = max(grade under footprint) + min_thickness`, underside `= max(grade, Y_top - \
            max_depth)` (benches flat past `max_depth`, default 3 m), vertical walls.\n\n\
            **Create one directly:** `create_entity {\"type\":\"conforming_solid\", \
            \"surface_id\":<terrain surface id>, \"position\":[x,z], \"half_extents\":[hx,hz], \
            \"min_thickness\":0.5, \"max_depth\":3.0}`. Move/re-conform it with \
            `set_property {\"property_name\":\"position\",\"value\":[x,0,z]}` — Y re-derives.\n\n\
            **Plant an existing building (reversible):** `terrain.plant_on_surface \
            {\"target_id\":<object>, \"surface_id\":<id>, \"min_thickness\":0.5, \
            \"hide_element_id\":<old foundation, optional>}` creates the hugging foundation under \
            the object, seats the object on its top, and hides the old foundation. \
            `terrain.unplant_on_surface {\"target_id\":<object>}` reverses it.\n\n\
            The geometry is grounded in the real terrain surface (not a stand-in)."
            .to_string(),
        trust_level: AgentSkillTrustLevel::Shipped,
        source_path: None,
    }
}

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
            .register_authored_entity_factory(ConformingSolidFactory)
            .register_agent_skill(terrain_conforming_foundation_skill())
            .register_terrain_provider(TerrainProviderImpl)
            .add_plugins(TerrainCommandPlugin)
            .add_plugins(ConformingSolidPlugin)
            .add_plugins(PlantingPlugin)
            .add_plugins(TerrainGenerationPlugin)
            .add_plugins(TerrainReviewPlugin)
            .add_plugins(TerrainToolPlugin);
    }
}
