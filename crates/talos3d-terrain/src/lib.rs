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
        id: AgentSkillId("terrain.conforming_foundation".to_string()),
        title: "Terrain-conforming (hugging) foundation".to_string(),
        summary:
            "Place a foundation whose flat top sits at minimum clearance over grade and whose \
                  underside hugs the terrain; optionally plant an existing building onto it."
                .to_string(),
        task_tags: vec![
            "foundation".to_string(),
            "terrain".to_string(),
            "hugging foundation".to_string(),
            "plant building".to_string(),
            "site".to_string(),
        ],
        must_read_at_bootstrap: true,
        referenced_tool_ids: vec![
            "create_entity".to_string(),
            "invoke_command".to_string(),
            "set_property".to_string(),
        ],
        required_tool_ids: vec![
            "get_capability_snapshot".to_string(),
            "get_agent_skill".to_string(),
            "elevation_at".to_string(),
            "take_screenshot".to_string(),
        ],
        forbidden_tool_ids: vec![
            "create_box for a terrain-conforming foundation".to_string(),
            "get_world_aabb as terrain grade source".to_string(),
        ],
        validation_tool_ids: vec![
            "elevation_at".to_string(),
            "get_world_aabb".to_string(),
            "take_screenshot".to_string(),
        ],
        success_criteria: vec![
            "Foundation top is derived from sampled terrain grade, not terrain AABB height."
                .to_string(),
            "Superstructure and conforming foundation move as one planted assembly.".to_string(),
            "When hide_element_id is supplied, the hidden base footprint drives foundation plan size."
                .to_string(),
        ],
        stop_conditions: vec![
            "No terrain_surface can be identified.".to_string(),
            "The building assembly cannot be resolved as one movable group or semantic structure."
                .to_string(),
        ],
        screenshot_requirements: vec![
            "Low side view showing the superstructure seated on the conforming foundation."
                .to_string(),
            "Plan/quarter view confirming roof eaves do not inflate the foundation footprint."
                .to_string(),
        ],
        common_failure_modes: vec![
            "Foundation is placed at the terrain datum while the real grade is higher or lower."
                .to_string(),
            "Walls or roof remain at world Y=0 after the foundation is planted on terrain."
                .to_string(),
            "Foundation footprint is sized from roof/eave/group bounds instead of the base footprint."
                .to_string(),
        ],
        regression_prompt_ids: vec![
            "terrain-plant-structure-hugging-foundation".to_string(),
            "terrain-plant-hidden-base-footprint".to_string(),
        ],
        next_skill_ids: vec![],
        body_markdown: "A *conforming solid* hugs a terrain surface: flat horizontal top at \
            `Y_top = max(grade under footprint) + min_thickness`, underside `= max(grade, Y_top - \
            max_depth)` (benches flat past `max_depth`, default 3 m), vertical walls.\n\n\
            **Create one directly:** `create_entity {\"type\":\"conforming_solid\", \
            \"surface_id\":<terrain surface id>, \"position\":[x,z], \"half_extents\":[hx,hz], \
            \"min_thickness\":0.5, \"max_depth\":3.0}`. Move/re-conform it with \
            `set_property {\"property_name\":\"position\",\"value\":[x,0,z]}` — Y re-derives.\n\n\
            **Plant a semantic structure:** identify the semantic `structure`, `house`, or `building` assembly, its *complete* \
            physical building group, and one terrain surface, then run Plant Structure; over MCP call \
            `invoke_command {\"command_id\":\"terrain.plant_structure\", \"parameters\": \
            {\"structure_id\":<structure>, \"group_id\":<complete physical group>, \
            \"foundation_id\":<foundation body>, \"surface_id\":<terrain>}}`. When the structure was \
            created by `create_assembly`, pass its returned `group_element_id` as `group_id`; the \
            semantic assembly's first group-valued member may be only a slab/foundation branch and \
            must never be inferred as the whole building. The group must cover every physical member \
            of the structure or the command stops before planting. \
            The command establishes the planting contract, converts the structure's bottom \
            foundation body into a conforming foundation, keeps that foundation inside the \
            movable group, and records `structure_id`, `foundation_structure_id`, and \
            `planted_group_id` for later prompt/refinement/move targeting.\n\n\
            **Plant an existing building (reversible):** call \
            `invoke_command {\"command_id\":\"terrain.plant_on_surface\", \"parameters\": \
            {\"target_id\":<object>, \"surface_id\":<id>, \"min_thickness\":0.5, \
            \"hide_element_id\":<old foundation/base, optional>}}`. It creates the hugging foundation \
            under the object, seats the object on its top, creates a semantic `structure` \
            assembly plus a nested semantic `foundation_system` assembly, and marks the group as \
            a planted structure so later horizontal moves re-seat the superstructure to the \
            foundation's newly sampled terrain top. When `hide_element_id` is supplied, that \
            foundation/base footprint drives the new conforming foundation in plan; roof eaves, \
            gable closure panels, trim, and other visual overhangs must not inflate the \
            foundation footprint. \
            `invoke_command {\"command_id\":\"terrain.release_planted_structure\", \"parameters\": \
            {\"target_id\":<object>}}` keeps the current \
            geometry but removes that terrain-following behavior. \
            `invoke_command {\"command_id\":\"terrain.demote_conforming_foundation\", \
            \"parameters\":{\"target_id\":<foundation_system_id>, \"mode\":\"snapshot\"}}` \
            freezes the adaptive foundation body as a static mesh; \
            `\"mode\":\"max_height_box\"` replaces it with a rectangular box whose height equals \
            the current conforming foundation's maximum thickness. Either demotion preserves the \
            semantic foundation_system and releases terrain-following behavior that depended on \
            the adaptive body. \
            `invoke_command {\"command_id\":\"terrain.unplant_on_surface\", \"parameters\": \
            {\"target_id\":<object>}}` reverses it.\n\n\
            **Mandatory postcondition:** record representative pre-plant AABBs for foundation, \
            walls, roof, openings, and trim. The plant response returns \
            `conforming_foundation_body_id`: the original rigid body is replaced in place under \
            that stable element id. On the next MCP request, inspect its rendered AABB; its plan \
            footprint must be preserved and its top must equal `y_top`, but its terrain-hugging \
            underside/min-Y is intentionally not a rigid `raised_by` translation. Every non-foundation \
            representative must have moved vertically by `raised_by` (within tolerance) and its \
            bottom must meet the returned `y_top`; the foundation stays at `y_top`. If only a \
            foundation branch moved, stop and report a planting-contract defect — never repair \
            the model by translating parts manually. The geometry is grounded in the real terrain \
            surface (not a stand-in)."
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_conforming_foundation_skill_uses_live_mcp_command_surface() {
        let skill = terrain_conforming_foundation_skill();
        assert!(
            skill.must_read_at_bootstrap,
            "the architecture guidance names this safety-critical workflow as a mandatory bootstrap skill"
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
