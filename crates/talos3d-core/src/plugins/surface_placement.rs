//! Domain-supplied vocabulary for reusable surface/support placement.
use bevy::prelude::*;

#[derive(Resource, Debug, Clone)]
pub struct SurfacePlacementVocabulary {
    pub support_assembly_type: String,
    pub support_member_role: String,
    pub structure_assembly_types: Vec<String>,
}

impl Default for SurfacePlacementVocabulary {
    fn default() -> Self {
        Self {
            support_assembly_type: "support_system".into(),
            support_member_role: "support".into(),
            structure_assembly_types: vec!["structure".into()],
        }
    }
}

pub fn placement_vocabulary(world: &World) -> SurfacePlacementVocabulary {
    world
        .get_resource::<SurfacePlacementVocabulary>()
        .cloned()
        .unwrap_or_default()
}
