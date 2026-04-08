use std::collections::HashMap;

use bevy::{ecs::system::SystemParam, prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPlugin};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{overlay_text as perf_overlay_text, PerfStats};
use crate::plugins::{
    camera::{
        focal_length_range_mm, CameraControlsState, CameraProjectionMode, CameraViewPreset,
        CAMERA_TOOLBAR_ID,
    },
    command_registry::{
        ordered_menu_categories, queue_command_invocation_resource, CommandDescriptor,
        CommandRegistry, IconRegistry, PendingCommandInvocations,
    },
    commands::ApplyEntityChangesCommand,
    cursor::{CursorWorldPos, ViewportUiInset},
    definition_browser::{
        draw_definitions_window, sync_definition_selection_context, DefinitionSelectionContext,
        DefinitionsWindowState,
    },
    document_properties::DocumentProperties,
    import::{
        apply_import_review_to_pending, DocumentImportPlacementState, ImportOriginMode,
        ImportProgressState, ImportReviewState, ImportedLayerPanelState, PendingImportCommit,
    },
    material_browser::{draw_materials_window, MaterialsWindowState},
    materials::MaterialRegistry,
    menu_bar::MenuBarState,
    modeling::definition::{DefinitionLibraryRegistry, DefinitionRegistry},
    palette::{draw_command_palette, PaletteState},
    property_edit::{
        parse_property_value, shared_property_value, PropertyEditState, PropertyPanelData,
        PropertyPanelState,
    },
    selection::Selected,
    toolbar::{
        apply_toolbar_float, redock_toolbar, set_toolbar_visibility, FloatingToolbarStates,
        RedockTarget, ToolbarDescriptor, ToolbarDock, ToolbarLayoutState, ToolbarRegistry,
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
const CAMERA_LENS_SLIDER_WIDTH: f32 = 140.0;
const CAMERA_VERTICAL_SLIDER_HEIGHT: f32 = 120.0;
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
        .init_resource::<ViewportContextMenu>()
        .init_resource::<MaterialsWindowState>()
        .add_systems(
            Update,
            (
                sync_definition_selection_context,
                draw_egui_chrome.in_set(EguiChromeSystems),
            )
                .chain(),
        );
    }
}

#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct EguiWantsInput {
    pub pointer: bool,
    pub keyboard: bool,
}

#[derive(Resource, Default, Debug, Clone)]
struct ViewportContextMenu {
    open: bool,
    position: egui::Pos2,
}

/// Describes exactly where a dragged toolbar should land.
#[derive(Debug, Clone, PartialEq)]
struct InsertionTarget {
    dock: ToolbarDock,
    /// Row/column band index within the dock.
    row: u32,
    /// When true a new band is created at `row`, shifting existing bands down.
    new_row: bool,
    /// ID of the toolbar to insert after; `None` means prepend to the row.
    insert_after: Option<String>,
}

#[derive(Resource, Default, Debug, Clone)]
struct ToolbarDragState {
    active: Option<ActiveToolbarDrag>,
    /// Toolbar frame rects captured during the current frame's render pass.
    /// Cleared at the start of every frame in `draw_egui_chrome`.
    toolbar_rects: HashMap<String, egui::Rect>,
}

#[derive(Debug, Clone)]
struct ActiveToolbarDrag {
    toolbar_id: String,
    original_dock: ToolbarDock,
    target: Option<InsertionTarget>,
}

struct ToolbarRenderContext<'a, 'w, 's> {
    contexts: &'a mut EguiContexts<'w, 's>,
    camera_controls: &'a mut CameraControlsState,
    command_registry: &'a CommandRegistry,
    icon_registry: &'a IconRegistry,
    toolbar_registry: &'a ToolbarRegistry,
    toolbar_layout_state: &'a mut ToolbarLayoutState,
    floating_states: &'a mut FloatingToolbarStates,
    doc_props: &'a mut DocumentProperties,
    pending_command_invocations: &'a mut PendingCommandInvocations,
    drag_state: &'a mut ToolbarDragState,
    selection_count: usize,
    current_tool: &'a str,
}

#[derive(SystemParam)]
struct ChromeData<'w, 's> {
    camera_controls: ResMut<'w, CameraControlsState>,
    command_registry: Res<'w, CommandRegistry>,
    icon_registry: Res<'w, IconRegistry>,
    toolbar_registry: Res<'w, ToolbarRegistry>,
    toolbar_layout_state: ResMut<'w, ToolbarLayoutState>,
    floating_states: ResMut<'w, FloatingToolbarStates>,
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
    definitions_window_state: ResMut<'w, DefinitionsWindowState>,
    definition_selection_context: Res<'w, DefinitionSelectionContext>,
    materials_window_state: ResMut<'w, MaterialsWindowState>,
    material_registry: ResMut<'w, MaterialRegistry>,
    definition_registry: Res<'w, DefinitionRegistry>,
    definition_library_registry: Res<'w, DefinitionLibraryRegistry>,
    definition_draft_registry:
        ResMut<'w, crate::plugins::definition_authoring::DefinitionDraftRegistry>,
    active_tool: Res<'w, State<ActiveTool>>,
    selected_query: Query<'w, 's, (), With<Selected>>,
    property_edit_state: ResMut<'w, PropertyEditState>,
    property_panel_state: ResMut<'w, PropertyPanelState>,
    property_panel_data: Res<'w, PropertyPanelData>,
    palette_state: ResMut<'w, PaletteState>,
    transform_state: Res<'w, TransformState>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    window_query: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    menu_bar_state: ResMut<'w, MenuBarState>,
    viewport_ui_inset: ResMut<'w, ViewportUiInset>,
    egui_wants_input: ResMut<'w, EguiWantsInput>,
    drag_state: ResMut<'w, ToolbarDragState>,
    viewport_context_menu: ResMut<'w, ViewportContextMenu>,
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
                            if category.label() == "View" {
                                ui.separator();
                                ui.menu_button("Toolbars", |ui| {
                                    draw_toolbar_visibility_menu(
                                        ui,
                                        &data.toolbar_registry,
                                        &mut data.toolbar_layout_state,
                                        &mut data.doc_props,
                                    );
                                });
                            }
                        });
                    }
                });
            });
    }

    // Clear stale frame rects before the toolbar render pass populates them.
    data.drag_state.toolbar_rects.clear();

    // Collect all toolbar rows up-front (owned) so the immutable borrows on
    // `toolbar_registry` and `toolbar_layout_state` are released before the
    // render loop takes a mutable borrow on `toolbar_layout_state`.
    let all_rows: Vec<(ToolbarDock, Vec<Vec<ToolbarDescriptor>>)> = [
        ToolbarDock::Top,
        ToolbarDock::Left,
        ToolbarDock::Right,
        ToolbarDock::Bottom,
    ]
    .iter()
    .map(|&dock| {
        let rows = toolbars_by_row(&data.toolbar_registry, &data.toolbar_layout_state, dock)
            .into_iter()
            .map(|row| row.into_iter().cloned().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        (dock, rows)
    })
    .collect();

    for (dock, rows) in all_rows {
        for (row_idx, row_descriptors) in rows.iter().enumerate() {
            let descriptor_refs: Vec<&ToolbarDescriptor> = row_descriptors.iter().collect();
            let mut render = ToolbarRenderContext {
                contexts: &mut contexts,
                camera_controls: &mut data.camera_controls,
                command_registry: &data.command_registry,
                icon_registry: &data.icon_registry,
                toolbar_registry: &data.toolbar_registry,
                toolbar_layout_state: &mut data.toolbar_layout_state,
                floating_states: &mut data.floating_states,
                doc_props: &mut data.doc_props,
                pending_command_invocations: &mut data.pending_command_invocations,
                drag_state: &mut data.drag_state,
                selection_count,
                current_tool: &current_tool,
            };
            draw_toolbar_row(&ctx, &mut render, &descriptor_refs, dock, row_idx as u32);
        }
    }

    // Render floating toolbars as egui::Window instances.
    {
        let floating_descriptors: Vec<ToolbarDescriptor> = data
            .toolbar_registry
            .toolbars()
            .filter(|d| {
                data.toolbar_layout_state
                    .entries
                    .get(&d.id)
                    .map(|e| e.dock == ToolbarDock::Floating && e.visible)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        let mut render = ToolbarRenderContext {
            contexts: &mut contexts,
            camera_controls: &mut data.camera_controls,
            command_registry: &data.command_registry,
            icon_registry: &data.icon_registry,
            toolbar_registry: &data.toolbar_registry,
            toolbar_layout_state: &mut data.toolbar_layout_state,
            floating_states: &mut data.floating_states,
            doc_props: &mut data.doc_props,
            pending_command_invocations: &mut data.pending_command_invocations,
            drag_state: &mut data.drag_state,
            selection_count,
            current_tool: &current_tool,
        };

        for descriptor in &floating_descriptors {
            draw_floating_toolbar(&ctx, &mut render, descriptor);
        }
    }

    draw_command_palette(
        &ctx,
        &mut data.palette_state,
        &data.command_registry,
        &mut data.pending_command_invocations,
        &mut data.status_bar_data,
        selection_count,
    );
    draw_property_panel(&ctx, &mut data);
    draw_definitions_window(
        &ctx,
        &mut data.definitions_window_state,
        &data.definition_selection_context,
        &data.definition_registry,
        &data.definition_library_registry,
        &mut data.definition_draft_registry,
        &mut data.pending_command_invocations,
        &data.cursor_world_pos,
        &mut data.status_bar_data,
    );
    draw_materials_window(
        &ctx,
        &mut data.materials_window_state,
        &mut data.material_registry,
        &mut data.pending_command_invocations,
    );
    draw_import_review_window(&ctx, &mut data);
    draw_imported_layers_window(&ctx, &mut data);
    draw_import_progress_window(&ctx, &data);

    // Compute the insertion target before rendering indicators, using a split
    // borrow to avoid a conflict between the immutable `drag_state` read inside
    // `compute_insertion_target` and the mutable write below.
    let new_target = if let Some(active_drag) = data.drag_state.active.as_ref() {
        let dragging_id = active_drag.toolbar_id.clone();
        ctx.pointer_hover_pos().and_then(|cursor| {
            compute_insertion_target(
                &ctx,
                cursor,
                &dragging_id,
                &data.drag_state,
                &data.toolbar_layout_state,
                &data.toolbar_registry,
            )
        })
    } else {
        None
    };
    if let Some(active_drag) = data.drag_state.active.as_mut() {
        active_drag.target = new_target;
    }

    draw_dock_targets(
        &ctx,
        &data.drag_state,
        &data.toolbar_registry,
        &data.toolbar_layout_state,
    );
    draw_drag_ghost(&ctx, &data.drag_state, &data.toolbar_registry);
    complete_toolbar_drag(
        &ctx,
        &mut data.drag_state,
        &mut data.toolbar_layout_state,
        &mut data.floating_states,
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

    // Viewport right-click context menu.
    // Detect right-click release without drag, outside any egui area.
    let right_released_in_viewport = ctx.input(|i| {
        i.pointer.button_released(egui::PointerButton::Secondary)
            && !i.pointer.is_decidedly_dragging()
    }) && !data.egui_wants_input.pointer
        && !crate::plugins::camera::orbit_modifier_pressed(&data.keys);
    if right_released_in_viewport {
        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
            data.viewport_context_menu.open = true;
            data.viewport_context_menu.position = pos;
        }
    }
    draw_viewport_context_menu(
        &ctx,
        &mut data.viewport_context_menu,
        selection_count,
        &mut data.materials_window_state,
        &mut data.pending_command_invocations,
    );

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

/// Returns the content size that the ghost and landing slot share.
///
/// This is the inner content area (grip + button placeholders) before any
/// Frame margin/stroke is applied. Both `draw_drag_ghost` and `draw_landing_slot`
/// use this so the two shapes match visually.
fn ghost_content_size(button_count: usize, horizontal: bool) -> egui::Vec2 {
    let cap = if horizontal { 12 } else { 8 };
    let buttons = button_count.min(cap) as f32 * TOOLBAR_BUTTON_SIZE;
    // Add the ghost Frame's own overhead (inner_margin=4 each side → 8, stroke=2 each side → 4).
    let frame_overhead = 12.0;
    if horizontal {
        egui::vec2(
            TOOLBAR_GRIP_SIZE + buttons + frame_overhead,
            TOOLBAR_BUTTON_SIZE,
        )
    } else {
        egui::vec2(
            TOOLBAR_BUTTON_SIZE,
            TOOLBAR_GRIP_SIZE + buttons + frame_overhead,
        )
    }
}

fn camera_toolbar_content_size(horizontal: bool) -> egui::Vec2 {
    if horizontal {
        egui::vec2(420.0, TOOLBAR_BUTTON_SIZE)
    } else {
        egui::vec2(
            TOOLBAR_BUTTON_SIZE,
            TOOLBAR_GRIP_SIZE + 6.0 * TOOLBAR_BUTTON_SIZE + CAMERA_VERTICAL_SLIDER_HEIGHT + 32.0,
        )
    }
}

fn toolbar_content_size(descriptor: &ToolbarDescriptor, horizontal: bool) -> egui::Vec2 {
    if descriptor.id == CAMERA_TOOLBAR_ID {
        camera_toolbar_content_size(horizontal)
    } else {
        let btn = descriptor
            .sections
            .iter()
            .map(|section| section.command_ids.len())
            .sum::<usize>();
        ghost_content_size(btn, horizontal)
    }
}

/// Draws a ghost-sized landing slot at the current position in `ui`.
///
/// The slot has the same dimensions as the drag ghost so the user can see
/// exactly where and how big the toolbar will appear when dropped.
fn draw_landing_slot(ui: &mut egui::Ui, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(4),
        CHROME_ACCENT.gamma_multiply(0.12),
    );
    // Draw a 2 px accent border using four line segments (avoids rect_stroke API variance).
    let r = rect.shrink(1.0);
    let s = egui::Stroke::new(2.0, CHROME_ACCENT);
    ui.painter().line_segment([r.left_top(), r.right_top()], s);
    ui.painter()
        .line_segment([r.right_top(), r.right_bottom()], s);
    ui.painter()
        .line_segment([r.right_bottom(), r.left_bottom()], s);
    ui.painter()
        .line_segment([r.left_bottom(), r.left_top()], s);
}

/// Draws all toolbars in a single row/column band as one egui panel.
///
/// Each toolbar within the row is rendered as a visually bounded `Frame` box.
/// For Top/Bottom docks the toolbars are laid out left-to-right; for Left/Right
/// they are stacked top-to-bottom.
///
/// When a drag targets this row the function inserts a ghost-sized landing slot
/// at the insertion point and omits the dragged toolbar from its original
/// position (replacing it with the slot for same-row reorders, or a dimmed
/// placeholder for cross-row drags).
fn draw_toolbar_row(
    ctx: &egui::Context,
    render: &mut ToolbarRenderContext<'_, '_, '_>,
    descriptors: &[&ToolbarDescriptor],
    dock: ToolbarDock,
    row_idx: u32,
) {
    let panel_id = format!("toolbar_row.{}.{}", dock.as_str(), row_idx);
    let horizontal = matches!(dock, ToolbarDock::Top | ToolbarDock::Bottom);

    // Pre-extract everything we need from `render` before the closure captures it,
    // so the closure only needs one mutable borrow of `render` at a time.
    let dragged_id: Option<String> = render
        .drag_state
        .active
        .as_ref()
        .map(|d| d.toolbar_id.clone());
    let insertion_target: Option<InsertionTarget> = render
        .drag_state
        .active
        .as_ref()
        .and_then(|d| d.target.clone());

    let is_target_row = insertion_target
        .as_ref()
        .map(|t| !t.new_row && t.dock == dock && t.row == row_idx)
        .unwrap_or(false);

    // Compute the landing slot size once (matches ghost dimensions).
    let slot_size: Option<egui::Vec2> = if is_target_row {
        dragged_id
            .as_deref()
            .and_then(|id| render.toolbar_registry.toolbars().find(|d| d.id == id))
            .map(|desc| {
                let slot_horizontal = insertion_target
                    .as_ref()
                    .map(|t| matches!(t.dock, ToolbarDock::Top | ToolbarDock::Bottom))
                    .unwrap_or(horizontal);
                toolbar_content_size(desc, slot_horizontal)
            })
    } else {
        None
    };

    let insert_after_id: Option<String> = insertion_target
        .as_ref()
        .and_then(|t| t.insert_after.clone());

    let render_row = |ui: &mut egui::Ui| {
        let row_layout = if horizontal {
            egui::Layout::left_to_right(egui::Align::Center)
        } else {
            egui::Layout::top_down(egui::Align::Center)
        };

        ui.with_layout(row_layout, |ui| {
            // Counts items placed so far; used to decide when to draw separators.
            let mut placed = 0usize;

            // Prepend slot (insert_after == None means "before the first toolbar").
            if is_target_row && insert_after_id.is_none() {
                if let Some(size) = slot_size {
                    draw_landing_slot(ui, size);
                    placed += 1;
                }
            }

            for descriptor in descriptors {
                let is_this_dragged = dragged_id.as_deref() == Some(descriptor.id.as_str());
                // For same-row reorders the slot replaces the toolbar entirely.
                if is_this_dragged && is_target_row {
                    continue;
                }

                if placed > 0 {
                    ui.separator();
                }

                let frame_stroke = if is_this_dragged {
                    // Cross-row drag: faint placeholder in the source row.
                    egui::Stroke::new(1.0, CHROME_MUTED.gamma_multiply(0.3))
                } else {
                    egui::Stroke::new(1.0, CHROME_HOVER)
                };

                let frame_response = egui::Frame::new()
                    .stroke(frame_stroke)
                    .corner_radius(egui::CornerRadius::same(3))
                    .inner_margin(egui::Margin::same(2))
                    .show(ui, |ui| {
                        draw_toolbar_content(ui, render, descriptor, dock);
                    });
                // Record rect for insertion-target computation next frame.
                render
                    .drag_state
                    .toolbar_rects
                    .insert(descriptor.id.clone(), frame_response.response.rect);
                frame_response.response.context_menu(|ui| {
                    draw_toolbar_visibility_menu(
                        ui,
                        render.toolbar_registry,
                        render.toolbar_layout_state,
                        render.doc_props,
                    );
                });
                placed += 1;

                // Append/insert slot after this toolbar if it is the insert_after target.
                if is_target_row && insert_after_id.as_deref() == Some(descriptor.id.as_str()) {
                    if let Some(size) = slot_size {
                        ui.separator();
                        draw_landing_slot(ui, size);
                        placed += 1;
                    }
                }
            }
        });
    };

    match dock {
        ToolbarDock::Top => {
            egui::TopBottomPanel::top(panel_id)
                .resizable(false)
                .show(ctx, render_row);
        }
        ToolbarDock::Bottom => {
            egui::TopBottomPanel::bottom(panel_id)
                .resizable(false)
                .show(ctx, render_row);
        }
        ToolbarDock::Left => {
            egui::SidePanel::left(panel_id)
                .resizable(false)
                .default_width(TOOLBAR_SIDE_WIDTH)
                .width_range(TOOLBAR_SIDE_WIDTH..=TOOLBAR_SIDE_WIDTH)
                .show(ctx, render_row);
        }
        ToolbarDock::Right => {
            egui::SidePanel::right(panel_id)
                .resizable(false)
                .default_width(TOOLBAR_SIDE_WIDTH)
                .width_range(TOOLBAR_SIDE_WIDTH..=TOOLBAR_SIDE_WIDTH)
                .show(ctx, render_row);
        }
        ToolbarDock::Floating => {
            // Floating toolbars are rendered by draw_floating_toolbar, not here.
        }
    }
}

/// Draws a single toolbar's grip and buttons into the given `ui`.
///
/// Does not create any panel — intended to be called within `draw_toolbar_row`.
fn draw_toolbar_content(
    ui: &mut egui::Ui,
    render: &mut ToolbarRenderContext<'_, '_, '_>,
    descriptor: &ToolbarDescriptor,
    dock: ToolbarDock,
) {
    let horizontal = matches!(dock, ToolbarDock::Top | ToolbarDock::Bottom);
    let is_dragging = render
        .drag_state
        .active
        .as_ref()
        .map(|d| d.toolbar_id == descriptor.id)
        .unwrap_or(false);

    if is_dragging {
        let size = toolbar_content_size(descriptor, horizontal);
        if horizontal {
            ui.allocate_space(size);
        } else {
            ui.allocate_space(egui::vec2(TOOLBAR_SIDE_WIDTH - 8.0, size.y));
        }
        return;
    }

    if descriptor.id == CAMERA_TOOLBAR_ID {
        draw_camera_toolbar_content(ui, render, dock, horizontal, descriptor);
        return;
    }

    let content_layout = if horizontal {
        egui::Layout::left_to_right(egui::Align::Center)
    } else {
        egui::Layout::top_down(egui::Align::Center)
    };

    ui.with_layout(content_layout, |ui| {
        let grip = draw_toolbar_grip(ui, dock);
        if grip.drag_started() {
            render.drag_state.active = Some(ActiveToolbarDrag {
                toolbar_id: descriptor.id.clone(),
                original_dock: dock,
                target: None,
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
}

fn draw_camera_toolbar_content(
    ui: &mut egui::Ui,
    render: &mut ToolbarRenderContext<'_, '_, '_>,
    dock: ToolbarDock,
    horizontal: bool,
    descriptor: &ToolbarDescriptor,
) {
    let content_layout = if horizontal {
        egui::Layout::left_to_right(egui::Align::Center)
    } else {
        egui::Layout::top_down(egui::Align::Center)
    };

    ui.with_layout(content_layout, |ui| {
        let grip = draw_toolbar_grip(ui, dock);
        if grip.drag_started() {
            render.drag_state.active = Some(ActiveToolbarDrag {
                toolbar_id: descriptor.id.clone(),
                original_dock: dock,
                target: None,
            });
        }

        draw_camera_toolbar_controls(ui, &mut *render.camera_controls, horizontal);
    });
}

fn draw_camera_toolbar_controls(
    ui: &mut egui::Ui,
    controls: &mut CameraControlsState,
    horizontal: bool,
) {
    let focal_range = focal_length_range_mm();

    if horizontal {
        ui.separator();
        ui.horizontal(|ui| {
            projection_mode_button(
                ui,
                controls,
                CameraProjectionMode::Perspective,
                "Perspective",
                egui::vec2(96.0, TOOLBAR_BUTTON_SIZE),
            );
            if projection_mode_button(
                ui,
                controls,
                CameraProjectionMode::Isometric,
                "Isometric",
                egui::vec2(88.0, TOOLBAR_BUTTON_SIZE),
            ) {
                controls.pending_view_preset = Some(CameraViewPreset::Isometric);
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            for (label, preset) in [
                ("Top", CameraViewPreset::Top),
                ("Left", CameraViewPreset::Left),
                ("Right", CameraViewPreset::Right),
                ("Bottom", CameraViewPreset::Bottom),
            ] {
                if ui
                    .add_sized(
                        egui::vec2(TOOLBAR_BUTTON_SIZE + 8.0, TOOLBAR_BUTTON_SIZE),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    controls.pending_view_preset = Some(preset);
                }
            }
        });

        ui.separator();
        ui.label(format!("Lens {:.0} mm", controls.focal_length_mm));
        let slider = egui::Slider::new(&mut controls.focal_length_mm, focal_range)
            .show_value(false)
            .clamping(egui::SliderClamping::Always);
        ui.add_enabled_ui(
            controls.projection_mode == CameraProjectionMode::Perspective,
            |ui| {
                ui.add_sized(egui::vec2(CAMERA_LENS_SLIDER_WIDTH, 0.0), slider);
            },
        );
    } else {
        ui.separator();
        for (label, mode) in [
            ("P", CameraProjectionMode::Perspective),
            ("I", CameraProjectionMode::Isometric),
        ] {
            let clicked = projection_mode_button(
                ui,
                controls,
                mode,
                label,
                egui::vec2(TOOLBAR_BUTTON_SIZE, TOOLBAR_BUTTON_SIZE),
            );
            if clicked && mode == CameraProjectionMode::Isometric {
                controls.pending_view_preset = Some(CameraViewPreset::Isometric);
            }
        }

        ui.separator();
        for (label, preset) in [
            ("T", CameraViewPreset::Top),
            ("L", CameraViewPreset::Left),
            ("R", CameraViewPreset::Right),
            ("B", CameraViewPreset::Bottom),
        ] {
            if ui
                .add_sized(
                    egui::vec2(TOOLBAR_BUTTON_SIZE, TOOLBAR_BUTTON_SIZE),
                    egui::Button::new(label),
                )
                .clicked()
            {
                controls.pending_view_preset = Some(preset);
            }
        }

        ui.separator();
        ui.label(format!("{:.0} mm", controls.focal_length_mm));
        let slider = egui::Slider::new(&mut controls.focal_length_mm, focal_range)
            .vertical()
            .show_value(false)
            .clamping(egui::SliderClamping::Always);
        ui.add_enabled_ui(
            controls.projection_mode == CameraProjectionMode::Perspective,
            |ui| {
                ui.add_sized(
                    egui::vec2(TOOLBAR_BUTTON_SIZE, CAMERA_VERTICAL_SLIDER_HEIGHT),
                    slider,
                );
            },
        );
    }
}

fn projection_mode_button(
    ui: &mut egui::Ui,
    controls: &mut CameraControlsState,
    mode: CameraProjectionMode,
    label: &str,
    size: egui::Vec2,
) -> bool {
    let selected = controls.projection_mode == mode;
    let response = ui.add_sized(
        size,
        egui::Button::new(label).fill(if selected { CHROME_ACCENT } else { CHROME_BG }),
    );
    if response.clicked() {
        controls.projection_mode = mode;
        return true;
    }
    false
}

fn draw_floating_toolbar(
    ctx: &egui::Context,
    render: &mut ToolbarRenderContext<'_, '_, '_>,
    descriptor: &ToolbarDescriptor,
) {
    let is_dragging = render
        .drag_state
        .active
        .as_ref()
        .map(|d| d.toolbar_id == descriptor.id)
        .unwrap_or(false);
    if is_dragging {
        return;
    }

    let floating_entry = render.floating_states.entries.get(&descriptor.id).cloned();
    let minimized = floating_entry
        .as_ref()
        .map(|e| e.minimized)
        .unwrap_or(false);
    let pos = floating_entry
        .as_ref()
        .map(|e| egui::pos2(e.position[0], e.position[1]))
        .unwrap_or_else(|| egui::pos2(100.0, 100.0));

    let window_id = egui::Id::new(format!("floating_toolbar_{}", descriptor.id));

    egui::Window::new(&descriptor.label)
        .id(window_id)
        .current_pos(pos)
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .movable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let grip = draw_toolbar_grip(ui, ToolbarDock::Floating);

                if grip.double_clicked() {
                    if let Some(entry) = render.floating_states.entries.get_mut(&descriptor.id) {
                        entry.minimized = !entry.minimized;
                    }
                }

                // Grip drag: move the floating window, or initiate re-dock.
                if grip.dragged() {
                    let delta = grip.drag_delta();
                    if let Some(entry) = render.floating_states.entries.get_mut(&descriptor.id) {
                        entry.position[0] += delta.x;
                        entry.position[1] += delta.y;
                    }
                    // Check if we're near a dock edge — if so, start a re-dock drag.
                    if let Some(cursor) = ctx.pointer_hover_pos() {
                        if dock_target_for_position(ctx, cursor).is_some()
                            && render.drag_state.active.is_none()
                        {
                            render.drag_state.active = Some(ActiveToolbarDrag {
                                toolbar_id: descriptor.id.clone(),
                                original_dock: ToolbarDock::Floating,
                                target: None,
                            });
                        }
                    }
                }

                if minimized {
                    ui.label(
                        egui::RichText::new(&descriptor.label)
                            .color(CHROME_MUTED)
                            .small(),
                    );
                } else if descriptor.id == CAMERA_TOOLBAR_ID {
                    draw_camera_toolbar_controls(ui, &mut *render.camera_controls, true);
                } else {
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
                        for command in available_commands {
                            let active =
                                command.activates_tool.as_deref() == Some(render.current_tool);
                            let image =
                                toolbar_image(render.contexts, render.icon_registry, command);
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
                    }
                }
            });
        });
}

fn draw_viewport_context_menu(
    ctx: &egui::Context,
    menu: &mut ViewportContextMenu,
    selection_count: usize,
    materials_window_state: &mut MaterialsWindowState,
    pending: &mut PendingCommandInvocations,
) {
    if !menu.open {
        return;
    }

    let has_selection = selection_count > 0;

    let area_response = egui::Area::new(egui::Id::new("viewport_ctx_menu"))
        .fixed_pos(menu.position)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(160.0);

                macro_rules! item {
                    ($label:expr, $id:expr) => {{
                        if ui.button($label).clicked() {
                            queue_command_invocation_resource(
                                pending,
                                $id.to_string(),
                                serde_json::json!({}),
                            );
                            menu.open = false;
                        }
                    }};
                    (enabled: $enabled:expr, $label:expr, $id:expr) => {{
                        if ui
                            .add_enabled($enabled, egui::Button::new($label))
                            .clicked()
                        {
                            queue_command_invocation_resource(
                                pending,
                                $id.to_string(),
                                serde_json::json!({}),
                            );
                            menu.open = false;
                        }
                    }};
                }

                if has_selection {
                    item!("Move    G", "modeling.move");
                    item!("Rotate    R", "modeling.rotate");
                    item!("Scale    S", "modeling.scale");
                    ui.separator();
                    if ui.button("Materials…    Cmd+\u{21e7}M").clicked() {
                        materials_window_state.visible = true;
                        menu.open = false;
                    }
                    ui.separator();
                    item!(
                        "Zoom to Selection    \u{21e7}Home",
                        "core.zoom_to_selection"
                    );
                    ui.separator();
                    item!("Group    Cmd+G", "modeling.group");
                    item!("Ungroup    Cmd+\u{21e7}G", "modeling.ungroup");
                    ui.separator();
                    item!("Deselect    Esc", "core.deselect");
                    item!("Delete    Delete", "core.delete");
                } else {
                    item!("Select All    Cmd+A", "core.select_all");
                    ui.separator();
                    item!("Zoom to Extents    Home", "core.zoom_to_extents");
                }
            });
        });

    // Close on click outside or Escape.
    let clicked_outside =
        ctx.input(|i| i.pointer.any_click()) && !area_response.response.contains_pointer();
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if clicked_outside || escape {
        menu.open = false;
    }
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
        let tip = if dock == ToolbarDock::Floating {
            "Drag to dock \u{2022} Double-click to minimize"
        } else {
            "Drag toolbar to another edge"
        };
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            response.layer_id,
            response.id.with("tip"),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            ui.label(tip);
        });
    }

    let painter = ui.painter();
    let dot_color = if response.dragged() || response.contains_pointer() {
        CHROME_TEXT
    } else {
        CHROME_MUTED
    };
    let offsets: &[[egui::Vec2; 3]; 2] = if matches!(
        dock,
        ToolbarDock::Top | ToolbarDock::Bottom | ToolbarDock::Floating
    ) {
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

/// Returns toolbars for `dock` grouped into rows.
///
/// The outer `Vec` is indexed by row (row 0 first). Each inner `Vec` is the
/// list of toolbar descriptors within that row, sorted by `order`.
fn toolbars_by_row<'a>(
    registry: &'a ToolbarRegistry,
    layout_state: &'a ToolbarLayoutState,
    dock: ToolbarDock,
) -> Vec<Vec<&'a ToolbarDescriptor>> {
    // Collect (row, order, descriptor) for all visible toolbars in this dock.
    let mut entries: Vec<(u32, u32, &'a ToolbarDescriptor)> = registry
        .toolbars()
        .filter_map(|descriptor| {
            let entry = layout_state.entries.get(&descriptor.id)?;
            if !entry.visible || entry.dock != dock {
                return None;
            }
            Some((entry.row, entry.order, descriptor))
        })
        .collect();

    if entries.is_empty() {
        return Vec::new();
    }

    // Sort by row then order then label for stable ordering.
    entries.sort_by_key(|(row, order, descriptor)| (*row, *order, descriptor.label.clone()));

    // Determine the max row index so we can build a dense Vec.
    let max_row = entries.iter().map(|(row, _, _)| *row).max().unwrap_or(0);
    let mut rows: Vec<Vec<&'a ToolbarDescriptor>> = (0..=max_row).map(|_| Vec::new()).collect();

    for (row, _, descriptor) in entries {
        rows[row as usize].push(descriptor);
    }

    // Drop empty rows that may have been created in the middle of a sparse range.
    rows.retain(|row| !row.is_empty());
    rows
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

/// Computes the precise insertion target for a toolbar being dragged.
///
/// Returns `None` when the cursor is not within any dock's hot zone.
fn compute_insertion_target(
    ctx: &egui::Context,
    cursor: egui::Pos2,
    dragging_id: &str,
    drag_state: &ToolbarDragState,
    layout_state: &ToolbarLayoutState,
    registry: &ToolbarRegistry,
) -> Option<InsertionTarget> {
    // Determine the target dock.
    //
    // Naive edge-proximity (dock_target_for_position) fails in the top-left / top-right
    // corners: the cursor can be within DOCK_TARGET_SIZE of both the Top and Left edges,
    // and Left "wins" because X < Y at those positions — even though the user is clearly
    // hovering within the Top toolbar row (same Y band).
    //
    // Fix: first check whether the cursor falls inside any existing dock's "full-span
    // strip" (the union of that dock's toolbar rects extended to the full screen width or
    // height). Toolbar bands are narrow and specific, so containment is an unambiguous
    // signal of intent. Only fall back to edge proximity when no band matches (e.g. the
    // cursor is in empty space between the toolbar and the screen edge, or the dock is
    // empty).
    let screen = ctx.viewport_rect();
    let dock = {
        // Check docks in priority order (Top/Bottom before Left/Right so horizontal bands
        // win over vertical bands in corner overlap situations).
        let band_dock = [
            ToolbarDock::Top,
            ToolbarDock::Bottom,
            ToolbarDock::Left,
            ToolbarDock::Right,
        ]
        .into_iter()
        .find_map(|candidate| {
            let horiz = matches!(candidate, ToolbarDock::Top | ToolbarDock::Bottom);
            // Union of all non-dragged toolbar rects in this dock.
            let band: egui::Rect = drag_state
                .toolbar_rects
                .iter()
                .filter_map(|(id, rect)| {
                    if id.as_str() == dragging_id {
                        return None;
                    }
                    layout_state
                        .entries
                        .get(id)
                        .filter(|e| e.dock == candidate && e.visible)
                        .map(|_| *rect)
                })
                .reduce(|a, b| a.union(b))?;

            // Extend the band to the full screen width (horizontal) or height (vertical).
            // This covers the "before first toolbar" prepend region where the cursor is
            // outside the toolbar rects but in the same perpendicular slice.
            let extended = if horiz {
                egui::Rect::from_min_max(
                    egui::pos2(screen.min.x, band.min.y),
                    egui::pos2(screen.max.x, band.max.y),
                )
            } else {
                egui::Rect::from_min_max(
                    egui::pos2(band.min.x, screen.min.y),
                    egui::pos2(band.max.x, screen.max.y),
                )
            };

            if extended.contains(cursor) {
                Some(candidate)
            } else {
                None
            }
        });

        band_dock.or_else(|| dock_target_for_position(ctx, cursor))?
    };
    let horizontal = matches!(dock, ToolbarDock::Top | ToolbarDock::Bottom);

    // Collect all visible toolbar entries in this dock excluding the dragged one.
    // Each entry: (row, order, toolbar_id, rect_option).
    let mut entries: Vec<(u32, u32, String, Option<egui::Rect>)> = registry
        .toolbars()
        .filter_map(|desc| {
            if desc.id == dragging_id {
                return None;
            }
            let entry = layout_state.entries.get(&desc.id)?;
            if !entry.visible || entry.dock != dock {
                return None;
            }
            let rect = drag_state.toolbar_rects.get(&desc.id).copied();
            Some((entry.row, entry.order, desc.id.clone(), rect))
        })
        .collect();

    entries.sort_by_key(|(row, order, _, _)| (*row, *order));

    // Group by row index. Compute the band rect (union of all rects in row).
    // rows: Vec<(row_idx, Vec<(order, id, rect_option)>)>
    type ToolbarRowEntry = (u32, String, Option<egui::Rect>);
    type ToolbarRows = Vec<(u32, Vec<ToolbarRowEntry>)>;
    let mut rows: ToolbarRows = {
        let mut map: std::collections::BTreeMap<u32, Vec<ToolbarRowEntry>> =
            std::collections::BTreeMap::new();
        for (row, order, id, rect) in entries {
            map.entry(row).or_default().push((order, id, rect));
        }
        map.into_iter().collect()
    };
    // Sort toolbars within each row by order.
    for (_, row_items) in &mut rows {
        row_items.sort_by_key(|(order, _, _)| *order);
    }

    // Helper: union rect from a row's toolbar rects.
    let band_rect = |row_items: &[(u32, String, Option<egui::Rect>)]| -> Option<egui::Rect> {
        row_items
            .iter()
            .filter_map(|(_, _, r)| *r)
            .reduce(|acc, r| acc.union(r))
    };

    // Compute per-row band rects and the perpendicular coordinate of their midpoint.
    // For horizontal docks: perp = y (top-to-bottom). For vertical: perp = x.
    let perp_coord = |rect: egui::Rect| -> f32 {
        if horizontal {
            rect.center().y
        } else {
            rect.center().x
        }
    };
    let perp_cursor = if horizontal { cursor.y } else { cursor.x };

    // Find which row band the cursor falls into, or the gap between two bands.
    //
    // Strategy:
    // 1. For each existing row with a known rect, test if `perp_cursor` is inside.
    // 2. If it is, use that row.
    // 3. If it falls between two bands, propose a new row between them.
    // 4. If it's before the first band, propose row 0 (shifting existing rows down).
    // 5. If the dock is empty, use row 0.

    if rows.is_empty() {
        // Dock has no other toolbars: row 0, new row (only item), prepend.
        return Some(InsertionTarget {
            dock,
            row: 0,
            new_row: false, // nothing to shift
            insert_after: None,
        });
    }

    // Build a list of (row_idx, band_rect) for rows that have a known rect.
    let banded: Vec<(u32, egui::Rect)> = rows
        .iter()
        .filter_map(|(row_idx, items)| band_rect(items).map(|r| (*row_idx, r)))
        .collect();

    // Determine which row (or gap) the cursor is in.
    let (target_row_idx, is_new_row) = if banded.is_empty() {
        // No rects yet (first frame of a new drag) — fall back to first existing row.
        (rows[0].0, false)
    } else {
        // Check if cursor is inside any band.
        let hit_band = banded.iter().find(|(_, r)| {
            if horizontal {
                cursor.y >= r.min.y && cursor.y <= r.max.y
            } else {
                cursor.x >= r.min.x && cursor.x <= r.max.x
            }
        });

        if let Some((row_idx, _)) = hit_band {
            (*row_idx, false)
        } else {
            // Find the gap. Sort banded by perp midpoint.
            let mut sorted_bands = banded.clone();
            sorted_bands.sort_by(|(_, a), (_, b)| perp_coord(*a).total_cmp(&perp_coord(*b)));

            if perp_cursor < perp_coord(sorted_bands[0].1) {
                // Before the first band: insert new row before it.
                (sorted_bands[0].0, true)
            } else if perp_cursor > perp_coord(*sorted_bands.last().map(|(_, r)| r).unwrap()) {
                // After the last band: append new row after the last existing row.
                let last_row = rows.iter().map(|(r, _)| *r).max().unwrap_or(0);
                (last_row + 1, false) // not shifting — just a new row after the end
            } else {
                // Between two bands: find the gap.
                let gap_pos = sorted_bands
                    .windows(2)
                    .position(|pair| {
                        perp_cursor > perp_coord(pair[0].1) && perp_cursor < perp_coord(pair[1].1)
                    })
                    .unwrap_or(0);
                // Insert new row between pair[gap_pos] and pair[gap_pos+1].
                // The new row index = pair[gap_pos+1].row, shifting that row down.
                (sorted_bands[gap_pos + 1].0, true)
            }
        }
    };

    // Now find the insertion point within the target row.
    let row_items: &[(u32, String, Option<egui::Rect>)] = rows
        .iter()
        .find(|(r, _)| *r == target_row_idx)
        .map(|(_, items)| items.as_slice())
        .unwrap_or(&[]);

    let insert_after = if row_items.is_empty() {
        None
    } else if horizontal {
        // Find the first toolbar whose rect midpoint x is to the right of cursor.
        let insert_before_id = row_items
            .iter()
            .find(|(_, _, rect)| rect.map(|r| cursor.x < r.center().x).unwrap_or(false));
        match insert_before_id {
            None => {
                // Cursor is past all midpoints: append after the last toolbar.
                row_items.last().map(|(_, id, _)| id.clone())
            }
            Some((_, id, _)) => {
                // Insert before this toolbar = insert after the one before it.
                let pos = row_items.iter().position(|(_, i, _)| i == id).unwrap_or(0);
                if pos == 0 {
                    None // prepend
                } else {
                    row_items.get(pos - 1).map(|(_, id, _)| id.clone())
                }
            }
        }
    } else {
        // Vertical: same logic but with y.
        let insert_before_id = row_items
            .iter()
            .find(|(_, _, rect)| rect.map(|r| cursor.y < r.center().y).unwrap_or(false));
        match insert_before_id {
            None => row_items.last().map(|(_, id, _)| id.clone()),
            Some((_, id, _)) => {
                let pos = row_items.iter().position(|(_, i, _)| i == id).unwrap_or(0);
                if pos == 0 {
                    None
                } else {
                    row_items.get(pos - 1).map(|(_, id, _)| id.clone())
                }
            }
        }
    };

    Some(InsertionTarget {
        dock,
        row: target_row_idx,
        new_row: is_new_row,
        insert_after,
    })
}

/// Draws the landing slot indicator for cases that `draw_toolbar_row` cannot
/// handle inline:
/// - Inserting into a new row (between existing rows) — slot painted over the gap
/// - Dropping onto an empty dock — slot painted at the dock edge
///
/// For normal within-row insertions the slot is already rendered by
/// `draw_toolbar_row` using `draw_landing_slot`; this function is a no-op.
fn draw_dock_targets(
    ctx: &egui::Context,
    drag_state: &ToolbarDragState,
    toolbar_registry: &ToolbarRegistry,
    layout_state: &ToolbarLayoutState,
) {
    let Some(active_drag) = drag_state.active.as_ref() else {
        return;
    };
    let Some(target) = active_drag.target.as_ref() else {
        return;
    };

    let horizontal = matches!(target.dock, ToolbarDock::Top | ToolbarDock::Bottom);

    // Ghost/slot size based on the dragged toolbar.
    let Some(desc) = toolbar_registry
        .toolbars()
        .find(|d| d.id == active_drag.toolbar_id)
    else {
        return;
    };
    let size = toolbar_content_size(desc, horizontal);

    // Check whether the target dock has other visible toolbars.
    // If it does and this is not a new-row drop, draw_toolbar_row already shows
    // the slot inline — nothing to do here.
    let dock_has_others = toolbar_registry.toolbars().any(|d| {
        d.id != active_drag.toolbar_id
            && layout_state
                .entries
                .get(&d.id)
                .map(|e| e.visible && e.dock == target.dock)
                .unwrap_or(false)
    });
    if dock_has_others && !target.new_row {
        return;
    }

    let screen = ctx.viewport_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("toolbar_dock_slot"),
    ));

    // Position the slot.
    let slot_rect = if target.new_row {
        // Find the screen rect of the row being shifted (target.row).
        let shifted_band: Option<egui::Rect> = toolbar_registry
            .toolbars()
            .filter_map(|d| {
                if d.id == active_drag.toolbar_id {
                    return None;
                }
                let entry = layout_state.entries.get(&d.id)?;
                if entry.visible && entry.dock == target.dock && entry.row == target.row {
                    drag_state.toolbar_rects.get(&d.id).copied()
                } else {
                    None
                }
            })
            .reduce(|a, b| a.union(b));

        if horizontal {
            let y = shifted_band
                .map(|r| r.min.y - 4.0)
                .unwrap_or(screen.min.y + 4.0);
            egui::Rect::from_min_size(egui::pos2(screen.min.x + 8.0, y - size.y), size)
        } else {
            let x = shifted_band
                .map(|r| r.min.x - 4.0)
                .unwrap_or(screen.min.x + 4.0);
            egui::Rect::from_min_size(egui::pos2(x - size.x, screen.min.y + 8.0), size)
        }
    } else {
        // Empty dock: anchor the slot at the dock edge.
        if horizontal {
            let y = match target.dock {
                ToolbarDock::Top => screen.min.y + 4.0,
                _ => screen.max.y - size.y - 4.0,
            };
            egui::Rect::from_min_size(egui::pos2(screen.min.x + 8.0, y), size)
        } else {
            let x = match target.dock {
                ToolbarDock::Left => screen.min.x + 4.0,
                _ => screen.max.x - size.x - 4.0,
            };
            egui::Rect::from_min_size(egui::pos2(x, screen.min.y + 8.0), size)
        }
    };

    // Draw with the same style as draw_landing_slot.
    painter.rect_filled(
        slot_rect,
        egui::CornerRadius::same(4),
        CHROME_ACCENT.gamma_multiply(0.12),
    );
    let r = slot_rect.shrink(1.0);
    let s = egui::Stroke::new(2.0, CHROME_ACCENT);
    painter.line_segment([r.left_top(), r.right_top()], s);
    painter.line_segment([r.right_top(), r.right_bottom()], s);
    painter.line_segment([r.right_bottom(), r.left_bottom()], s);
    painter.line_segment([r.left_bottom(), r.left_top()], s);
}

fn draw_drag_ghost(
    ctx: &egui::Context,
    drag_state: &ToolbarDragState,
    toolbar_registry: &ToolbarRegistry,
) {
    let Some(active_drag) = drag_state.active.as_ref() else {
        return;
    };
    let Some(cursor_pos) = ctx.pointer_hover_pos() else {
        return;
    };
    let Some(descriptor) = toolbar_registry
        .toolbars()
        .find(|d| d.id == active_drag.toolbar_id)
    else {
        return;
    };

    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

    // Ghost orientation follows the hovered dock zone; falls back to the
    // toolbar's current dock so it stays in its native orientation mid-air.
    // Floating toolbars default to horizontal.
    let display_dock = active_drag
        .target
        .as_ref()
        .map(|t| t.dock)
        .unwrap_or(active_drag.original_dock);
    let horizontal = matches!(
        display_dock,
        ToolbarDock::Top | ToolbarDock::Bottom | ToolbarDock::Floating
    );

    egui::Area::new(egui::Id::new("toolbar_drag_ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(cursor_pos + egui::vec2(12.0, 12.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(CHROME_BG)
                .stroke(egui::Stroke::new(2.0, CHROME_ACCENT))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    // The grip dot offsets match draw_toolbar_grip: two columns of
                    // three dots for horizontal toolbars, two rows of three for vertical.
                    let dot_offsets: [[egui::Vec2; 3]; 2] = if horizontal {
                        [
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
                        [
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

                    let paint_grip = |ui: &mut egui::Ui| {
                        let (grip_rect, _) = ui.allocate_exact_size(
                            egui::vec2(TOOLBAR_GRIP_SIZE, TOOLBAR_GRIP_SIZE),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter();
                        for group in &dot_offsets {
                            for offset in group {
                                painter.rect_filled(
                                    egui::Rect::from_center_size(
                                        grip_rect.min + *offset,
                                        egui::vec2(TOOLBAR_GRIP_DOT_SIZE, TOOLBAR_GRIP_DOT_SIZE),
                                    ),
                                    1.0,
                                    CHROME_MUTED,
                                );
                            }
                        }
                    };

                    let paint_buttons = |ui: &mut egui::Ui, count: usize| {
                        for _ in 0..count {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(TOOLBAR_BUTTON_SIZE, TOOLBAR_BUTTON_SIZE),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                rect.shrink(4.0),
                                egui::CornerRadius::same(3),
                                CHROME_HOVER,
                            );
                        }
                    };

                    if descriptor.id == CAMERA_TOOLBAR_ID {
                        let (rect, _) = ui.allocate_exact_size(
                            camera_toolbar_content_size(horizontal),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter();
                        let grip_rect = egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(TOOLBAR_GRIP_SIZE, TOOLBAR_GRIP_SIZE),
                        );
                        for group in &dot_offsets {
                            for offset in group {
                                painter.rect_filled(
                                    egui::Rect::from_center_size(
                                        grip_rect.min + *offset,
                                        egui::vec2(TOOLBAR_GRIP_DOT_SIZE, TOOLBAR_GRIP_DOT_SIZE),
                                    ),
                                    1.0,
                                    CHROME_MUTED,
                                );
                            }
                        }
                        if horizontal {
                            let button_x = grip_rect.max.x + 12.0;
                            for index in 0..6 {
                                let x = button_x + index as f32 * (TOOLBAR_BUTTON_SIZE * 0.9);
                                let y = rect.center().y - TOOLBAR_BUTTON_SIZE * 0.5 + 1.0;
                                let button = egui::Rect::from_min_size(
                                    egui::pos2(x, y),
                                    egui::vec2(
                                        TOOLBAR_BUTTON_SIZE - 4.0,
                                        TOOLBAR_BUTTON_SIZE - 4.0,
                                    ),
                                );
                                painter.rect_filled(
                                    button,
                                    egui::CornerRadius::same(3),
                                    CHROME_HOVER,
                                );
                            }
                            let slider = egui::Rect::from_min_size(
                                egui::pos2(
                                    rect.max.x - CAMERA_LENS_SLIDER_WIDTH - 16.0,
                                    rect.center().y - 4.0,
                                ),
                                egui::vec2(CAMERA_LENS_SLIDER_WIDTH, 8.0),
                            );
                            painter.rect_filled(slider, egui::CornerRadius::same(4), CHROME_HOVER);
                        } else {
                            let x = rect.center().x - (TOOLBAR_BUTTON_SIZE - 4.0) * 0.5;
                            for index in 0..6 {
                                let y = grip_rect.max.y
                                    + 8.0
                                    + index as f32 * (TOOLBAR_BUTTON_SIZE * 0.85);
                                let button = egui::Rect::from_min_size(
                                    egui::pos2(x, y),
                                    egui::vec2(
                                        TOOLBAR_BUTTON_SIZE - 4.0,
                                        TOOLBAR_BUTTON_SIZE - 4.0,
                                    ),
                                );
                                painter.rect_filled(
                                    button,
                                    egui::CornerRadius::same(3),
                                    CHROME_HOVER,
                                );
                            }
                            let slider = egui::Rect::from_min_size(
                                egui::pos2(
                                    rect.center().x - 4.0,
                                    rect.max.y - CAMERA_VERTICAL_SLIDER_HEIGHT - 12.0,
                                ),
                                egui::vec2(8.0, CAMERA_VERTICAL_SLIDER_HEIGHT),
                            );
                            painter.rect_filled(slider, egui::CornerRadius::same(4), CHROME_HOVER);
                        }
                    } else if horizontal {
                        let button_count: usize = descriptor
                            .sections
                            .iter()
                            .map(|section| section.command_ids.len())
                            .sum();
                        ui.horizontal(|ui| {
                            paint_grip(ui);
                            paint_buttons(ui, button_count.min(12));
                        });
                    } else {
                        let button_count: usize = descriptor
                            .sections
                            .iter()
                            .map(|section| section.command_ids.len())
                            .sum();
                        ui.vertical(|ui| {
                            paint_grip(ui);
                            paint_buttons(ui, button_count.min(8));
                        });
                    }
                });
        });
}

fn complete_toolbar_drag(
    ctx: &egui::Context,
    drag_state: &mut ToolbarDragState,
    toolbar_layout_state: &mut ToolbarLayoutState,
    floating_states: &mut FloatingToolbarStates,
    doc_props: &mut DocumentProperties,
) {
    let Some(active_drag) = drag_state.active.as_ref() else {
        return;
    };
    if ctx.input(|input| input.pointer.primary_down()) {
        return;
    }

    let toolbar_id = active_drag.toolbar_id.clone();
    let target = active_drag.target.clone();
    drag_state.active = None;

    let Some(target) = target else {
        // No dock target — tear off into a floating window.
        if let Some(cursor) = ctx.pointer_hover_pos() {
            apply_toolbar_float(
                toolbar_layout_state,
                floating_states,
                doc_props,
                &toolbar_id,
                [cursor.x, cursor.y],
            );
        }
        return;
    };
    // Apply even when dock is unchanged — handles same-row reordering.
    redock_toolbar(
        toolbar_layout_state,
        floating_states,
        doc_props,
        &toolbar_id,
        RedockTarget {
            dock: target.dock,
            target_row: target.row,
            new_row: target.new_row,
            insert_after: target.insert_after.as_deref(),
        },
    );
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
