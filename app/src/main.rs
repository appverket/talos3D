use bevy::{
    app::AppExit,
    prelude::*,
    render::{
        settings::{Backends, PowerPreference, WgpuSettings},
        RenderPlugin,
    },
    window::{ExitCondition, WindowCloseRequested, WindowClosed},
};
#[cfg(feature = "model-api")]
use std::env;
use talos3d_architecture_core::ArchitectureCorePlugin;
use talos3d_architecture_elements::ArchitecturalPlugin;
use talos3d_architecture_product::ArchitectureProductPlugin;
use talos3d_core::capability_registry::DefaultsRegistry;
#[cfg(feature = "model-api")]
use talos3d_core::plugins::model_api::ModelApiPlugin;
#[cfg(feature = "perf-stats")]
use talos3d_core::plugins::perf_stats::PerfStatsPlugin;
use talos3d_core::plugins::{
    assistant_chat::AssistantChatPlugin,
    authoring_guidance::AuthoringGuidancePlugin,
    bundled_definition_libraries::BundledDefinitionLibrariesPlugin,
    camera::CameraPlugin,
    clipping_planes::ClippingPlanesPlugin,
    command_registry::CommandRegistryPlugin,
    commands::CommandPlugin,
    compass::CompassPlugin,
    cursor::CursorPlugin,
    definition_preview_scene::DefinitionPreviewPlugin,
    dimension_line::DimensionLinePlugin,
    document_properties::DocumentProperties,
    document_state::DocumentStatePlugin,
    drafting::DraftingPlugin,
    drafting_sheet::DraftingSheetPreviewPlugin,
    drawing_export::DrawingExportPlugin,
    egui_chrome::EguiChromePlugin,
    face_edit::FaceEditPlugin,
    grid::GridPlugin,
    guide_line::GuideLinePlugin,
    handles::HandlesPlugin,
    history::HistoryPlugin,
    identity::IdentityPlugin,
    import::ImportPlugin,
    inference::InferencePlugin,
    input_ownership::InputOwnershipPlugin,
    layers::LayerPlugin,
    lighting::LightingPlugin,
    materials::MaterialPlugin,
    modeling::ModelingPlugin,
    named_views::NamedViewsPlugin,
    palette::PalettePlugin,
    persistence::PersistencePlugin,
    property_edit::PropertyEditPlugin,
    render_pipeline::RenderPipelinePlugin,
    selection::SelectionPlugin,
    shading::ShadingPlugin,
    snap::SnapPlugin,
    storage::{LocalFileBackend, Storage},
    tape_tool::TapeToolPlugin,
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
    #[cfg(feature = "model-api")]
    configure_model_api_launch_from_args();

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(RenderPlugin {
                render_creation: WgpuSettings {
                    backends: Some(Backends::PRIMARY),
                    power_preference: PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    ..default()
                }
                .into(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Talos3D".into(),
                    canvas: Some("#bevy".into()),
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: false,
                    ..default()
                }),
                exit_condition: ExitCondition::OnPrimaryClosed,
                close_when_requested: true,
                ..default()
            }),
    )
    .insert_resource(Storage(Box::new(LocalFileBackend)))
    .init_state::<AppMode>()
    .add_plugins(AssistantChatPlugin)
    .add_plugins(AuthoringGuidancePlugin)
    .add_plugins(CameraPlugin)
    .add_plugins(CompassPlugin)
    .add_plugins(NamedViewsPlugin)
    .add_plugins(ClippingPlanesPlugin)
    .add_plugins(CommandRegistryPlugin)
    .add_plugins(DocumentStatePlugin)
    .add_plugins(DrawingExportPlugin)
    .add_plugins(GridPlugin)
    .add_plugins(CursorPlugin)
    .add_plugins(IdentityPlugin)
    .add_plugins(HistoryPlugin)
    .add_plugins(CommandPlugin)
    .add_plugins(ImportPlugin)
    .add_plugins(LayerPlugin)
    .add_plugins(MaterialPlugin)
    .add_plugins(GuideLinePlugin)
    .add_plugins(DimensionLinePlugin)
    .add_plugins(DraftingPlugin)
    .add_plugins(DraftingSheetPreviewPlugin)
    .add_plugins(LightingPlugin)
    .add_plugins(ModelingPlugin)
    .add_plugins(BundledDefinitionLibrariesPlugin)
    .add_plugins(DefinitionPreviewPlugin)
    .add_plugins(ArchitecturalPlugin)
    .add_plugins(ArchitectureProductPlugin)
    // PP70–PP78 semantic substrate: element classes, recipe families, domain
    // validators, catalog providers, and generation priors. Per ADR-037 these
    // live in a separate capability crate; the legacy ArchitecturalPlugin
    // above no longer registers them. CorpusGapPlugin provides the shared
    // CorpusGapQueue + CorpusPassageRegistry that PP78's MCP tools consume.
    .add_plugins(talos3d_core::plugins::corpus_gap::CorpusGapPlugin)
    .add_plugins(talos3d_core::plugins::recipe_drafts::RecipeDraftPlugin)
    .add_plugins(talos3d_core::plugins::assembly_pattern_drafts::AssemblyPatternDraftPlugin)
    .add_plugins(talos3d_core::plugins::session_draft_cache::SessionDraftCachePlugin)
    // PP79–PP80 curation substrate: SourceRegistry (seeded with Canonical
    // ISO 129-1 / ASME Y14.5 / ISO 80000-1) + NominationQueue.
    // See ADR-040 and the CURATION_SUBSTRATE agreement.
    .add_plugins(talos3d_core::curation::CurationPlugin)
    .add_plugins(ArchitectureCorePlugin);
    // ArchitectureProductPlugin is already registered in the builder chain above
    // (a second unconditional add here panics: "plugin was already added").

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
        .add_plugins(RenderPipelinePlugin)
        .add_plugins(EguiChromePlugin)
        .add_plugins(ToolbarPlugin)
        .add_plugins(PalettePlugin)
        .add_plugins(ToolPlugin)
        .add_plugins(TapeToolPlugin)
        .add_systems(Startup, init_document_properties)
        .add_systems(Update, exit_on_window_close_events);

    #[cfg(feature = "perf-stats")]
    app.add_plugins(PerfStatsPlugin);

    app.run();
}

#[cfg(feature = "model-api")]
fn configure_model_api_launch_from_args() {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instance-id" => {
                if let Some(value) = args.next() {
                    env::set_var("TALOS3D_INSTANCE_ID", value);
                }
            }
            "--model-api-port" => {
                if let Some(value) = args.next() {
                    env::set_var("TALOS3D_MODEL_API_PORT", value);
                }
            }
            "--instance-registry-dir" => {
                if let Some(value) = args.next() {
                    env::set_var("TALOS3D_INSTANCE_REGISTRY_DIR", value);
                }
            }
            _ => {}
        }
    }
}

fn init_document_properties(world: &mut World) {
    let mut props = DocumentProperties::default();
    if let Some(registry) = world.get_resource::<DefaultsRegistry>() {
        registry.apply_all(&mut props);
    }
    world.insert_resource(props);
}

fn exit_on_window_close_events(
    mut close_requests: MessageReader<WindowCloseRequested>,
    mut closed_windows: MessageReader<WindowClosed>,
    mut app_exit: MessageWriter<AppExit>,
) {
    if close_requests.read().next().is_some() || closed_windows.read().next().is_some() {
        app_exit.write(AppExit::Success);
    }
}
