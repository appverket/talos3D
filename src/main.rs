use bevy::{
    app::AppExit,
    prelude::*,
    window::{ExitCondition, WindowCloseRequested},
};
use talos3d_architectural::ArchitecturalPlugin;
use talos3d_core::capability_registry::DefaultsRegistry;
#[cfg(feature = "model-api")]
use talos3d_core::plugins::model_api::ModelApiPlugin;
#[cfg(feature = "perf-stats")]
use talos3d_core::plugins::perf_stats::PerfStatsPlugin;
use talos3d_core::plugins::{
    camera::CameraPlugin,
    command_registry::CommandRegistryPlugin,
    commands::CommandPlugin,
    cursor::CursorPlugin,
    document_properties::DocumentProperties,
    document_state::DocumentStatePlugin,
    egui_chrome::EguiChromePlugin,
    face_edit::FaceEditPlugin,
    grid::GridPlugin,
    handles::HandlesPlugin,
    history::HistoryPlugin,
    identity::IdentityPlugin,
    import::ImportPlugin,
    inference::InferencePlugin,
    input_ownership::InputOwnershipPlugin,
    layers::LayerPlugin,
    modeling::ModelingPlugin,
    palette::PalettePlugin,
    persistence::PersistencePlugin,
    property_edit::PropertyEditPlugin,
    selection::SelectionPlugin,
    shading::ShadingPlugin,
    snap::SnapPlugin,
    storage::{LocalFileBackend, Storage},
    toolbar::ToolbarPlugin,
    tools::ToolPlugin,
    transform::TransformPlugin,
    ui::UiPlugin,
};
#[cfg(feature = "terrain")]
use talos3d_terrain::TerrainPlugin;

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum AppMode {
    #[default]
    Idle,
    Drafting,
    Viewing,
}

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Talos3D".into(),
            canvas: Some("#bevy".into()),
            fit_canvas_to_parent: true,
            prevent_default_event_handling: false,
            ..default()
        }),
        exit_condition: ExitCondition::DontExit,
        close_when_requested: false,
        ..default()
    }))
    .insert_resource(Storage(Box::new(LocalFileBackend)))
    .init_state::<AppMode>()
    .add_plugins(CameraPlugin)
    .add_plugins(CommandRegistryPlugin)
    .add_plugins(DocumentStatePlugin)
    .add_plugins(GridPlugin)
    .add_plugins(CursorPlugin)
    .add_plugins(IdentityPlugin)
    .add_plugins(HistoryPlugin)
    .add_plugins(CommandPlugin)
    .add_plugins(ImportPlugin)
    .add_plugins(LayerPlugin)
    .add_plugins(ModelingPlugin)
    .add_plugins(ArchitecturalPlugin);

    #[cfg(feature = "terrain")]
    app.add_plugins(TerrainPlugin);

    #[cfg(feature = "model-api")]
    app.add_plugins(ModelApiPlugin);

    app.add_plugins(InputOwnershipPlugin)
        .add_plugins(SelectionPlugin)
        .add_plugins(FaceEditPlugin)
        .add_plugins(InferencePlugin)
        .add_plugins(HandlesPlugin)
        .add_plugins(TransformPlugin)
        .add_plugins(PropertyEditPlugin)
        .add_plugins(ShadingPlugin)
        .add_plugins(SnapPlugin)
        .add_plugins(PersistencePlugin)
        .add_plugins(UiPlugin)
        .add_plugins(EguiChromePlugin)
        .add_plugins(ToolbarPlugin)
        .add_plugins(PalettePlugin)
        .add_plugins(ToolPlugin)
        .add_systems(Startup, (init_document_properties, setup_lighting))
        .add_systems(Update, exit_on_close_request);

    #[cfg(feature = "perf-stats")]
    app.add_plugins(PerfStatsPlugin);

    app.run();
}

fn init_document_properties(world: &mut World) {
    let mut props = DocumentProperties::default();
    if let Some(registry) = world.get_resource::<DefaultsRegistry>() {
        registry.apply_all(&mut props);
    }
    world.insert_resource(props);
}

fn setup_lighting(mut commands: Commands) {
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.9, 0.92, 1.0),
        brightness: 120.0,
        affects_lightmapped_meshes: true,
    });

    commands.spawn((
        DirectionalLight {
            color: Color::srgb(1.0, 0.97, 0.88),
            illuminance: 12_000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(10.0, 20.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.6, 0.7, 0.9),
            illuminance: 2_000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(-8.0, 4.0, -6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

fn exit_on_close_request(
    mut close_requests: MessageReader<WindowCloseRequested>,
    mut app_exit: MessageWriter<AppExit>,
) {
    if close_requests.read().next().is_some() {
        app_exit.write(AppExit::Success);
    }
}
