pub mod commands;
pub mod components;
pub mod conforming;
pub mod cut_fill;
mod discovery_guidance;
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
use talos3d_capability_api::{
    capabilities::{
        CapabilityDescriptor, CapabilityDistribution, CapabilityRegistryAppExt, RequireWorkbench,
        TerrainProviderRegistryAppExt, WorkbenchDescriptor,
    },
    modeling::ModelingWorkbench,
};
use talos3d_core::plugins::agent_skills::{
    AgentSkill, AgentSkillAppExt, AgentSkillId, AgentSkillTrustLevel,
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
        id: AgentSkillId("terrain.conforming_foundation".into()),
        title: "Terrain-conforming support".into(),
        summary: "Create a support solid from sampled terrain grade and place a complete structure on it through the shared placement plan.".into(),
        task_tags: vec!["terrain".into(), "support".into(), "surface placement".into()],
        must_read_at_bootstrap: true,
        referenced_tool_ids: vec!["create_entity".into(), "invoke_command".into(), "set_property".into()],
        required_tool_ids: vec!["get_capability_snapshot".into(), "get_agent_skill".into(), "elevation_at".into(), "take_screenshot".into()],
        forbidden_tool_ids: vec!["get_world_aabb as terrain grade source".into()],
        validation_tool_ids: vec!["elevation_at".into(), "get_world_aabb".into(), "take_screenshot".into()],
        success_criteria: vec!["Support top derives from sampled terrain grade.".into(), "The complete aggregate moves through one shared plan.".into(), "An authored base footprint determines support plan size.".into()],
        stop_conditions: vec!["No terrain surface or coherent placement aggregate can be identified.".into()],
        screenshot_requirements: vec!["Inspect contact and the support footprint in target-framed views.".into()],
        common_failure_modes: vec!["Using terrain bounds as grade.".into(), "Using visual overhangs instead of the authored base footprint.".into()],
        regression_prompt_ids: vec!["terrain-plant-structure-hugging-foundation".into(), "terrain-plant-hidden-base-footprint".into()],
        next_skill_ids: Vec::new(),
        body_markdown: "A conforming_solid has a flat top at max(sampled grade under the footprint) + min_thickness. Its underside follows grade within max_depth. Use create_entity with type conforming_solid and an explicit surface_id, position, half_extents, min_thickness and max_depth.

Plant the complete semantic structure with terrain.plant_structure through invoke_command; provide structure_id, surface_id and the complete group_id. The legacy foundation_id parameter identifies the support body. The active edition supplies its accepted structure and support assembly types; the generic defaults are structure and support_system.

terrain.plant_on_surface accepts target_id and surface_id for reversible placement. When hide_element_id identifies the previous base, its footprint determines support plan size. Inspect all aggregate members and sampled contact after placement. A changed support AABB is expected; unrelated geometry must be preserved.

terrain.release_planted_structure removes adaptive placement while preserving current geometry. terrain.demote_conforming_foundation freezes the adaptive support as a snapshot or max_height_box; these legacy command ids describe the same generic support operation. terrain.unplant_on_surface reverses placement.

When create_assembly returns group_element_id, use that complete aggregate as group_id. Example: invoke_command {\"command_id\":\"terrain.plant_structure\", \"parameters\":{\"structure_id\":<structure>,\"group_id\":<complete physical group>,\"foundation_id\":<support>,\"surface_id\":<surface>}}. For object placement use invoke_command {\"command_id\":\"terrain.plant_on_surface\", \"parameters\":{\"target_id\":<object>,\"surface_id\":<surface>}}. After a partial or unexpected result, never repair the model by translating parts manually; inspect the shared contract and undo or rerun the aggregate command.

Domain packs supply architectural or engineering obligations and construction guidance. A terrain-derived support alone does not resolve those claims.".into(),
        trust_level: AgentSkillTrustLevel::Shipped,
        source_path: None,
    }
}

pub struct TerrainPlugin;

#[derive(Resource, Default)]
pub struct TerrainWorkbench;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        discovery_guidance::register_guidance(app);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_conforming_foundation_skill_uses_live_mcp_command_surface() {
        let skill = terrain_conforming_foundation_skill();
        assert!(
            skill.must_read_at_bootstrap,
            "surface placement requires this shared workflow as a mandatory bootstrap skill"
        );

        assert!(skill
            .referenced_tool_ids
            .contains(&"invoke_command".to_string()));
        assert!(!skill
            .referenced_tool_ids
            .contains(&"terrain.plant_structure".to_string()));
        assert!(skill.body_markdown.contains("invoke_command"));
        assert!(skill
            .body_markdown
            .contains("\"command_id\":\"terrain.plant_structure\""));
        assert!(skill
            .body_markdown
            .contains("\"command_id\":\"terrain.plant_on_surface\""));
        assert!(skill.body_markdown.contains("group_element_id"));
        assert!(skill
            .body_markdown
            .contains("\"group_id\":<complete physical group>"));
        assert!(skill
            .body_markdown
            .contains("never repair the model by translating parts manually"));
    }
}
