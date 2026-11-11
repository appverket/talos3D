//! Outliner gestures only choose targets; domain edits use registered commands.
use super::*;

pub(super) fn padlock_image(ui: &egui::Ui, locked: bool) -> egui::Image<'static> {
    let name = if locked { "lock_closed" } else { "lock_open" };
    let id = egui::Id::new(("outliner_padlock", name));
    // Reuse the toolbar artwork pipeline, rasterized/uploaded once per UI context.
    // Only the virtualized visible rows submit textured quads on subsequent frames.
    let texture = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(id));
    let texture = texture.unwrap_or_else(|| {
        let rgba = super::super::icons::render_icon(name);
        let size = super::super::icons::ICON_SIZE as usize;
        let texture = ui.ctx().load_texture(
            name,
            egui::ColorImage::from_rgba_unmultiplied([size, size], &rgba),
            egui::TextureOptions::LINEAR,
        );
        ui.ctx()
            .data_mut(|data| data.insert_temp(id, texture.clone()));
        texture
    });
    egui::Image::new((texture.id(), egui::vec2(16.0, 16.0))).alt_text(if locked {
        "Locked"
    } else {
        "Unlocked"
    })
}

pub(super) fn row_selection_action(
    tree: &OutlinerTree,
    state: &mut OutlinerWindowState,
    index: usize,
    modifiers: egui::Modifiers,
) -> Option<OutlinerSelectAction> {
    let node = &tree.nodes[index];
    let target = node.select_entity?;
    if modifiers.shift {
        if let Some(anchor) = state.selection_anchor.and_then(|id| {
            tree.nodes
                .iter()
                .position(|n| n.node_id == id && n.select_entity.is_some())
        }) {
            if tree.nodes[anchor].parent == node.parent {
                let siblings = node
                    .parent
                    .map_or(&tree.roots, |parent| &tree.nodes[parent].children);
                if let (Some(start), Some(end)) = (
                    siblings.iter().position(|i| *i == anchor),
                    siblings.iter().position(|i| *i == index),
                ) {
                    return Some(OutlinerSelectAction {
                        target,
                        additive: modifiers.command,
                        range: Some(
                            siblings[start.min(end)..=start.max(end)]
                                .iter()
                                .filter_map(|i| tree.nodes[*i].select_entity)
                                .collect(),
                        ),
                    });
                }
            }
        }
    }
    // A missing/deleted anchor or a different parent starts a fresh range.
    state.selection_anchor = Some(node.node_id);
    Some(OutlinerSelectAction {
        target,
        additive: modifiers.command,
        range: None,
    })
}

pub(super) fn can_group_selection(tree: &OutlinerTree, selected: &HashSet<Entity>) -> bool {
    if selected.len() < 2 {
        return false;
    }
    let mut parent = None;
    let mut found = HashSet::new();
    for node in &tree.nodes {
        if let Some(entity) = node.select_entity.filter(|e| selected.contains(e)) {
            if node.visibility_id.is_none()
                || node.kind == OutlinerKind::Part
                || node.effectively_locked
            {
                return false;
            }
            if parent.is_some_and(|parent| parent != node.parent) {
                return false;
            }
            parent = Some(node.parent);
            found.insert(entity);
        }
    }
    found.len() == selected.len()
}

pub(super) fn draw_row_context_menu(
    ui: &mut egui::Ui,
    node: &OutlinerNode,
    tree: &OutlinerTree,
    selected: &HashSet<Entity>,
    pending: &mut PendingCommandInvocations,
    registry: &CommandRegistry,
) {
    let selected_here = node.select_entity.is_some_and(|e| selected.contains(&e));
    if node.select_entity.is_some() {
        if ui
            .add_enabled(
                selected_here && can_group_selection(tree, selected),
                egui::Button::new("Group Selected"),
            )
            .on_disabled_hover_text("Select two or more editable objects under the same parent")
            .clicked()
        {
            queue_command_invocation_resource(
                pending,
                "modeling.group".to_string(),
                serde_json::json!({}),
            );
            ui.close();
        }
        if ui.button("Lock Selected").clicked() {
            queue_command_invocation_resource(
                pending,
                "view.lock_selected".to_string(),
                serde_json::json!({}),
            );
            ui.close();
        }
    } else if node.effectively_locked
        && ui
            .add_enabled(node.explicitly_locked, egui::Button::new("Unlock"))
            .on_disabled_hover_text("Unlock the parent group first")
            .clicked()
    {
        queue_command_invocation_resource(
            pending,
            "view.set_object_locked".to_string(),
            serde_json::json!({"element_ids":[node.node_id],"locked":false}),
        );
        ui.close();
    }
    if node.kind != OutlinerKind::Group || node.effectively_locked {
        return;
    }
    ui.separator();
    let (command_id, fallback_label) = if node.is_linked_model {
        ("modeling.open_linked_model", "Open Linked Model")
    } else {
        (
            "modeling.create_linked_model_from_selection",
            "Create Linked Model",
        )
    };
    let label = registry
        .get(command_id)
        .map(|d| d.label.as_str())
        .unwrap_or(fallback_label);
    if ui.button(label).clicked() {
        queue_command_invocation_resource(
            pending,
            command_id.to_string(),
            serde_json::json!({"group_id":node.node_id}),
        );
        ui.close();
    }
    if node.is_linked_model {
        let command_id = "modeling.refresh_linked_models";
        let label = registry
            .get(command_id)
            .map(|d| d.label.as_str())
            .unwrap_or("Refresh Linked Model");
        if ui.button(label).clicked() {
            queue_command_invocation_resource(
                pending,
                command_id.to_string(),
                serde_json::json!({"group_ids":[node.node_id],"force":true}),
            );
            ui.close();
        }
    }
}
