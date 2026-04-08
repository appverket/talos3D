use bevy::prelude::*;
use bevy_egui::egui;

use crate::plugins::{
    command_registry::{
        queue_command_invocation_resource, CommandDescriptor, CommandRegistry,
        PendingCommandInvocations,
    },
    ui::StatusBarData,
};

const PALETTE_MAX_ROWS: usize = 9;
const PALETTE_WINDOW_SIZE: egui::Vec2 = egui::vec2(760.0, 420.0);
const PALETTE_WINDOW_OFFSET: [f32; 2] = [0.0, 64.0];

pub struct PalettePlugin;

impl Plugin for PalettePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PaletteState>()
            .add_systems(Update, toggle_palette);
    }
}

#[derive(Resource, Default, Debug, Clone)]
pub struct PaletteState {
    open: bool,
    filter: String,
    selected_index: usize,
    recent_command_ids: Vec<String>,
}

impl PaletteState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn show(&mut self) {
        self.open = true;
        self.filter.clear();
        self.selected_index = 0;
    }

    pub(crate) fn hide(&mut self) {
        self.open = false;
        self.filter.clear();
        self.selected_index = 0;
    }

    fn record_recent(&mut self, command_id: &str) {
        self.recent_command_ids.retain(|id| id != command_id);
        self.recent_command_ids.insert(0, command_id.to_string());
        self.recent_command_ids.truncate(8);
    }
}

#[derive(Debug, Clone)]
struct PaletteCommandEntry {
    descriptor: CommandDescriptor,
    score: i32,
    recent_rank: Option<usize>,
    disabled_reason: Option<&'static str>,
}

impl PaletteCommandEntry {
    fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }
}

fn toggle_palette(world: &mut World) {
    let primary_modifier_pressed =
        primary_modifier_pressed(world.resource::<ButtonInput<KeyCode>>());
    let open_shortcut = {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        (primary_modifier_pressed && keys.just_pressed(KeyCode::KeyK))
            || (!primary_modifier_pressed && keys.just_pressed(KeyCode::Slash))
    };
    if !open_shortcut {
        return;
    }

    let mut palette_state = world.resource_mut::<PaletteState>();
    if palette_state.is_open() {
        palette_state.hide();
        world.resource_mut::<StatusBarData>().command_hint = None;
    } else {
        palette_state.show();
    }
}

pub fn draw_command_palette(
    ctx: &egui::Context,
    palette_state: &mut PaletteState,
    registry: &CommandRegistry,
    pending: &mut PendingCommandInvocations,
    status_bar_data: &mut StatusBarData,
    selection_count: usize,
) {
    if !palette_state.is_open() {
        if status_bar_data.command_hint.is_some() {
            status_bar_data.command_hint = None;
        }
        return;
    }

    let entries = palette_command_entries(
        registry,
        palette_state.filter.as_str(),
        &palette_state.recent_command_ids,
        selection_count,
    );

    if entries.is_empty() {
        palette_state.selected_index = 0;
    } else {
        palette_state.selected_index = palette_state.selected_index.min(entries.len() - 1);
    }

    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        palette_state.hide();
        status_bar_data.command_hint = None;
        return;
    }
    if !entries.is_empty() && ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
        palette_state.selected_index = palette_state
            .selected_index
            .checked_sub(1)
            .unwrap_or(entries.len() - 1);
    }
    if !entries.is_empty() && ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
        palette_state.selected_index = (palette_state.selected_index + 1) % entries.len();
    }

    status_bar_data.command_hint = entries
        .get(palette_state.selected_index)
        .and_then(|entry| entry.descriptor.hint.clone());

    let mut should_execute = ctx.input(|input| input.key_pressed(egui::Key::Enter));
    egui::Window::new("Command Palette")
        .id(egui::Id::new("command_palette_window"))
        .anchor(egui::Align2::CENTER_TOP, PALETTE_WINDOW_OFFSET)
        .fixed_size(PALETTE_WINDOW_SIZE)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let search = egui::TextEdit::singleline(&mut palette_state.filter)
                .hint_text("Search commands, categories, ids, or type command arguments")
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Heading);
            let search_response = ui.add(search);
            if !search_response.has_focus() {
                search_response.request_focus();
            }
            if search_response.changed() {
                palette_state.selected_index = 0;
            }

            ui.add_space(8.0);

            if entries.is_empty() {
                ui.label(
                    egui::RichText::new("No commands match the current search.")
                        .italics()
                        .weak(),
                );
                return;
            }

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .id_salt("command_palette_results")
                .show(ui, |ui| {
                    for (index, entry) in entries.iter().take(PALETTE_MAX_ROWS).enumerate() {
                        let selected = index == palette_state.selected_index;
                        let response = egui::Frame::new()
                            .fill(if selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                let mut label = entry.descriptor.label.clone();
                                if palette_state.filter.trim().is_empty()
                                    && entry.recent_rank.is_some()
                                {
                                    label = format!("Recent · {label}");
                                }
                                if let Some(shortcut) = entry.descriptor.default_shortcut.as_deref()
                                {
                                    label.push_str(&format!("  [{shortcut}]"));
                                }
                                let label_text = if entry.enabled() {
                                    egui::RichText::new(label)
                                } else {
                                    egui::RichText::new(label).weak()
                                };
                                ui.label(label_text.strong());
                                let detail = if let Some(reason) = entry.disabled_reason {
                                    egui::RichText::new(format!(
                                        "{reason}  ·  {}",
                                        palette_entry_description(entry)
                                    ))
                                    .small()
                                    .weak()
                                } else {
                                    egui::RichText::new(palette_entry_description(entry))
                                        .small()
                                        .weak()
                                };
                                ui.label(detail);
                            })
                            .response
                            .interact(egui::Sense::click());

                        if response.clicked() {
                            palette_state.selected_index = index;
                        }
                        if response.double_clicked() {
                            palette_state.selected_index = index;
                            should_execute = true;
                        }
                    }
                });

            ui.separator();
            if let Some(entry) = entries.get(palette_state.selected_index) {
                if let Some(hint) = entry.descriptor.hint.as_deref() {
                    ui.label(egui::RichText::new(hint).small().weak());
                }
            }
        });

    if should_execute {
        execute_palette_selection(
            palette_state,
            registry,
            pending,
            status_bar_data,
            selection_count,
        );
    }
}

fn execute_palette_selection(
    palette_state: &mut PaletteState,
    registry: &CommandRegistry,
    pending: &mut PendingCommandInvocations,
    status_bar_data: &mut StatusBarData,
    selection_count: usize,
) {
    let invocation = resolve_palette_invocation(
        registry,
        palette_state.filter.as_str(),
        &palette_state.recent_command_ids,
        selection_count,
        palette_state.selected_index,
    );
    let (descriptor, parameters) = match invocation {
        Ok(Some(invocation)) => invocation,
        Ok(None) => return,
        Err(error) => {
            status_bar_data.set_feedback(error, 2.0);
            return;
        }
    };
    if let Some(reason) = disabled_reason_for_selection(selection_count, &descriptor) {
        status_bar_data.set_feedback(reason.to_string(), 2.0);
        return;
    }

    let command_id = descriptor.id.clone();
    queue_command_invocation_resource(pending, command_id.clone(), parameters);
    palette_state.record_recent(&command_id);
    palette_state.hide();
    status_bar_data.command_hint = None;
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn filtered_commands(
    registry: &CommandRegistry,
    filter: &str,
) -> Vec<CommandDescriptor> {
    palette_command_entries(registry, filter, &[], 0)
        .into_iter()
        .map(|entry| entry.descriptor)
        .collect()
}

fn palette_command_entries(
    registry: &CommandRegistry,
    filter: &str,
    recent_command_ids: &[String],
    selection_count: usize,
) -> Vec<PaletteCommandEntry> {
    if let Some((descriptor, _)) = parameterized_palette_prefix(registry, filter) {
        let recent_rank = recent_command_ids
            .iter()
            .position(|id| id == &descriptor.id);
        return vec![PaletteCommandEntry {
            disabled_reason: disabled_reason_for_selection(selection_count, &descriptor),
            descriptor,
            recent_rank,
            score: 100_000,
        }];
    }

    let filter = filter.trim().to_ascii_lowercase();
    let mut entries: Vec<_> = registry
        .commands()
        .filter_map(|descriptor| {
            palette_match_score(descriptor, &filter, recent_command_ids).map(|score| {
                PaletteCommandEntry {
                    disabled_reason: disabled_reason_for_selection(selection_count, descriptor),
                    descriptor: descriptor.clone(),
                    recent_rank: recent_command_ids
                        .iter()
                        .position(|id| id == &descriptor.id),
                    score,
                }
            })
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                left.descriptor
                    .category
                    .label()
                    .cmp(right.descriptor.category.label())
            })
            .then_with(|| left.descriptor.label.cmp(&right.descriptor.label))
    });
    entries
}

fn resolve_palette_invocation(
    registry: &CommandRegistry,
    filter: &str,
    recent_command_ids: &[String],
    selection_count: usize,
    selected_index: usize,
) -> Result<Option<(CommandDescriptor, serde_json::Value)>, String> {
    if let Some((descriptor, remainder)) = parameterized_palette_prefix(registry, filter) {
        let parameters = parse_palette_parameters(&descriptor, &remainder)?;
        return Ok(Some((descriptor, parameters)));
    }

    Ok(
        palette_command_entries(registry, filter, recent_command_ids, selection_count)
            .into_iter()
            .nth(selected_index)
            .map(|entry| (entry.descriptor, serde_json::json!({}))),
    )
}

fn parameterized_palette_prefix(
    registry: &CommandRegistry,
    filter: &str,
) -> Option<(CommandDescriptor, String)> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return None;
    }

    registry
        .commands()
        .filter(|descriptor| descriptor.parameters.is_some())
        .filter_map(|descriptor| {
            palette_command_prefix_match(trimmed, &descriptor.label)
                .or_else(|| palette_command_prefix_match(trimmed, &descriptor.id))
                .map(|remainder| (descriptor.clone(), remainder))
        })
        .max_by_key(|(descriptor, _)| descriptor.label.len())
}

fn palette_command_prefix_match(input: &str, prefix: &str) -> Option<String> {
    let input_lower = input.to_ascii_lowercase();
    let prefix_lower = prefix.to_ascii_lowercase();
    if input_lower == prefix_lower {
        return Some(String::new());
    }
    input_lower
        .strip_prefix(&(prefix_lower + " "))
        .map(|remainder| remainder.trim().to_string())
}

fn parse_palette_parameters(
    descriptor: &CommandDescriptor,
    remainder: &str,
) -> Result<serde_json::Value, String> {
    let Some(schema) = descriptor.parameters.as_ref() else {
        return Ok(serde_json::json!({}));
    };
    let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) else {
        return Ok(serde_json::json!({}));
    };
    if properties.contains_key("x") && properties.contains_key("y") && properties.contains_key("z")
    {
        let coordinates = remainder
            .replace(',', " ")
            .split_whitespace()
            .map(str::parse::<f32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| format!("{} expects three numeric coordinates", descriptor.label))?;
        if coordinates.len() != 3 {
            return Err(format!(
                "{} expects three numeric coordinates",
                descriptor.label
            ));
        }
        return Ok(serde_json::json!({
            "x": coordinates[0],
            "y": coordinates[1],
            "z": coordinates[2],
        }));
    }

    if remainder.is_empty() {
        Ok(serde_json::json!({}))
    } else {
        Err(format!(
            "{} does not accept palette arguments",
            descriptor.label
        ))
    }
}

fn palette_match_score(
    descriptor: &CommandDescriptor,
    filter: &str,
    recent_command_ids: &[String],
) -> Option<i32> {
    let recent_boost = recent_command_ids
        .iter()
        .position(|id| id == &descriptor.id)
        .map(|index| 500 - (index as i32 * 25))
        .unwrap_or_default();
    if filter.is_empty() {
        return Some(recent_boost);
    }

    let label = descriptor.label.to_ascii_lowercase();
    let id = descriptor.id.to_ascii_lowercase();
    let description = descriptor.description.to_ascii_lowercase();
    let category = descriptor.category.label().to_ascii_lowercase();
    let shortcut = descriptor
        .default_shortcut
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let capability = descriptor
        .capability_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut score = 0;
    score = score.max(text_match_score(&label, filter, 8_000));
    score = score.max(text_match_score(&id, filter, 6_500));
    score = score.max(text_match_score(&description, filter, 3_000));
    score = score.max(text_match_score(&category, filter, 2_400));
    score = score.max(text_match_score(&shortcut, filter, 2_000));
    score = score.max(text_match_score(&capability, filter, 1_800));
    (score > 0).then_some(score + recent_boost)
}

fn text_match_score(haystack: &str, needle: &str, base: i32) -> i32 {
    if haystack.is_empty() || needle.is_empty() {
        return 0;
    }
    if haystack == needle {
        return base + 1_200;
    }
    if haystack.starts_with(needle) {
        return base + 900 - haystack.len().min(80) as i32;
    }
    if let Some(index) = haystack.find(needle) {
        let word_bonus = if index == 0
            || haystack[..index]
                .chars()
                .last()
                .is_some_and(|character| matches!(character, ' ' | '.' | '_' | '-'))
        {
            350
        } else {
            0
        };
        return base + 500 + word_bonus - index.min(60) as i32;
    }
    subsequence_match_score(haystack, needle).map_or(0, |score| base / 2 + score)
}

fn subsequence_match_score(haystack: &str, needle: &str) -> Option<i32> {
    let mut score = 0;
    let mut haystack_index = 0usize;
    let mut last_match = None;
    let haystack_chars: Vec<_> = haystack.chars().collect();

    for needle_char in needle.chars() {
        let mut found = None;
        for (index, hay_char) in haystack_chars.iter().enumerate().skip(haystack_index) {
            if *hay_char == needle_char {
                found = Some(index);
                break;
            }
        }
        let index = found?;
        score += 30;
        if index == 0 {
            score += 30;
        }
        if let Some(previous) = last_match {
            if index == previous + 1 {
                score += 25;
            }
            score -= (index.saturating_sub(previous + 1)).min(6) as i32 * 2;
        } else {
            score -= index.min(8) as i32 * 3;
        }
        haystack_index = index + 1;
        last_match = Some(index);
    }

    Some(score.max(1))
}

fn disabled_reason_for_selection(
    selection_count: usize,
    descriptor: &CommandDescriptor,
) -> Option<&'static str> {
    if descriptor.requires_selection && selection_count == 0 {
        Some("Requires a selection")
    } else {
        None
    }
}

fn palette_entry_description(entry: &PaletteCommandEntry) -> String {
    let mut parts = vec![entry.descriptor.category.label().to_string()];
    if let Some(capability_id) = entry.descriptor.capability_id.as_deref() {
        parts.push(capability_id.to_string());
    }
    if !entry.descriptor.description.is_empty() {
        parts.push(entry.descriptor.description.clone());
    }
    parts.push(entry.descriptor.id.clone());
    parts.join("  ·  ")
}

fn primary_modifier_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::command_registry::CommandRegistryPlugin;

    #[test]
    fn filtered_commands_keep_parameterized_prefix_matches_visible() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let commands = filtered_commands(registry, "Set Pivot 1 2 3");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "core.set_pivot");
    }

    #[test]
    fn resolve_palette_invocation_parses_xyz_triplets() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let (descriptor, parameters) =
            resolve_palette_invocation(registry, "Set Pivot 1 2 3", &[], 0, 0)
                .expect("set pivot input should parse")
                .expect("set pivot should resolve to a command");

        assert_eq!(descriptor.id, "core.set_pivot");
        assert_eq!(
            parameters,
            serde_json::json!({"x": 1.0, "y": 2.0, "z": 3.0})
        );
    }

    #[test]
    fn filtered_commands_match_category_text() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let commands = filtered_commands(registry, "view");
        assert!(commands.iter().any(|descriptor| {
            descriptor.category == crate::plugins::command_registry::CommandCategory::View
        }));
    }

    #[test]
    fn palette_entries_surface_disabled_reason_for_selection_commands() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let entry = palette_command_entries(registry, "Delete", &[], 0)
            .into_iter()
            .find(|entry| entry.descriptor.id == "core.delete")
            .expect("delete command should be present");

        assert_eq!(entry.disabled_reason, Some("Requires a selection"));
    }

    #[test]
    fn recent_commands_rank_ahead_when_palette_is_empty() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let entries = palette_command_entries(
            registry,
            "",
            &["core.save".to_string(), "core.open".to_string()],
            0,
        );

        assert_eq!(entries[0].descriptor.id, "core.save");
        assert_eq!(entries[1].descriptor.id, "core.open");
    }
}
