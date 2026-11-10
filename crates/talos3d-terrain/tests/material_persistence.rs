//! Regression at the public capability/command/project boundary.
use bevy::prelude::*;
use talos3d_core::{
    capability_registry::CapabilityRegistry,
    plugins::{
        command_registry::{execute_command, CommandRegistry},
        document_properties::DocumentProperties,
        document_state::DocumentState,
        history::{History, HistoryPlugin, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        layers::LayerRegistry,
        materials::TextureRegistry,
        materials::{MaterialAssignment, MaterialDef, MaterialPlugin, MaterialRegistry},
        modeling::definition::{DefinitionLibraryRegistry, DefinitionRegistry},
        named_views::NamedViewRegistry,
        persistence::{capture_project_file_bytes, open_project_bytes, OpaquePersistedEntities},
        property_edit::PropertyEditState,
        selection::Selected,
        tools::ActiveTool,
        transform::TransformState,
    },
};
use talos3d_terrain::{
    components::TerrainSurface,
    snapshots::{TerrainSurfaceFactory, TerrainSurfaceSnapshot},
};

fn app() -> App {
    // Register the actual UI commands, without running GPU material systems.
    let mut registration = App::new();
    registration.add_plugins(MaterialPlugin);
    let commands = registration
        .world_mut()
        .remove_resource::<CommandRegistry>()
        .unwrap();
    let mut app = App::new();
    app.add_plugins(HistoryPlugin).insert_resource(commands);
    let w = app.world_mut();
    let mut factories = CapabilityRegistry::default();
    factories.register_factory(TerrainSurfaceFactory);
    w.insert_resource(factories);
    w.init_resource::<DocumentProperties>();
    w.init_resource::<LayerRegistry>();
    w.init_resource::<MaterialRegistry>();
    w.init_resource::<TextureRegistry>();
    w.init_resource::<DefinitionRegistry>();
    w.init_resource::<DefinitionLibraryRegistry>();
    w.init_resource::<NamedViewRegistry>();
    w.init_resource::<ElementIdAllocator>();
    w.init_resource::<OpaquePersistedEntities>();
    w.init_resource::<DocumentState>();
    w.init_resource::<PropertyEditState>();
    w.init_resource::<TransformState>();
    w.init_resource::<NextState<ActiveTool>>();
    w.resource_mut::<MaterialRegistry>().upsert(MaterialDef {
        id: "ground".into(),
        ..Default::default()
    });
    app
}

fn assignment(app: &mut App) -> Option<MaterialAssignment> {
    app.world_mut()
        .query::<(&ElementId, Option<&MaterialAssignment>)>()
        .iter(app.world())
        .find(|(id, _)| id.0 == 266)
        .unwrap()
        .1
        .cloned()
}

#[test]
fn terrain_material_survives_project_history_and_registered_factory_roundtrip() {
    use talos3d_core::authored_entity::AuthoredEntity;
    let mut source = app();
    TerrainSurfaceSnapshot {
        element_id: ElementId(266),
        surface: TerrainSurface::new("Site".into(), vec![]),
    }
    .apply_to(source.world_mut());
    let entity = source
        .world_mut()
        .query::<(Entity, &ElementId)>()
        .iter(source.world())
        .find(|(_, id)| id.0 == 266)
        .unwrap()
        .0;
    source.world_mut().entity_mut(entity).insert(Selected);
    source
        .world_mut()
        .resource_mut::<History>()
        .mark_save_point();
    execute_command(
        source.world_mut(),
        "materials.apply_to_selection",
        &serde_json::json!({"material_id":"ground"}),
    )
    .unwrap();
    let expected = Some(MaterialAssignment::new("ground"));
    assert_eq!(assignment(&mut source), expected);
    assert!(source.world().resource::<DocumentState>().dirty);
    assert_eq!(source.world().resource::<History>().undo_stack_len(), 1);
    source
        .world_mut()
        .resource_mut::<PendingCommandQueue>()
        .queue_undo();
    source.update();
    assert_eq!(assignment(&mut source), None);
    assert!(!source.world().resource::<DocumentState>().dirty);
    source
        .world_mut()
        .resource_mut::<PendingCommandQueue>()
        .queue_redo();
    source.update();
    assert_eq!(assignment(&mut source), expected);

    let captured = source
        .world()
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&source.world().entity(entity), source.world())
        .unwrap();
    // Registered factory restoration is also the clipboard and linked import path.
    let translated = captured.translate_by(Vec3::X);
    let registry = source.world().resource::<CapabilityRegistry>();
    let restored = registry
        .factory_for("terrain_surface")
        .unwrap()
        .from_persisted_json(&translated.to_persisted_json())
        .unwrap();
    assert_eq!(restored.material_assignment(), expected);
    assert!(restored.0.as_any().is::<TerrainSurfaceSnapshot>());
    assert_eq!(restored, translated);
    assert_ne!(restored, restored.set_material_assignment(None).unwrap());
    let bytes = capture_project_file_bytes(source.world_mut()).unwrap();
    let mut target = app();
    open_project_bytes(target.world_mut(), &bytes).unwrap();
    assert_eq!(assignment(&mut target), expected);
    execute_command(
        target.world_mut(),
        "materials.clear_assignment_on_selection",
        &serde_json::json!({}),
    )
    .unwrap();
    // Reload starts without a selection. Select the restored terrain explicitly.
    let entity = target
        .world_mut()
        .query::<(Entity, &ElementId)>()
        .iter(target.world())
        .find(|(_, id)| id.0 == 266)
        .unwrap()
        .0;
    target.world_mut().entity_mut(entity).insert(Selected);
    execute_command(
        target.world_mut(),
        "materials.clear_assignment_on_selection",
        &serde_json::json!({}),
    )
    .unwrap();
    let cleared = capture_project_file_bytes(target.world_mut()).unwrap();
    open_project_bytes(target.world_mut(), &cleared).unwrap();
    assert_eq!(assignment(&mut target), None);
}
