//! Capability-owned MCP discovery cards.
use bevy::prelude::*;
use talos3d_core::plugins::authoring_guidance::{GuidanceCardInfo, GuidanceCardRegistry};

pub(crate) fn register_guidance(app: &mut App) {
    app.init_resource::<GuidanceCardRegistry>();
    for card in cards() {
        app.world_mut()
            .resource_mut::<GuidanceCardRegistry>()
            .insert_if_absent(card);
    }
}

fn cards() -> Vec<GuidanceCardInfo> {
    vec![
        GuidanceCardInfo {
            id: "dkg.site_from_survey".into(), title: "Derive Terrain From Survey".into(),
            task_tags: vec!["terrain".into(), "survey".into()],
            summary: "Inspect drawing units and coordinate frames before reconstructing a terrain surface from survey evidence.".into(),
            referenced_tool_ids: vec!["import_file".into(), "invoke_command".into(), "elevation_at".into(), "take_screenshot".into()],
            body_markdown: Some("Retain source identity, units, elevation datum and uncertainty. Review the reconstructed surface against the source before using it for placement; bounds are not sampled grade.".into()),
            ..GuidanceCardInfo::default()
        },
        GuidanceCardInfo {
            id: "dkg.terrain_foundation".into(), title: "Conform Supports To Terrain".into(),
            task_tags: vec!["terrain".into(), "support".into(), "placement".into()],
            summary: "Use sampled elevation and the authored support footprint to place a whole structure on a terrain-conforming support; preserve its local frame.".into(),
            referenced_tool_ids: vec!["invoke_command".into(), "elevation_at".into(), "get_world_aabb".into(), "take_screenshot".into()],
            body_markdown: Some("Use terrain.plant_structure or terrain.plant_on_surface through invoke_command. Capability vocabulary determines structure and support assembly types. Move the aggregate through the shared placement plan. Validate contact with sampled elevation and inspect the rendered result. Domain packs supply engineering or construction obligations; the generic surface operation does not certify them.".into()),
            ..GuidanceCardInfo::default()
        },
    ]
}

#[cfg(all(test, feature = "model-api"))]
mod tests {
    use super::*;
    use talos3d_core::plugins::model_api::handle_get_guidance_card;
    #[test]
    fn terrain_guidance_has_no_architecture_vocabulary() {
        let mut app = App::new();
        register_guidance(&mut app);
        let card = handle_get_guidance_card(app.world(), "dkg.terrain_foundation".into()).unwrap();
        assert!(card.summary.contains("support footprint"));
        assert!(!card.summary.contains("foundation_system"));
        assert!(!card.body_markdown.unwrap().contains("house"));
    }
}
