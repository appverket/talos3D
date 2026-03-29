use bevy::{ecs::system::SystemParam, prelude::*};

use crate::plugins::{
    command_registry::{
        queue_command_invocation, CommandDescriptor, CommandRegistry, IconRegistry,
    },
    property_edit::PropertyEditState,
    selection::Selected,
    transform::TransformState,
    ui::StatusBarData,
};

const PALETTE_WIDTH_PX: f32 = 720.0;
const PALETTE_TOP_PX: f32 = 72.0;
const PALETTE_MAX_ROWS: usize = 9;
const PALETTE_BG: Color = Color::srgba(0.08, 0.09, 0.11, 0.96);
const PALETTE_PANEL_BG: Color = Color::srgba(0.12, 0.13, 0.16, 0.98);
const PALETTE_ROW_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.0);
const PALETTE_ROW_ACTIVE_BG: Color = Color::srgba(0.22, 0.32, 0.45, 0.95);
const PALETTE_TEXT: Color = Color::srgb(0.92, 0.94, 0.98);
const PALETTE_DISABLED_TEXT: Color = Color::srgb(0.48, 0.5, 0.56);
const PALETTE_MUTED_TEXT: Color = Color::srgb(0.72, 0.75, 0.8);
const PALETTE_INPUT_SIZE: f32 = 20.0;
const PALETTE_LABEL_SIZE: f32 = 16.0;
const PALETTE_DESC_SIZE: f32 = 13.0;
const PALETTE_ICON_SIZE: f32 = 14.0;

pub struct PalettePlugin;

impl Plugin for PalettePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PaletteState>()
            .add_systems(Startup, setup_palette_ui)
            .add_systems(
                Update,
                (
                    toggle_palette,
                    handle_palette_input,
                    update_palette_command_hint,
                    update_palette_ui,
                ),
            );
    }
}

#[derive(Resource, Default, Debug, Clone)]
pub struct PaletteState {
    open: bool,
    filter: String,
    selected_index: usize,
}

impl PaletteState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn open(&mut self) {
        self.open = true;
        self.filter.clear();
        self.selected_index = 0;
    }

    fn close(&mut self) {
        self.open = false;
        self.filter.clear();
        self.selected_index = 0;
    }
}

#[derive(Component)]
struct PaletteRoot;

#[derive(Component)]
struct PaletteInputText;

#[derive(Component)]
struct PaletteRow {
    index: usize,
}

#[derive(Component)]
struct PaletteRowIcon {
    index: usize,
}

#[derive(Component)]
struct PaletteRowLabel {
    index: usize,
}

#[derive(Component)]
struct PaletteRowDescription {
    index: usize,
}

type PaletteRootFilter = (
    With<PaletteRoot>,
    Without<PaletteRow>,
    Without<PaletteRowIcon>,
);
type PaletteInputFilter = (
    With<PaletteInputText>,
    Without<PaletteRowLabel>,
    Without<PaletteRowDescription>,
);
type PaletteRowFilter = (Without<PaletteRoot>, Without<PaletteRowIcon>);
type PaletteIconFilter = (Without<PaletteRoot>, Without<PaletteRow>);
type PaletteLabelFilter = (Without<PaletteInputText>, Without<PaletteRowDescription>);
type PaletteDescriptionFilter = (Without<PaletteInputText>, Without<PaletteRowLabel>);

#[derive(SystemParam)]
struct PaletteUiQueries<'w, 's> {
    root_query: Query<'w, 's, &'static mut Node, PaletteRootFilter>,
    input_query: Query<'w, 's, &'static mut Text, PaletteInputFilter>,
    row_query: Query<
        'w,
        's,
        (
            &'static PaletteRow,
            &'static mut Node,
            &'static mut BackgroundColor,
        ),
        PaletteRowFilter,
    >,
    icon_query: Query<
        'w,
        's,
        (
            &'static PaletteRowIcon,
            &'static mut Node,
            &'static mut ImageNode,
        ),
        PaletteIconFilter,
    >,
    label_query: Query<
        'w,
        's,
        (
            &'static PaletteRowLabel,
            &'static mut Text,
            &'static mut TextColor,
        ),
        PaletteLabelFilter,
    >,
    description_query: Query<
        'w,
        's,
        (
            &'static PaletteRowDescription,
            &'static mut Text,
            &'static mut TextColor,
        ),
        PaletteDescriptionFilter,
    >,
}

fn setup_palette_ui(mut commands: Commands) {
    commands
        .spawn((
            PaletteRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                display: Display::None,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Start,
                ..default()
            },
            BackgroundColor(PALETTE_BG),
            GlobalZIndex(20),
        ))
        .with_children(|parent| {
            parent
                .spawn((
                    Node {
                        width: Val::Px(PALETTE_WIDTH_PX),
                        margin: UiRect::top(Val::Px(PALETTE_TOP_PX)),
                        padding: UiRect::all(Val::Px(14.0)),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        ..default()
                    },
                    BackgroundColor(PALETTE_PANEL_BG),
                ))
                .with_children(|parent| {
                    parent.spawn((
                        PaletteInputText,
                        Text::new("> "),
                        TextFont {
                            font_size: PALETTE_INPUT_SIZE,
                            ..default()
                        },
                        TextColor(PALETTE_TEXT),
                    ));

                    for index in 0..PALETTE_MAX_ROWS {
                        parent
                            .spawn((
                                PaletteRow { index },
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::axes(Val::Px(10.0), Val::Px(8.0)),
                                    column_gap: Val::Px(10.0),
                                    align_items: AlignItems::Start,
                                    ..default()
                                },
                                BackgroundColor(PALETTE_ROW_BG),
                            ))
                            .with_children(|parent| {
                                parent.spawn((
                                    PaletteRowIcon { index },
                                    ImageNode::default(),
                                    Node {
                                        width: Val::Px(PALETTE_ICON_SIZE),
                                        height: Val::Px(PALETTE_ICON_SIZE),
                                        margin: UiRect::top(Val::Px(4.0)),
                                        ..default()
                                    },
                                ));
                                parent
                                    .spawn(Node {
                                        flex_grow: 1.0,
                                        flex_direction: FlexDirection::Column,
                                        row_gap: Val::Px(3.0),
                                        ..default()
                                    })
                                    .with_children(|parent| {
                                        parent.spawn((
                                            PaletteRowLabel { index },
                                            Text::new(""),
                                            TextFont {
                                                font_size: PALETTE_LABEL_SIZE,
                                                ..default()
                                            },
                                            TextColor(PALETTE_TEXT),
                                        ));
                                        parent.spawn((
                                            PaletteRowDescription { index },
                                            Text::new(""),
                                            TextFont {
                                                font_size: PALETTE_DESC_SIZE,
                                                ..default()
                                            },
                                            TextColor(PALETTE_MUTED_TEXT),
                                        ));
                                    });
                            });
                    }
                });
        });
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
        palette_state.close();
        world.resource_mut::<StatusBarData>().command_hint = None;
    } else {
        palette_state.open();
    }
}

fn handle_palette_input(world: &mut World) {
    if !world.resource::<PaletteState>().is_open() {
        return;
    }

    if world.resource::<PropertyEditState>().is_active()
        || !world.resource::<TransformState>().is_idle()
    {
        world.resource_mut::<PaletteState>().close();
        world.resource_mut::<StatusBarData>().command_hint = None;
        return;
    }

    let (
        escape_pressed,
        backspace_pressed,
        enter_pressed,
        primary_modifier_active,
        arrow_up_pressed,
        arrow_down_pressed,
        just_pressed,
    ) = {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        (
            keys.just_pressed(KeyCode::Escape),
            keys.just_pressed(KeyCode::Backspace) || keys.just_pressed(KeyCode::NumpadBackspace),
            keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter),
            primary_modifier_pressed(keys),
            keys.just_pressed(KeyCode::ArrowUp),
            keys.just_pressed(KeyCode::ArrowDown),
            keys.get_just_pressed().copied().collect::<Vec<_>>(),
        )
    };

    if escape_pressed {
        world.resource_mut::<PaletteState>().close();
        world.resource_mut::<StatusBarData>().command_hint = None;
        return;
    }

    let descriptors = filtered_commands(
        world.resource::<CommandRegistry>(),
        world.resource::<PaletteState>().filter.as_str(),
    );
    let descriptor_count = descriptors.len();

    {
        let mut state = world.resource_mut::<PaletteState>();
        if descriptor_count == 0 {
            state.selected_index = 0;
        } else {
            state.selected_index = state.selected_index.min(descriptor_count - 1);
            if arrow_up_pressed {
                state.selected_index = state
                    .selected_index
                    .checked_sub(1)
                    .unwrap_or(descriptor_count.saturating_sub(1));
            }
            if arrow_down_pressed {
                state.selected_index = (state.selected_index + 1) % descriptor_count;
            }
        }
    }

    if backspace_pressed {
        let mut state = world.resource_mut::<PaletteState>();
        state.filter.pop();
        state.selected_index = 0;
    }

    if !primary_modifier_active {
        let mut inserted_any = false;
        for key in just_pressed {
            if let Some(character) = palette_input_char(key) {
                world.resource_mut::<PaletteState>().filter.push(character);
                inserted_any = true;
            }
        }
        if inserted_any {
            world.resource_mut::<PaletteState>().selected_index = 0;
        }
    }

    if !enter_pressed {
        return;
    }

    let invocation = {
        let state = world.resource::<PaletteState>();
        resolve_palette_invocation(
            world.resource::<CommandRegistry>(),
            state.filter.as_str(),
            state.selected_index,
        )
    };
    let (descriptor, parameters) = match invocation {
        Ok(Some(invocation)) => invocation,
        Ok(None) => return,
        Err(error) => {
            world
                .resource_mut::<StatusBarData>()
                .set_feedback(error, 2.0);
            return;
        }
    };
    let selection_count = world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .count();
    if !command_enabled_with_selection(selection_count, &descriptor) {
        return;
    }

    queue_command_invocation(world, descriptor.id, parameters);
    world.resource_mut::<PaletteState>().close();
    world.resource_mut::<StatusBarData>().command_hint = None;
}

fn update_palette_command_hint(
    palette_state: Res<PaletteState>,
    registry: Res<CommandRegistry>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if !palette_state.is_open() {
        if status_bar_data.command_hint.is_some() {
            status_bar_data.command_hint = None;
        }
        return;
    }

    let hint = filtered_commands(&registry, palette_state.filter.as_str())
        .into_iter()
        .nth(palette_state.selected_index)
        .and_then(|descriptor| descriptor.hint.clone());
    status_bar_data.command_hint = hint;
}

fn update_palette_ui(
    palette_state: Res<PaletteState>,
    registry: Res<CommandRegistry>,
    icon_registry: Res<IconRegistry>,
    selected_query: Query<Entity, With<Selected>>,
    mut ui: PaletteUiQueries,
) {
    let Ok(mut root_node) = ui.root_query.single_mut() else {
        return;
    };
    root_node.display = if palette_state.is_open() {
        Display::Flex
    } else {
        Display::None
    };
    if !palette_state.is_open() {
        return;
    }

    if let Ok(mut input) = ui.input_query.single_mut() {
        **input = format!("> {}", palette_state.filter);
    }

    let descriptors = filtered_commands(&registry, palette_state.filter.as_str());
    let selection_count = selected_query.iter().count();
    let visible_start = palette_state
        .selected_index
        .saturating_sub(PALETTE_MAX_ROWS.saturating_sub(1));

    for (row, mut node, mut background) in &mut ui.row_query {
        let descriptor_index = visible_start + row.index;
        let Some(_descriptor) = descriptors.get(descriptor_index) else {
            node.display = Display::None;
            continue;
        };
        node.display = Display::Flex;
        *background = BackgroundColor(if descriptor_index == palette_state.selected_index {
            PALETTE_ROW_ACTIVE_BG
        } else {
            PALETTE_ROW_BG
        });
    }

    for (row_icon, mut node, mut image) in &mut ui.icon_query {
        let descriptor_index = visible_start + row_icon.index;
        let descriptor = descriptors.get(descriptor_index);
        if let Some(handle) = descriptor
            .and_then(|descriptor| descriptor.icon.as_deref())
            .and_then(|icon| icon_registry.get(icon))
        {
            node.display = Display::Flex;
            *image = ImageNode::new(handle);
        } else {
            node.display = Display::None;
            *image = ImageNode::default();
        }
    }

    for (row_label, mut text, mut color) in &mut ui.label_query {
        let descriptor_index = visible_start + row_label.index;
        let Some(descriptor) = descriptors.get(descriptor_index) else {
            **text = String::new();
            continue;
        };
        let enabled = command_enabled_with_selection(selection_count, descriptor);
        let shortcut = descriptor
            .default_shortcut
            .as_deref()
            .map(|shortcut| format!("  [{shortcut}]"))
            .unwrap_or_default();
        **text = format!("{}{}", descriptor.label, shortcut);
        color.0 = if enabled {
            PALETTE_TEXT
        } else {
            PALETTE_DISABLED_TEXT
        };
    }

    for (row_description, mut text, mut color) in &mut ui.description_query {
        let descriptor_index = visible_start + row_description.index;
        let Some(descriptor) = descriptors.get(descriptor_index) else {
            **text = String::new();
            continue;
        };
        let enabled = command_enabled_with_selection(selection_count, descriptor);
        **text = format!("{}  ({})", descriptor.description, descriptor.id);
        color.0 = if enabled {
            PALETTE_MUTED_TEXT
        } else {
            PALETTE_DISABLED_TEXT
        };
    }
}

pub(crate) fn filtered_commands(
    registry: &CommandRegistry,
    filter: &str,
) -> Vec<CommandDescriptor> {
    if let Some((descriptor, _)) = parameterized_palette_prefix(registry, filter) {
        return vec![descriptor];
    }

    let filter = filter.trim().to_ascii_lowercase();
    registry
        .commands()
        .filter(|descriptor| {
            filter.is_empty()
                || descriptor.label.to_ascii_lowercase().contains(&filter)
                || descriptor.id.to_ascii_lowercase().contains(&filter)
                || descriptor
                    .description
                    .to_ascii_lowercase()
                    .contains(&filter)
        })
        .cloned()
        .collect()
}

fn resolve_palette_invocation(
    registry: &CommandRegistry,
    filter: &str,
    selected_index: usize,
) -> Result<Option<(CommandDescriptor, serde_json::Value)>, String> {
    if let Some((descriptor, remainder)) = parameterized_palette_prefix(registry, filter) {
        let parameters = parse_palette_parameters(&descriptor, &remainder)?;
        return Ok(Some((descriptor, parameters)));
    }

    Ok(filtered_commands(registry, filter)
        .into_iter()
        .nth(selected_index)
        .map(|descriptor| (descriptor, serde_json::json!({}))))
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

fn command_enabled_with_selection(selection_count: usize, descriptor: &CommandDescriptor) -> bool {
    !descriptor.requires_selection || selection_count > 0
}

fn primary_modifier_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    }
}

fn palette_input_char(key: KeyCode) -> Option<char> {
    match key {
        KeyCode::Space => Some(' '),
        KeyCode::Minus | KeyCode::NumpadSubtract => Some('-'),
        KeyCode::Period | KeyCode::NumpadDecimal | KeyCode::NumpadComma => Some('.'),
        KeyCode::Slash | KeyCode::NumpadDivide => Some('/'),
        KeyCode::Digit0 | KeyCode::Numpad0 => Some('0'),
        KeyCode::Digit1 | KeyCode::Numpad1 => Some('1'),
        KeyCode::Digit2 | KeyCode::Numpad2 => Some('2'),
        KeyCode::Digit3 | KeyCode::Numpad3 => Some('3'),
        KeyCode::Digit4 | KeyCode::Numpad4 => Some('4'),
        KeyCode::Digit5 | KeyCode::Numpad5 => Some('5'),
        KeyCode::Digit6 | KeyCode::Numpad6 => Some('6'),
        KeyCode::Digit7 | KeyCode::Numpad7 => Some('7'),
        KeyCode::Digit8 | KeyCode::Numpad8 => Some('8'),
        KeyCode::Digit9 | KeyCode::Numpad9 => Some('9'),
        KeyCode::KeyA => Some('a'),
        KeyCode::KeyB => Some('b'),
        KeyCode::KeyC => Some('c'),
        KeyCode::KeyD => Some('d'),
        KeyCode::KeyE => Some('e'),
        KeyCode::KeyF => Some('f'),
        KeyCode::KeyG => Some('g'),
        KeyCode::KeyH => Some('h'),
        KeyCode::KeyI => Some('i'),
        KeyCode::KeyJ => Some('j'),
        KeyCode::KeyK => Some('k'),
        KeyCode::KeyL => Some('l'),
        KeyCode::KeyM => Some('m'),
        KeyCode::KeyN => Some('n'),
        KeyCode::KeyO => Some('o'),
        KeyCode::KeyP => Some('p'),
        KeyCode::KeyQ => Some('q'),
        KeyCode::KeyR => Some('r'),
        KeyCode::KeyS => Some('s'),
        KeyCode::KeyT => Some('t'),
        KeyCode::KeyU => Some('u'),
        KeyCode::KeyV => Some('v'),
        KeyCode::KeyW => Some('w'),
        KeyCode::KeyX => Some('x'),
        KeyCode::KeyY => Some('y'),
        KeyCode::KeyZ => Some('z'),
        _ => None,
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
        let (descriptor, parameters) = resolve_palette_invocation(registry, "Set Pivot 1 2 3", 0)
            .expect("set pivot input should parse")
            .expect("set pivot should resolve to a command");

        assert_eq!(descriptor.id, "core.set_pivot");
        assert_eq!(
            parameters,
            serde_json::json!({"x": 1.0, "y": 2.0, "z": 3.0})
        );
    }
}
