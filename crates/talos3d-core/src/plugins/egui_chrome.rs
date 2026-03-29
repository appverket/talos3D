use bevy::{ecs::system::SystemParam, prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPlugin};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{overlay_text as perf_overlay_text, PerfStats};
use crate::plugins::{
    command_registry::{
        ordered_menu_categories, queue_command_invocation_resource, CommandDescriptor,
        CommandRegistry, IconRegistry, PendingCommandInvocations,
    },
    commands::ApplyEntityChangesCommand,
    cursor::{CursorWorldPos, ViewportUiInset},
    document_properties::DocumentProperties,
    import::{
        apply_import_review_to_pending, DocumentImportPlacementState, ImportOriginMode,
        ImportProgressState, ImportReviewState, ImportedLayerPanelState, PendingImportCommit,
    },
    menu_bar::MenuBarState,
    palette::PaletteState,
    property_edit::{
        parse_property_value, shared_property_value, PropertyEditState, PropertyPanelData,
        PropertyPanelState,
    },
    selection::Selected,
    toolbar::{
        apply_toolbar_layout_change, set_toolbar_visibility, ToolbarDescriptor, ToolbarDock,
        ToolbarLayoutState, ToolbarRegistry,
    },
    tools::ActiveTool,
    transform::TransformState,
    ui::{coordinate_text, hint_text, StatusBarData},
};

const CHROME_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 23, 28, 245);
const CHROME_ACCENT: egui::Color32 = egui::Color32::from_rgba_premultiplied(56, 82, 115, 245);
const CHROME_HOVER: egui::Color32 = egui::Color32::from_rgba_premultiplied(46, 56, 71, 245);
const CHROME_TEXT: egui::Color32 = egui::Color32::from_rgb(235, 240, 248);
const CHROME_MUTED: egui::Color32 = egui::Color32::from_rgb(160, 168, 182);
const TOOLBAR_BUTTON_SIZE: f32 = 32.0;
const TOOLBAR_ICON_SIZE: f32 = 18.0;
const TOOLBAR_GRIP_SIZE: f32 = 18.0;
const TOOLBAR_GRIP_DOT_SIZE: f32 = 2.0;
const TOOLBAR_SIDE_WIDTH: f32 = TOOLBAR_BUTTON_SIZE + 16.0;
const DOCK_TARGET_SIZE: f32 = 56.0;
const PROPERTY_PANEL_WIDTH: f32 = 240.0;
#[cfg(feature = "perf-stats")]
const PERF_OVERLAY_WIDTH: f32 = 168.0;

pub struct EguiChromePlugin;

/// System set for the egui chrome draw pass.  Other systems that depend on
/// `ViewportUiInset` (e.g. camera viewport) should run `.after(EguiChromeSystems)`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct EguiChromeSystems;

impl Plugin for EguiChromePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin {
            #[allow(deprecated)]
            enable_multipass_for_primary_context: false,
            // Disable bindless textures — not yet supported on Metal
            // (see https://github.com/bevyengine/bevy/issues/18149)
            bindless_mode_array_size: None,
            ..default()
        })
        .init_resource::<MenuBarState>()
        .init_resource::<EguiWantsInput>()
        .init_resource::<ToolbarDragState>()
        .add_systems(Update, draw_egui_chrome.in_set(EguiChromeSystems));
    }
}

#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct EguiWantsInput {
    pub pointer: bool,
    pub keyboard: bool,
}

#[derive(Resource, Default, Debug, Clone)]
struct ToolbarDragState {
    active: Option<ActiveToolbarDrag>,
}

#[derive(Debug, Clone)]
struct ActiveToolbarDrag {
    toolbar_id: String,
    original_dock: ToolbarDock,
    target_dock: Option<ToolbarDock>,
}

struct ToolbarRenderContext<'a, 'w, 's> {
    contexts: &'a mut EguiContexts<'w, 's>,
    command_registry: &'a CommandRegistry,
    icon_registry: &'a IconRegistry,
    toolbar_registry: &'a ToolbarRegistry,
    toolbar_layout_state: &'a mut ToolbarLayoutState,
    doc_props: &'a mut DocumentProperties,
    pending_command_invocations: &'a mut PendingCommandInvocations,
    drag_state: &'a mut ToolbarDragState,
    selection_count: usize,
    current_tool: &'a str,
}

#[derive(SystemParam)]
struct ChromeData<'w, 's> {
    command_registry: Res<'w, CommandRegistry>,
    icon_registry: Res<'w, IconRegistry>,
    toolbar_registry: Res<'w, ToolbarRegistry>,
    toolbar_layout_state: ResMut<'w, ToolbarLayoutState>,
    pending_command_invocations: ResMut<'w, PendingCommandInvocations>,
    apply_entity_changes: ResMut<'w, Messages<ApplyEntityChangesCommand>>,
    status_bar_data: ResMut<'w, StatusBarData>,
    doc_props: ResMut<'w, DocumentProperties>,
    import_progress: Res<'w, ImportProgressState>,
    import_review_state: ResMut<'w, ImportReviewState>,
    import_placement_state: ResMut<'w, DocumentImportPlacementState>,
    imported_layer_panel_state: ResMut<'w, ImportedLayerPanelState>,
    pending_import_commit: ResMut<'w, PendingImportCommit>,
    layer_registry: ResMut<'w, crate::plugins::layers::LayerRegistry>,
    layer_state: ResMut<'w, crate::plugins::layers::LayerState>,
    cursor_world_pos: Res<'w, CursorWorldPos>,
    active_tool: Res<'w, State<ActiveTool>>,
    selected_query: Query<'w, 's, (), With<Selected>>,
    property_edit_state: ResMut<'w, PropertyEditState>,
    property_panel_state: ResMut<'w, PropertyPanelState>,
    property_panel_data: Res<'w, PropertyPanelData>,
    palette_state: Res<'w, PaletteState>,
    transform_state: Res<'w, TransformState>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    window_query: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    menu_bar_state: ResMut<'w, MenuBarState>,
    viewport_ui_inset: ResMut<'w, ViewportUiInset>,
    egui_wants_input: ResMut<'w, EguiWantsInput>,
    drag_state: ResMut<'w, ToolbarDragState>,
    #[cfg(feature = "perf-stats")]
    perf_stats: Res<'w, PerfStats>,
}

fn draw_egui_chrome(mut contexts: EguiContexts, mut data: ChromeData) {
    let Ok(ctx_ref) = contexts.ctx_mut() else {
        warn!("draw_egui_chrome: ctx_mut() failed");
        return;
    };
    let ctx = ctx_ref.clone();

    apply_chrome_visuals(&ctx);

    if data.keys.just_pressed(KeyCode::F10) {
        data.menu_bar_state.visible = !data.menu_bar_state.visible;
    }
    if data.keys.just_pressed(KeyCode::Escape) {
        #[allow(deprecated)]
        ctx.memory_mut(|memory| memory.close_all_popups());
    }

    let selection_count = data.selected_query.iter().count();
    let current_tool = format!("{:?}", data.active_tool.get());
    let mut hovered_menu_hint = None;

    if data.property_panel_state.visible && data.property_edit_state.is_active() {
        data.property_edit_state.clear();
    }

    if data.menu_bar_state.visible {
        egui::TopBottomPanel::top("menu_bar")
            .resizable(false)
            .show(&ctx, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    for category in ordered_menu_categories(&data.command_registry) {
                        ui.menu_button(category.label(), |ui| {
                            for descriptor in
                                data.command_registry.commands().filter(|descriptor| {
                                    descriptor.show_in_menu && descriptor.category == category
                                })
                            {
                                let enabled = !descriptor.requires_selection || selection_count > 0;
                                let label = if let Some(shortcut) = &descriptor.default_shortcut {
                                    format!("{}    {shortcut}", descriptor.label)
                                } else {
                                    descriptor.label.clone()
                                };
                                let response = ui.add_enabled(enabled, egui::Button::new(label));
                                if enabled && response.contains_pointer() {
                                    hovered_menu_hint = descriptor.hint.clone();
                                }
                                if enabled
                                    && response.contains_pointer()
                                    && ui.ctx().input(|i| i.pointer.primary_released())
                                {
                                    queue_command_invocation_resource(
                                        &mut data.pending_command_invocations,
                                        descriptor.id.clone(),
                                        serde_json::json!({}),
                                    );
                                    ui.close();
                                }
                            }
                        });
                    }
                });
            });
    }

    let top_toolbars = ordered_toolbars_for_dock(
        &data.toolbar_registry,
        &data.toolbar_layout_state,
        ToolbarDock::Top,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    for descriptor in top_toolbars {
        let mut render = ToolbarRenderContext {
            contexts: &mut contexts,
            command_registry: &data.command_registry,
            icon_registry: &data.icon_registry,
            toolbar_registry: &data.toolbar_registry,
            toolbar_layout_state: &mut data.toolbar_layout_state,
            doc_props: &mut data.doc_props,
            pending_command_invocations: &mut data.pending_command_invocations,
            drag_state: &mut data.drag_state,
            selection_count,
            current_tool: &current_tool,
        };
        draw_toolbar_panel(&ctx, &mut render, &descriptor, ToolbarDock::Top);
    }

    let left_toolbars = ordered_toolbars_for_dock(
        &data.toolbar_registry,
        &data.toolbar_layout_state,
        ToolbarDock::Left,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    for descriptor in left_toolbars {
        let mut render = ToolbarRenderContext {
            contexts: &mut contexts,
            command_registry: &data.command_registry,
            icon_registry: &data.icon_registry,
            toolbar_registry: &data.toolbar_registry,
            toolbar_layout_state: &mut data.toolbar_layout_state,
            doc_props: &mut data.doc_props,
            pending_command_invocations: &mut data.pending_command_invocations,
            drag_state: &mut data.drag_state,
            selection_count,
            current_tool: &current_tool,
        };
        draw_toolbar_panel(&ctx, &mut render, &descriptor, ToolbarDock::Left);
    }

    let right_toolbars = ordered_toolbars_for_dock(
        &data.toolbar_registry,
        &data.toolbar_layout_state,
        ToolbarDock::Right,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    for descriptor in right_toolbars {
        let mut render = ToolbarRenderContext {
            contexts: &mut contexts,
            command_registry: &data.command_registry,
            icon_registry: &data.icon_registry,
            toolbar_registry: &data.toolbar_registry,
            toolbar_layout_state: &mut data.toolbar_layout_state,
            doc_props: &mut data.doc_props,
            pending_command_invocations: &mut data.pending_command_invocations,
            drag_state: &mut data.drag_state,
            selection_count,
            current_tool: &current_tool,
        };
        draw_toolbar_panel(&ctx, &mut render, &descriptor, ToolbarDock::Right);
    }

    let bottom_toolbars = ordered_toolbars_for_dock(
        &data.toolbar_registry,
        &data.toolbar_layout_state,
        ToolbarDock::Bottom,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    for descriptor in bottom_toolbars {
        let mut render = ToolbarRenderContext {
            contexts: &mut contexts,
            command_registry: &data.command_registry,
            icon_registry: &data.icon_registry,
            toolbar_registry: &data.toolbar_registry,
            toolbar_layout_state: &mut data.toolbar_layout_state,
            doc_props: &mut data.doc_props,
            pending_command_invocations: &mut data.pending_command_invocations,
            drag_state: &mut data.drag_state,
            selection_count,
            current_tool: &current_tool,
        };
        draw_toolbar_panel(&ctx, &mut render, &descriptor, ToolbarDock::Bottom);
    }

    draw_property_panel(&ctx, &mut data);
    draw_import_review_window(&ctx, &mut data);
    draw_imported_layers_window(&ctx, &mut data);
    draw_import_progress_window(&ctx, &data);

    draw_dock_targets(&ctx, &data.drag_state);
    if let Some(active_drag) = data.drag_state.active.as_mut() {
        active_drag.target_dock = ctx
            .pointer_hover_pos()
            .and_then(|pos| dock_target_for_position(&ctx, pos));
    }
    complete_toolbar_drag(
        &ctx,
        &mut data.drag_state,
        &mut data.toolbar_layout_state,
        &mut data.doc_props,
    );

    data.status_bar_data.command_hint = hovered_menu_hint;
    let status_hint = hint_text(&data.status_bar_data);
    let coordinates = coordinate_text(&data.cursor_world_pos, &data.doc_props);
    egui::TopBottomPanel::bottom("status_bar")
        .resizable(false)
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!(
                    "[{}]",
                    data.status_bar_data.tool_name
                )));
                ui.separator();
                ui.label(coordinates);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(status_hint);
                });
            });
        });

    let Ok(window) = data.window_query.single() else {
        return;
    };
    let available = ctx.available_rect();
    #[cfg(feature = "perf-stats")]
    draw_perf_overlay(&ctx, available, &data.perf_stats);

    // With the patched bevy_egui, screen_rect is always the full window,
    // so available_rect() gives absolute window-relative coordinates.
    data.viewport_ui_inset.top = available.min.y;
    data.viewport_ui_inset.bottom = (window.height() - available.max.y).max(0.0);
    data.viewport_ui_inset.left = available.min.x;
    data.viewport_ui_inset.right = (window.width() - available.max.x).max(0.0);

    let wants_ptr = ctx.wants_pointer_input();
    let over_area = ctx.is_pointer_over_area();
    data.egui_wants_input.pointer = wants_ptr || over_area;
    data.egui_wants_input.keyboard = ctx.wants_keyboard_input();
}

fn draw_property_panel(ctx: &egui::Context, data: &mut ChromeData) {
    data.property_panel_state.interacting = false;

    if data.property_panel_data.snapshots.is_empty() {
        data.property_panel_state.visible = false;
        return;
    }

    let blocked = data.palette_state.is_open()
        || !data.transform_state.is_idle()
        || data.property_edit_state.is_active();

    // Position in top-right of the available area
    let available = ctx.available_rect();
    let default_pos = egui::pos2(
        available.max.x - PROPERTY_PANEL_WIDTH - 8.0,
        available.min.y + 8.0,
    );

    let mut open = true;
    let response = egui::Window::new(property_panel_title(
        data.property_panel_data.entity_type,
        data.property_panel_data.snapshots.len(),
        data.property_panel_data.mixed_selection,
    ))
    .id(egui::Id::new("property_panel"))
    .default_pos(default_pos)
    .default_width(PROPERTY_PANEL_WIDTH)
    .resizable(false)
    .collapsible(true)
    .open(&mut open)
    .show(ctx, |ui| {
        if data.property_panel_data.mixed_selection {
            ui.label(format!(
                "Mixed selection ({} entities)",
                data.property_panel_data.snapshots.len()
            ));
            return;
        }

        let Some(first) = data.property_panel_data.snapshots.first() else {
            return;
        };
        let fields = first.property_fields();
        let sections = property_panel_sections(&fields);
        let mut pending_action = None;
        let mut pending_status_error = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for section in sections {
                egui::CollapsingHeader::new(section.title)
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::Grid::new(format!("property_panel_grid_{}", section.title))
                            .num_columns(2)
                            .spacing([10.0, 8.0])
                            .show(ui, |ui| {
                                for (index, field) in section.fields {
                                    let shared_value = shared_property_value(
                                        &data.property_panel_data.snapshots,
                                        field.name,
                                    );
                                    let field_id = format!(
                                        "{}:{}",
                                        data.property_panel_data.entity_type.unwrap_or("mixed"),
                                        field.name
                                    );
                                    let active = data.property_panel_state.active_field.as_deref()
                                        == Some(field_id.as_str());

                                    ui.label(field.label);
                                    if active {
                                        if !field.editable {
                                            data.property_panel_state.active_field = None;
                                            data.property_panel_state.buffer.clear();
                                        }
                                        let mut buffer = data.property_panel_state.buffer.clone();
                                        let response = ui.add_enabled(
                                            !blocked && field.editable,
                                            egui::TextEdit::singleline(&mut buffer)
                                                .desired_width(150.0),
                                        );
                                        if !blocked && field.editable && !response.has_focus() {
                                            response.request_focus();
                                        }
                                        if response.changed() {
                                            data.property_panel_state.buffer = buffer.clone();
                                        }
                                        if field.editable
                                            && response.has_focus()
                                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                                        {
                                            match parse_panel_property_value(
                                                field.name,
                                                field.kind.clone(),
                                                &buffer,
                                                &data.doc_props,
                                            ) {
                                                Ok(value) => {
                                                    pending_action = Some(
                                                        PropertyPanelAction::CommitAndFocusNext {
                                                            index,
                                                            value,
                                                        },
                                                    )
                                                }
                                                Err(error) => pending_status_error = Some(error),
                                            }
                                        } else if field.editable
                                            && response.has_focus()
                                            && ui.input(|input| input.key_pressed(egui::Key::Tab))
                                        {
                                            let backwards = ui.input(|input| input.modifiers.shift);
                                            pending_action = Some(PropertyPanelAction::FocusNext {
                                                index,
                                                backwards,
                                            });
                                        } else if field.editable
                                            && response.has_focus()
                                            && ui
                                                .input(|input| input.key_pressed(egui::Key::Escape))
                                        {
                                            pending_action = Some(PropertyPanelAction::CancelEdit);
                                        }
                                    } else {
                                        let display = shared_value
                                            .as_ref()
                                            .map(|value| {
                                                format_panel_property_value(
                                                    value,
                                                    field.name,
                                                    &data.doc_props,
                                                )
                                            })
                                            .unwrap_or_else(|| "---".to_string());
                                        let button = egui::Button::new(display)
                                            .fill(if field.editable {
                                                CHROME_HOVER
                                            } else {
                                                CHROME_BG
                                            })
                                            .min_size(egui::vec2(150.0, 22.0));
                                        let editable = !blocked && field.editable;
                                        let response = ui.add_enabled(editable, button);
                                        if editable
                                            && response.contains_pointer()
                                            && ui.ctx().input(|i| i.pointer.primary_released())
                                        {
                                            focus_property_panel_field(
                                                &mut data.property_panel_state,
                                                data.property_panel_data
                                                    .entity_type
                                                    .unwrap_or("mixed"),
                                                field.name,
                                                &shared_value,
                                                &data.doc_props,
                                            );
                                        }
                                        if !field.editable && response.contains_pointer() {
                                            egui::Tooltip::always_open(
                                                ui.ctx().clone(),
                                                response.layer_id,
                                                response.id.with("tip"),
                                                egui::PopupAnchor::Pointer,
                                            )
                                            .gap(12.0)
                                            .show(
                                                |ui| {
                                                    ui.label("Read-only");
                                                },
                                            );
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            }
        });

        if let Some(error) = pending_status_error {
            data.status_bar_data.set_feedback(error, 2.0);
        }
        if let Some(action) = pending_action {
            apply_property_panel_action(data, &fields, action);
        }
    });

    data.property_panel_state.visible = open;
    data.property_panel_state.interacting = response
        .map(|r| r.response.contains_pointer())
        .unwrap_or(false)
        || ctx.wants_keyboard_input();
}

fn draw_import_review_window(ctx: &egui::Context, data: &mut ChromeData) {
    if data.import_review_state.requests.is_empty() {
        return;
    }

    let mut commit_requested = false;
    let mut cancel_requested = false;
    let source_name = data
        .import_review_state
        .source_name
        .clone()
        .unwrap_or_else(|| "Import".to_string());

    egui::Window::new(format!("Review Import: {source_name}"))
        .collapsible(false)
        .resizable(true)
        .default_width(360.0)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 56.0))
        .show(ctx, |ui| {
            ui.label(format!(
                "{} parsed entities",
                data.import_review_state.requests.len()
            ));
            ui.separator();

            ui.label("Transform");
            ui.horizontal(|ui| {
                ui.label("Unit scale");
                ui.add(
                    egui::DragValue::new(&mut data.import_review_state.settings.unit_scale)
                        .speed(0.01)
                        .range(0.0001..=10_000.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Placement");
                egui::ComboBox::from_id_salt("import_origin_mode")
                    .selected_text(data.import_review_state.settings.origin_mode.label())
                    .show_ui(ui, |ui| {
                        for mode in ImportOriginMode::ALL {
                            ui.selectable_value(
                                &mut data.import_review_state.settings.origin_mode,
                                mode,
                                mode.label(),
                            );
                        }
                    });
            });
            ui.label(
                egui::RichText::new(data.import_review_state.settings.origin_mode.description())
                    .small()
                    .color(CHROME_MUTED),
            );
            if data.import_review_state.settings.origin_mode == ImportOriginMode::DocumentLocalOrigin
            {
                let status = if let Some(offset) = data.import_placement_state.local_origin_offset {
                    format!(
                        "Document import offset: x {:.2}, y {:.2}, z {:.2}",
                        offset.x, offset.y, offset.z
                    )
                } else {
                    "This import will establish the document-local offset from its bounds."
                        .to_string()
                };
                ui.label(egui::RichText::new(status).small().color(CHROME_MUTED));
            }
            ui.horizontal(|ui| {
                ui.label("Manual offset");
                ui.add(
                    egui::DragValue::new(&mut data.import_review_state.settings.origin_offset.x)
                        .speed(0.1)
                        .prefix("x "),
                );
                ui.add(
                    egui::DragValue::new(&mut data.import_review_state.settings.origin_offset.y)
                        .speed(0.1)
                        .prefix("y "),
                );
                ui.add(
                    egui::DragValue::new(&mut data.import_review_state.settings.origin_offset.z)
                        .speed(0.1)
                        .prefix("z "),
                );
            });

            ui.separator();
            ui.label("Layers");
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for layer in &mut data.import_review_state.layers {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut layer.include, "");
                        ui.checkbox(&mut layer.visible, "");
                        ui.label(format!("{} ({})", layer.name, layer.count));
                    });
                }
            });
            ui.label(egui::RichText::new("First checkbox imports the layer. Second controls initial visibility after import.").small().color(CHROME_MUTED));

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel_requested = true;
                }
                let import_enabled = data.import_review_state.layers.iter().any(|layer| layer.include);
                if ui.add_enabled(import_enabled, egui::Button::new("Import")).clicked() {
                    commit_requested = true;
                }
            });
        });

    if cancel_requested {
        *data.import_review_state = ImportReviewState::default();
        data.status_bar_data
            .set_feedback("Import cancelled".to_string(), 2.0);
    } else if commit_requested {
        match apply_import_review_to_pending(
            &mut data.import_review_state,
            &mut data.pending_import_commit,
            &mut data.import_placement_state,
            &mut data.imported_layer_panel_state,
        ) {
            Ok(()) => data
                .status_bar_data
                .set_feedback("Import queued".to_string(), 2.0),
            Err(error) => data.status_bar_data.set_feedback(error, 2.0),
        }
    }
}

fn draw_imported_layers_window(ctx: &egui::Context, data: &mut ChromeData) {
    // Only show the layer panel if there are layers beyond Default
    if data.layer_registry.layers.len() <= 1 {
        return;
    }

    egui::Window::new("Layers")
        .default_width(280.0)
        .resizable(true)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 360.0))
        .show(ctx, |ui| {
            let sorted = data
                .layer_registry
                .sorted_layers()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            for layer_def in &sorted {
                let is_active = data.layer_state.active_layer == layer_def.name;

                ui.horizontal(|ui| {
                    let mut visible = layer_def.visible;
                    if ui.checkbox(&mut visible, "").changed() {
                        if let Some(def) = data.layer_registry.layers.get_mut(&layer_def.name) {
                            def.visible = visible;
                        }
                    }

                    let label_text = if is_active {
                        format!("▸ {}", layer_def.name)
                    } else {
                        format!("  {}", layer_def.name)
                    };

                    if ui.selectable_label(is_active, label_text).clicked() {
                        data.layer_state.active_layer = layer_def.name.clone();
                        if let Some(def) = data.layer_registry.layers.get_mut(&layer_def.name) {
                            def.visible = true;
                            def.locked = false;
                        }
                    }
                });
            }

            ui.separator();
            if ui.button("+ Add Layer").clicked() {
                let name = data.layer_registry.generate_unique_name();
                data.layer_registry.create_layer(name);
            }
        });
}

fn draw_import_progress_window(ctx: &egui::Context, data: &ChromeData) {
    let Some(started_at) = data.import_progress.started_at else {
        return;
    };
    if started_at.elapsed().as_secs_f32() < 0.5 {
        return;
    }

    egui::Window::new("Importing")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 56.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "Parsing {}...",
                    data.import_progress
                        .source_name
                        .as_deref()
                        .unwrap_or("file")
                ));
            });
        });
}

enum PropertyPanelAction {
    CommitAndFocusNext {
        index: usize,
        value: crate::authored_entity::PropertyValue,
    },
    FocusNext {
        index: usize,
        backwards: bool,
    },
    CancelEdit,
}

fn apply_property_panel_action(
    data: &mut ChromeData,
    fields: &[crate::authored_entity::PropertyFieldDef],
    action: PropertyPanelAction,
) {
    match action {
        PropertyPanelAction::CommitAndFocusNext { index, value } => {
            let Some(field) = fields.get(index) else {
                return;
            };
            let before = data.property_panel_data.snapshots.clone();
            let after = before
                .iter()
                .map(|snapshot| snapshot.set_property_json(field.name, &value.to_json()))
                .collect::<Result<Vec<_>, _>>();
            match after {
                Ok(after) => {
                    if before != after {
                        data.apply_entity_changes.write(ApplyEntityChangesCommand {
                            label: "Edit properties",
                            before: before.clone(),
                            after: after.clone(),
                        });
                    }
                    if fields.len() > 1 {
                        focus_index_in_property_panel(
                            &mut data.property_panel_state,
                            data.property_panel_data.entity_type.unwrap_or("mixed"),
                            fields,
                            (index + 1) % fields.len(),
                            &after,
                            &data.doc_props,
                        );
                    } else {
                        data.property_panel_state.active_field = None;
                        data.property_panel_state.buffer.clear();
                    }
                }
                Err(error) => data.status_bar_data.set_feedback(error, 2.0),
            }
        }
        PropertyPanelAction::FocusNext { index, backwards } => {
            if fields.is_empty() {
                data.property_panel_state.active_field = None;
                data.property_panel_state.buffer.clear();
                return;
            }
            let len = fields.len();
            let next_index = if backwards {
                index.checked_sub(1).unwrap_or(len - 1)
            } else {
                (index + 1) % len
            };
            focus_index_in_property_panel(
                &mut data.property_panel_state,
                data.property_panel_data.entity_type.unwrap_or("mixed"),
                fields,
                next_index,
                &data.property_panel_data.snapshots,
                &data.doc_props,
            );
        }
        PropertyPanelAction::CancelEdit => {
            data.property_panel_state.active_field = None;
            data.property_panel_state.buffer.clear();
        }
    }
}

struct PropertyPanelSection<'a> {
    title: &'static str,
    fields: Vec<(usize, &'a crate::authored_entity::PropertyFieldDef)>,
}

fn property_panel_sections(
    fields: &[crate::authored_entity::PropertyFieldDef],
) -> Vec<PropertyPanelSection<'_>> {
    let mut transform_fields = Vec::new();
    let mut property_fields = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if is_transform_property_field(field.name) {
            transform_fields.push((index, field));
        } else {
            property_fields.push((index, field));
        }
    }

    let mut sections = Vec::new();
    if !transform_fields.is_empty() {
        sections.push(PropertyPanelSection {
            title: "Transform",
            fields: transform_fields,
        });
    }
    if !property_fields.is_empty() {
        sections.push(PropertyPanelSection {
            title: "Properties",
            fields: property_fields,
        });
    }
    sections
}

fn is_transform_property_field(field_name: &str) -> bool {
    matches!(
        field_name,
        "start" | "end" | "centre" | "center" | "corner_a" | "corner_b" | "elevation"
    )
}

fn focus_index_in_property_panel(
    panel_state: &mut PropertyPanelState,
    entity_type: &str,
    fields: &[crate::authored_entity::PropertyFieldDef],
    index: usize,
    snapshots: &[crate::authored_entity::BoxedEntity],
    doc_props: &DocumentProperties,
) {
    let Some(field) = fields.get(index) else {
        panel_state.active_field = None;
        panel_state.buffer.clear();
        return;
    };
    let shared_value = shared_property_value(snapshots, field.name);
    focus_property_panel_field(
        panel_state,
        entity_type,
        field.name,
        &shared_value,
        doc_props,
    );
}

fn focus_property_panel_field(
    panel_state: &mut PropertyPanelState,
    entity_type: &str,
    field_name: &str,
    value: &Option<crate::authored_entity::PropertyValue>,
    doc_props: &DocumentProperties,
) {
    panel_state.active_field = Some(format!("{entity_type}:{field_name}"));
    panel_state.buffer = value
        .as_ref()
        .map(|value| edit_buffer_for_property_value(value, field_name, doc_props))
        .unwrap_or_default();
}

fn property_panel_title(
    entity_type: Option<&'static str>,
    count: usize,
    mixed_selection: bool,
) -> String {
    if mixed_selection {
        return format!("Mixed Selection ({count})");
    }
    match (entity_type, count) {
        (Some(entity_type), 1) => display_entity_type_name(entity_type),
        (Some(entity_type), count) => format!("{count} {}s", display_entity_type_name(entity_type)),
        (None, _) => "Selection".to_string(),
    }
}

fn display_entity_type_name(entity_type: &str) -> String {
    entity_type
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_panel_property_value(
    value: &crate::authored_entity::PropertyValue,
    field_name: &str,
    doc_props: &DocumentProperties,
) -> String {
    match value {
        crate::authored_entity::PropertyValue::Scalar(value)
            if scalar_uses_display_units(field_name) =>
        {
            doc_props
                .display_unit
                .format_value(*value, doc_props.precision)
        }
        crate::authored_entity::PropertyValue::Scalar(value) => format!("{value:.2}"),
        crate::authored_entity::PropertyValue::Vec2(value) => format!(
            "{:.2}, {:.2}",
            doc_props.display_unit.from_metres(value.x),
            doc_props.display_unit.from_metres(value.y)
        ),
        crate::authored_entity::PropertyValue::Vec3(value) => format!(
            "{:.2}, {:.2}, {:.2}",
            doc_props.display_unit.from_metres(value.x),
            doc_props.display_unit.from_metres(value.y),
            doc_props.display_unit.from_metres(value.z)
        ),
        crate::authored_entity::PropertyValue::Text(value) => value.clone(),
    }
}

fn edit_buffer_for_property_value(
    value: &crate::authored_entity::PropertyValue,
    field_name: &str,
    doc_props: &DocumentProperties,
) -> String {
    match value {
        crate::authored_entity::PropertyValue::Scalar(value)
            if scalar_uses_display_units(field_name) =>
        {
            format!("{:.2}", doc_props.display_unit.from_metres(*value))
        }
        crate::authored_entity::PropertyValue::Scalar(value) => format!("{value:.2}"),
        crate::authored_entity::PropertyValue::Vec2(value) => format!(
            "{:.2}, {:.2}",
            doc_props.display_unit.from_metres(value.x),
            doc_props.display_unit.from_metres(value.y)
        ),
        crate::authored_entity::PropertyValue::Vec3(value) => format!(
            "{:.2}, {:.2}, {:.2}",
            doc_props.display_unit.from_metres(value.x),
            doc_props.display_unit.from_metres(value.y),
            doc_props.display_unit.from_metres(value.z)
        ),
        crate::authored_entity::PropertyValue::Text(value) => value.clone(),
    }
}

fn parse_panel_property_value(
    field_name: &str,
    kind: crate::authored_entity::PropertyValueKind,
    buffer: &str,
    doc_props: &DocumentProperties,
) -> Result<crate::authored_entity::PropertyValue, String> {
    let parsed = parse_property_value(kind.clone(), buffer)?;
    Ok(match parsed {
        crate::authored_entity::PropertyValue::Scalar(value)
            if scalar_uses_display_units(field_name) =>
        {
            crate::authored_entity::PropertyValue::Scalar(doc_props.display_unit.to_metres(value))
        }
        crate::authored_entity::PropertyValue::Vec2(value) => {
            crate::authored_entity::PropertyValue::Vec2(Vec2::new(
                doc_props.display_unit.to_metres(value.x),
                doc_props.display_unit.to_metres(value.y),
            ))
        }
        crate::authored_entity::PropertyValue::Vec3(value) => {
            crate::authored_entity::PropertyValue::Vec3(Vec3::new(
                doc_props.display_unit.to_metres(value.x),
                doc_props.display_unit.to_metres(value.y),
                doc_props.display_unit.to_metres(value.z),
            ))
        }
        value => value,
    })
}

fn scalar_uses_display_units(field_name: &str) -> bool {
    matches!(
        field_name,
        name if name.contains("height")
            || name.contains("width")
            || name.contains("thickness")
            || name.contains("radius")
            || name.contains("length")
            || name.contains("elevation")
    )
}

#[cfg(feature = "perf-stats")]
fn draw_perf_overlay(ctx: &egui::Context, available: egui::Rect, perf_stats: &PerfStats) {
    if !perf_stats.visible {
        return;
    }

    egui::Area::new(egui::Id::new("perf_overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(
            available.max.x - PERF_OVERLAY_WIDTH - 10.0,
            available.min.y + 10.0,
        ))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(CHROME_BG)
                .stroke(egui::Stroke::new(1.0, CHROME_HOVER))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_min_width(PERF_OVERLAY_WIDTH);
                    ui.label(egui::RichText::new(perf_overlay_text(perf_stats)).monospace());
                });
        });
}

fn draw_toolbar_panel(
    ctx: &egui::Context,
    render: &mut ToolbarRenderContext<'_, '_, '_>,
    descriptor: &ToolbarDescriptor,
    dock: ToolbarDock,
) {
    let panel_id = format!("toolbar.{}.{}", dock.as_str(), descriptor.id);
    let render_contents = |ui: &mut egui::Ui| {
        let layout = if matches!(dock, ToolbarDock::Top | ToolbarDock::Bottom) {
            egui::Layout::left_to_right(egui::Align::Center)
        } else {
            egui::Layout::top_down(egui::Align::Center)
        };

        ui.with_layout(layout, |ui| {
            let grip = draw_toolbar_grip(ui, dock);
            if grip.drag_started() {
                render.drag_state.active = Some(ActiveToolbarDrag {
                    toolbar_id: descriptor.id.clone(),
                    original_dock: dock,
                    target_dock: Some(dock),
                });
            }

            for (section_index, section) in descriptor.sections.iter().enumerate() {
                let available_commands = section
                    .command_ids
                    .iter()
                    .filter_map(|command_id| render.command_registry.get(command_id))
                    .collect::<Vec<_>>();
                if available_commands.is_empty() {
                    continue;
                }
                if section_index > 0 {
                    ui.separator();
                }

                let section_layout = if matches!(dock, ToolbarDock::Top | ToolbarDock::Bottom) {
                    egui::Layout::left_to_right(egui::Align::Center)
                } else {
                    egui::Layout::top_down(egui::Align::Center)
                };
                ui.with_layout(section_layout, |ui| {
                    for command in available_commands {
                        let active = command.activates_tool.as_deref() == Some(render.current_tool);
                        let image = toolbar_image(render.contexts, render.icon_registry, command);
                        let mut button = if let Some(img) = image {
                            egui::Button::image(img)
                        } else {
                            egui::Button::new(toolbar_button_fallback_text(command))
                        };
                        button = button
                            .min_size(egui::vec2(TOOLBAR_BUTTON_SIZE, TOOLBAR_BUTTON_SIZE))
                            .fill(if active { CHROME_ACCENT } else { CHROME_BG });
                        let enabled = !command.requires_selection || render.selection_count > 0;
                        let response = ui.add_enabled(enabled, button);
                        // Workaround: egui's hovered()/clicked() flags are
                        // broken for toolbar buttons (contains_pointer=true
                        // but hovered=false). Detect hover and click manually
                        // from contains_pointer + raw pointer state.
                        let ptr_over = enabled && response.contains_pointer();
                        if ptr_over {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            egui::Tooltip::always_open(
                                ui.ctx().clone(),
                                response.layer_id,
                                response.id.with("tip"),
                                egui::PopupAnchor::Pointer,
                            )
                            .gap(12.0)
                            .show(|ui| {
                                ui.label(&command.label);
                            });
                        }
                        if ptr_over && ui.ctx().input(|i| i.pointer.primary_released()) {
                            queue_command_invocation_resource(
                                render.pending_command_invocations,
                                command.id.clone(),
                                serde_json::json!({}),
                            );
                        }
                    }
                });
            }
        });
    };

    let panel_response = match dock {
        ToolbarDock::Top => egui::TopBottomPanel::top(panel_id)
            .resizable(false)
            .show(ctx, render_contents),
        ToolbarDock::Bottom => egui::TopBottomPanel::bottom(panel_id)
            .resizable(false)
            .show(ctx, render_contents),
        ToolbarDock::Left => egui::SidePanel::left(panel_id)
            .resizable(false)
            .default_width(TOOLBAR_SIDE_WIDTH)
            .width_range(TOOLBAR_SIDE_WIDTH..=TOOLBAR_SIDE_WIDTH)
            .show(ctx, render_contents),
        ToolbarDock::Right => egui::SidePanel::right(panel_id)
            .resizable(false)
            .default_width(TOOLBAR_SIDE_WIDTH)
            .width_range(TOOLBAR_SIDE_WIDTH..=TOOLBAR_SIDE_WIDTH)
            .show(ctx, render_contents),
    };

    panel_response.response.context_menu(|ui| {
        draw_toolbar_visibility_menu(
            ui,
            render.toolbar_registry,
            render.toolbar_layout_state,
            render.doc_props,
        );
    });
}

fn draw_toolbar_visibility_menu(
    ui: &mut egui::Ui,
    toolbar_registry: &ToolbarRegistry,
    toolbar_layout_state: &mut ToolbarLayoutState,
    doc_props: &mut DocumentProperties,
) {
    ui.label("Toolbars");
    ui.separator();

    let mut descriptors = toolbar_registry.toolbars().collect::<Vec<_>>();
    descriptors.sort_by(|left, right| left.label.cmp(&right.label));
    for descriptor in descriptors {
        let visible = toolbar_layout_state
            .entries
            .get(&descriptor.id)
            .map(|entry| entry.visible)
            .unwrap_or(descriptor.id == "core");
        if descriptor.id == "core" {
            let mut always_visible = true;
            ui.add_enabled(
                false,
                egui::Checkbox::new(&mut always_visible, &descriptor.label),
            );
            continue;
        }

        let mut next_visible = visible;
        if ui.checkbox(&mut next_visible, &descriptor.label).changed() {
            set_toolbar_visibility(
                toolbar_layout_state,
                doc_props,
                &descriptor.id,
                next_visible,
            );
        }
    }
}

fn draw_toolbar_grip(ui: &mut egui::Ui, dock: ToolbarDock) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(TOOLBAR_GRIP_SIZE, TOOLBAR_GRIP_SIZE),
        egui::Sense::click_and_drag(),
    );
    if response.contains_pointer() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            response.layer_id,
            response.id.with("tip"),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            ui.label("Drag toolbar to another edge");
        });
    }

    let painter = ui.painter();
    let dot_color = if response.dragged() || response.contains_pointer() {
        CHROME_TEXT
    } else {
        CHROME_MUTED
    };
    let offsets: &[[egui::Vec2; 3]; 2] = if matches!(dock, ToolbarDock::Top | ToolbarDock::Bottom) {
        &[
            [
                egui::vec2(5.0, 5.0),
                egui::vec2(5.0, 9.0),
                egui::vec2(5.0, 13.0),
            ],
            [
                egui::vec2(10.0, 5.0),
                egui::vec2(10.0, 9.0),
                egui::vec2(10.0, 13.0),
            ],
        ]
    } else {
        &[
            [
                egui::vec2(5.0, 5.0),
                egui::vec2(9.0, 5.0),
                egui::vec2(13.0, 5.0),
            ],
            [
                egui::vec2(5.0, 10.0),
                egui::vec2(9.0, 10.0),
                egui::vec2(13.0, 10.0),
            ],
        ]
    };

    for group in offsets {
        for offset in group {
            let dot_rect = egui::Rect::from_center_size(
                rect.min + *offset,
                egui::vec2(TOOLBAR_GRIP_DOT_SIZE, TOOLBAR_GRIP_DOT_SIZE),
            );
            painter.rect_filled(dot_rect, 1.0, dot_color);
        }
    }

    response
}

fn ordered_toolbars_for_dock<'a>(
    registry: &'a ToolbarRegistry,
    layout_state: &'a ToolbarLayoutState,
    dock: ToolbarDock,
) -> Vec<&'a ToolbarDescriptor> {
    let mut descriptors = registry
        .toolbars()
        .filter_map(|descriptor| {
            let entry = layout_state.entries.get(&descriptor.id)?;
            if !entry.visible || entry.dock != dock {
                return None;
            }
            Some((entry.order, descriptor))
        })
        .collect::<Vec<_>>();
    descriptors.sort_by_key(|(order, descriptor)| (*order, descriptor.label.clone()));
    descriptors
        .into_iter()
        .map(|(_, descriptor)| descriptor)
        .collect()
}

fn toolbar_image<'a>(
    contexts: &'a mut EguiContexts,
    icon_registry: &IconRegistry,
    descriptor: &CommandDescriptor,
) -> Option<egui::Image<'a>> {
    let handle = descriptor
        .icon
        .as_deref()
        .and_then(|icon_id| icon_registry.get(icon_id))?;
    let texture_id = contexts
        .image_id(&handle)
        .unwrap_or_else(|| contexts.add_image(bevy_egui::EguiTextureHandle::Weak(handle.id())));
    Some(egui::Image::new((
        texture_id,
        egui::vec2(TOOLBAR_ICON_SIZE, TOOLBAR_ICON_SIZE),
    )))
}

fn toolbar_button_fallback_text(descriptor: &CommandDescriptor) -> String {
    descriptor
        .label
        .chars()
        .next()
        .map(|character| character.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn dock_target_for_position(ctx: &egui::Context, position: egui::Pos2) -> Option<ToolbarDock> {
    let screen = ctx.viewport_rect();
    [
        (
            ToolbarDock::Top,
            position.y - screen.top(),
            position.y <= screen.top() + DOCK_TARGET_SIZE,
        ),
        (
            ToolbarDock::Bottom,
            screen.bottom() - position.y,
            position.y >= screen.bottom() - DOCK_TARGET_SIZE,
        ),
        (
            ToolbarDock::Left,
            position.x - screen.left(),
            position.x <= screen.left() + DOCK_TARGET_SIZE,
        ),
        (
            ToolbarDock::Right,
            screen.right() - position.x,
            position.x >= screen.right() - DOCK_TARGET_SIZE,
        ),
    ]
    .into_iter()
    .filter(|(_, _, inside)| *inside)
    .min_by(|(_, left_distance, _), (_, right_distance, _)| left_distance.total_cmp(right_distance))
    .map(|(dock, _, _)| dock)
}

fn draw_dock_targets(ctx: &egui::Context, drag_state: &ToolbarDragState) {
    let Some(active_drag) = drag_state.active.as_ref() else {
        return;
    };

    let screen = ctx.viewport_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("toolbar_dock_targets"),
    ));
    for (dock, rect) in [
        (
            ToolbarDock::Top,
            egui::Rect::from_min_max(
                screen.min,
                egui::pos2(screen.max.x, screen.min.y + DOCK_TARGET_SIZE),
            ),
        ),
        (
            ToolbarDock::Bottom,
            egui::Rect::from_min_max(
                egui::pos2(screen.min.x, screen.max.y - DOCK_TARGET_SIZE),
                screen.max,
            ),
        ),
        (
            ToolbarDock::Left,
            egui::Rect::from_min_max(
                screen.min,
                egui::pos2(screen.min.x + DOCK_TARGET_SIZE, screen.max.y),
            ),
        ),
        (
            ToolbarDock::Right,
            egui::Rect::from_min_max(
                egui::pos2(screen.max.x - DOCK_TARGET_SIZE, screen.min.y),
                screen.max,
            ),
        ),
    ] {
        let color = if active_drag.target_dock == Some(dock) {
            CHROME_ACCENT
        } else {
            CHROME_HOVER
        };
        painter.rect_filled(rect, 0.0, color.gamma_multiply(0.5));
    }
}

fn complete_toolbar_drag(
    ctx: &egui::Context,
    drag_state: &mut ToolbarDragState,
    toolbar_layout_state: &mut ToolbarLayoutState,
    doc_props: &mut DocumentProperties,
) {
    let Some(active_drag) = drag_state.active.as_ref() else {
        return;
    };
    if ctx.input(|input| input.pointer.primary_down()) {
        return;
    }

    let toolbar_id = active_drag.toolbar_id.clone();
    let original_dock = active_drag.original_dock;
    let target_dock = active_drag.target_dock;
    drag_state.active = None;

    let Some(target_dock) = target_dock else {
        return;
    };
    if target_dock == original_dock {
        return;
    }

    apply_toolbar_layout_change(toolbar_layout_state, doc_props, &toolbar_id, target_dock);
}

fn apply_chrome_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = CHROME_BG;
    visuals.panel_fill = CHROME_BG;
    visuals.widgets.noninteractive.fg_stroke.color = CHROME_TEXT;
    visuals.widgets.inactive.bg_fill = CHROME_BG;
    visuals.widgets.inactive.fg_stroke.color = CHROME_TEXT;
    visuals.widgets.hovered.bg_fill = CHROME_HOVER;
    visuals.widgets.hovered.fg_stroke.color = CHROME_TEXT;
    visuals.widgets.active.bg_fill = CHROME_ACCENT;
    visuals.widgets.active.fg_stroke.color = CHROME_TEXT;
    visuals.widgets.open.bg_fill = CHROME_ACCENT;
    visuals.override_text_color = Some(CHROME_TEXT);
    visuals.selection.bg_fill = CHROME_ACCENT;
    visuals.selection.stroke.color = CHROME_TEXT;
    visuals.widgets.noninteractive.bg_stroke.color = CHROME_MUTED;
    ctx.set_visuals(visuals);
}
