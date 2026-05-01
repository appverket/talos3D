use bevy::prelude::*;
use serde_json::Value;
use talos3d_capability_api::{
    commands::{
        activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        CommandResult, IconRegistry,
    },
    document_properties::DocumentProperties,
    icons,
    prelude::{
        AssemblyTypeDescriptor, CapabilityDescriptor, CapabilityDistribution,
        CapabilityRegistryAppExt, DefaultsContributor, DefaultsRegistryAppExt,
        RelationTypeDescriptor, WorkbenchDescriptor,
    },
    toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
    tools::ActiveTool,
};
use talos3d_core::capability_registry::TerrainProviderRegistry;

use crate::{
    components::{BuildingPad, BuildingPadExcavation},
    create_commands::ArchitecturalCreateCommandPlugin,
    hosted_layout::evaluate_hosted_layouts_and_placements,
    mesh_generation, rules,
    snapshots::{BuildingPadFactory, OpeningFactory, WallFactory},
    tools,
};

pub struct ArchitecturalPlugin;

struct ArchitecturalDefaultsContributor;

impl DefaultsContributor for ArchitecturalDefaultsContributor {
    fn contribute_defaults(&self, props: &mut DocumentProperties) {
        props
            .domain_defaults
            .entry("architectural".to_string())
            .or_insert_with(|| {
                serde_json::json!({
                    "wall_height": 3.0,
                    "wall_thickness": 0.2,
                    "opening_width": 1.2,
                    "opening_height": 1.5,
                    "sill_height": 0.9
                })
            });
    }
}

impl Plugin for ArchitecturalPlugin {
    fn build(&self, app: &mut App) {
        app.register_workbench(
            WorkbenchDescriptor::new("architectural", "Architectural")
                .with_description("Architectural design with walls, openings, and BIM")
                .with_capabilities(["modeling", "architectural"]),
        )
        .register_capability(
            CapabilityDescriptor::new("architectural", "Architectural")
                .with_description("Walls, openings, and BIM metadata")
                .with_dependencies(["modeling"])
                .with_distribution(CapabilityDistribution::ReferenceExtension),
        )
        // Assembly types
        .register_assembly_type(AssemblyTypeDescriptor {
            assembly_type: "house".into(),
            label: "House".into(),
            description: "A complete residential building".into(),
            expected_member_types: vec!["wall".into(), "opening".into(), "storey".into()],
            expected_member_roles: vec![
                "exterior_wall".into(),
                "partition".into(),
                "roof_element".into(),
                "storey".into(),
            ],
            expected_relation_types: vec!["hosted_on".into(), "bounds".into()],
            parameter_schema: serde_json::json!({
                "properties": {
                    "num_floors": {"type": "integer"},
                    "roof_type": {"type": "string"}
                }
            }),
        })
        .register_assembly_type(AssemblyTypeDescriptor {
            assembly_type: "storey".into(),
            label: "Storey".into(),
            description: "One floor level of a building".into(),
            expected_member_types: vec!["wall".into(), "room".into()],
            expected_member_roles: vec!["wall".into(), "space".into()],
            expected_relation_types: vec!["bounds".into()],
            parameter_schema: serde_json::json!({
                "properties": {
                    "level": {"type": "number"}
                }
            }),
        })
        .register_assembly_type(AssemblyTypeDescriptor {
            assembly_type: "room".into(),
            label: "Room".into(),
            description: "An enclosed space bounded by walls".into(),
            expected_member_types: vec!["wall".into()],
            expected_member_roles: vec!["boundary".into()],
            expected_relation_types: vec!["adjacent_to".into()],
            parameter_schema: serde_json::json!({
                "properties": {
                    "function": {"type": "string"},
                    "target_area": {"type": "number"}
                }
            }),
        })
        .register_assembly_type(AssemblyTypeDescriptor {
            assembly_type: "roof_system".into(),
            label: "Roof System".into(),
            description: "Roof structure and covering".into(),
            expected_member_types: vec![],
            expected_member_roles: vec!["structure".into(), "covering".into()],
            expected_relation_types: vec!["supports".into()],
            parameter_schema: serde_json::json!({
                "properties": {
                    "pitch": {"type": "number"},
                    "style": {"type": "string"}
                }
            }),
        })
        // Relation types
        .register_relation_type(RelationTypeDescriptor {
            relation_type: "hosted_on".into(),
            label: "Hosted On".into(),
            description: "Element is hosted on a parent surface (e.g. opening on wall)".into(),
            valid_source_types: vec!["opening".into(), "occurrence".into()],
            valid_target_types: vec!["wall".into()],
            parameter_schema: serde_json::json!({
                "properties": {
                    "position_along_wall": {"type": "number", "minimum": 0, "maximum": 1},
                    "opening_element_id": {"type": "integer"},
                    "placement_anchor": {"type": "string"}
                }
            }),
            participates_in_dependency_graph: true,
            external_classification: None,
            host_contract_kind: None,
        })
        .register_relation_type(RelationTypeDescriptor {
            relation_type: "layout_on_host".into(),
            label: "Layout On Host".into(),
            description: "Collection-level hosted layout that distributes hosted_on relations along a wall or wall segment.".into(),
            valid_source_types: vec!["opening".into(), "occurrence".into(), "semantic_assembly".into()],
            valid_target_types: vec!["wall".into()],
            parameter_schema: serde_json::json!({
                "properties": {
                    "members": {"type": "array"},
                    "mode": {"type": "string", "enum": ["fixed_start_total_width", "equal_spacing"]},
                    "anchor": {"type": "string", "enum": ["left_edge", "center"]},
                    "start_offset_m": {"type": "number"},
                    "total_width_m": {"type": "number"},
                    "member_width_m": {"type": "number"},
                    "sill_height_m": {"type": "number"},
                    "member_height_m": {"type": "number"}
                },
                "required": ["members", "start_offset_m", "total_width_m"]
            }),
            participates_in_dependency_graph: true,
            external_classification: None,
            host_contract_kind: None,
        })
        .register_relation_type(RelationTypeDescriptor {
            relation_type: "bounds".into(),
            label: "Bounds".into(),
            description: "Element spatially bounds another (e.g. wall bounds room)".into(),
            valid_source_types: vec!["wall".into()],
            valid_target_types: vec!["room".into()],
            parameter_schema: serde_json::json!({}),
            participates_in_dependency_graph: false,
            external_classification: None,
            host_contract_kind: None,
        })
        .register_relation_type(RelationTypeDescriptor {
            relation_type: "adjacent_to".into(),
            label: "Adjacent To".into(),
            description: "Two spaces share a boundary".into(),
            valid_source_types: vec!["room".into()],
            valid_target_types: vec!["room".into()],
            parameter_schema: serde_json::json!({}),
            participates_in_dependency_graph: false,
            external_classification: None,
            host_contract_kind: None,
        })
        .register_relation_type(RelationTypeDescriptor {
            relation_type: "supports".into(),
            label: "Supports".into(),
            description: "Structural support relationship".into(),
            valid_source_types: vec!["wall".into()],
            valid_target_types: vec!["storey".into(), "roof_system".into()],
            parameter_schema: serde_json::json!({}),
            participates_in_dependency_graph: false,
            external_classification: None,
            host_contract_kind: None,
        })
        // PP71–PP74 element classes, recipe families, and architectural
        // validators now live in the `talos3d-architecture-core` crate
        // (ADR-037). Applications wanting the semantic substrate register
        // `ArchitectureCorePlugin` alongside `ArchitecturalPlugin`.
        .register_defaults_contributor(ArchitecturalDefaultsContributor)
        .register_command(
            CommandDescriptor {
                id: "architectural.create_wall".to_string(),
                label: "Create Wall".to_string(),
                description: "Activate wall placement".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({"type":"object"})),
                default_shortcut: Some("W".to_string()),
                icon: Some("icon.architectural.wall".to_string()),
                hint: Some("Click two points to place a wall".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: Some("PlaceWall".to_string()),
                capability_id: Some("architectural".to_string()),
            },
            execute_create_wall,
        )
        .register_command(
            CommandDescriptor {
                id: "architectural.place_opening".to_string(),
                label: "Place Opening".to_string(),
                description: "Activate opening placement".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({"type":"object"})),
                default_shortcut: Some("O".to_string()),
                icon: Some("icon.architectural.opening".to_string()),
                hint: Some("Hover a wall and click to place an opening".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: Some("PlaceOpening".to_string()),
                capability_id: Some("architectural".to_string()),
            },
            execute_place_opening,
        )
        .register_command(
            CommandDescriptor {
                id: "architectural.create_building_pad".to_string(),
                label: "Create Building Pad".to_string(),
                description: "Activate building pad placement".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({"type":"object"})),
                default_shortcut: None,
                icon: Some("icon.architectural.building_pad".to_string()),
                hint: Some("Click pad boundary vertices, then press Enter to close".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: Some("PlaceBuildingPad".to_string()),
                capability_id: Some("architectural".to_string()),
            },
            execute_create_building_pad,
        )
        .register_toolbar(ToolbarDescriptor {
            id: "architectural".to_string(),
            label: "Architectural".to_string(),
            default_dock: ToolbarDock::Left,
            default_visible: true,
            sections: vec![ToolbarSection {
                label: "Walls & Openings".to_string(),
                command_ids: vec![
                    "architectural.create_wall".to_string(),
                    "architectural.place_opening".to_string(),
                    "architectural.create_building_pad".to_string(),
                ],
            }],
        })
        .register_authored_entity_factory(BuildingPadFactory)
        .register_authored_entity_factory(WallFactory)
        .register_authored_entity_factory(OpeningFactory)
        .add_systems(Startup, setup_architectural_icons)
        .add_plugins(ArchitecturalCreateCommandPlugin)
        .add_plugins(mesh_generation::ArchitecturalMeshPlugin)
        .add_plugins(rules::ArchitecturalRulesPlugin)
        .add_plugins(tools::ArchitecturalToolPlugin)
        .add_systems(
            Update,
            (
                evaluate_hosted_layouts_and_placements
                    .before(talos3d_core::plugins::modeling::occurrence::evaluate_occurrences)
                    .in_set(
                        talos3d_core::plugins::modeling::mesh_generation::EvaluationSet::Evaluate,
                    ),
                update_building_pad_excavations,
            ),
        );
    }
}

fn update_building_pad_excavations(
    mut commands: Commands,
    terrain_registry: Option<Res<TerrainProviderRegistry>>,
    pads: Query<(Entity, &BuildingPad)>,
    world: &World,
) {
    let provider = terrain_registry.and_then(|registry| registry.provider());
    for (entity, pad) in &pads {
        let volume = provider.as_ref().and_then(|provider| {
            provider.volume_above_datum(world, &pad.boundary, pad.pad_elevation)
        });
        commands
            .entity(entity)
            .insert(BuildingPadExcavation { volume });
    }
}

fn execute_create_wall(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceWall)
}

fn execute_place_opening(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceOpening)
}

fn execute_create_building_pad(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::PlaceBuildingPad)
}

fn setup_architectural_icons(
    mut images: ResMut<Assets<Image>>,
    mut icon_registry: ResMut<IconRegistry>,
) {
    let size = bevy::render::render_resource::Extent3d {
        width: icons::ICON_SIZE,
        height: icons::ICON_SIZE,
        depth_or_array_layers: 1,
    };
    for (id, icon_name) in [
        ("icon.architectural.wall", "wall"),
        ("icon.architectural.opening", "opening"),
    ] {
        let rgba = icons::render_icon(icon_name);
        let image = Image::new(
            size,
            bevy::render::render_resource::TextureDimension::D2,
            rgba,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::default(),
        );
        icon_registry.register(id, images.add(image));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use talos3d_core::{
        capability_registry::{TerrainProvider, TerrainProviderRegistry},
        plugins::modeling::primitives::TriangleMesh,
    };

    struct FixedVolumeTerrainProvider;

    impl TerrainProvider for FixedVolumeTerrainProvider {
        fn elevation_at(&self, _world: &World, _x: f32, _z: f32) -> Option<f32> {
            Some(0.0)
        }

        fn surface_within_boundary(
            &self,
            _world: &World,
            _boundary: &[Vec2],
        ) -> Option<TriangleMesh> {
            None
        }

        fn volume_above_datum(
            &self,
            _world: &World,
            boundary: &[Vec2],
            datum_y: f32,
        ) -> Option<f64> {
            Some(boundary.len() as f64 + f64::from(datum_y))
        }
    }

    #[test]
    fn building_pad_excavation_updates_from_terrain_provider() {
        let mut app = App::new();
        app.init_resource::<TerrainProviderRegistry>();
        app.world_mut()
            .resource_mut::<TerrainProviderRegistry>()
            .register(FixedVolumeTerrainProvider);
        app.add_systems(Update, update_building_pad_excavations);
        let entity = app
            .world_mut()
            .spawn(BuildingPad {
                boundary: vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(2.0, 0.0),
                    Vec2::new(2.0, 2.0),
                    Vec2::new(0.0, 2.0),
                ],
                pad_elevation: 1.5,
            })
            .id();

        app.update();

        assert_eq!(
            app.world()
                .entity(entity)
                .get::<BuildingPadExcavation>()
                .map(|excavation| excavation.volume),
            Some(Some(5.5))
        );
    }

    #[test]
    fn building_pad_excavation_is_na_without_terrain_provider() {
        let mut app = App::new();
        app.add_systems(Update, update_building_pad_excavations);
        let entity = app
            .world_mut()
            .spawn(BuildingPad {
                boundary: vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(1.0, 0.0),
                    Vec2::new(1.0, 1.0),
                ],
                pad_elevation: 0.0,
            })
            .id();

        app.update();

        assert_eq!(
            app.world()
                .entity(entity)
                .get::<BuildingPadExcavation>()
                .map(|excavation| excavation.volume),
            Some(None)
        );
    }
}
