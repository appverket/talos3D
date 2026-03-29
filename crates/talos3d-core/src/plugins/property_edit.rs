use bevy::prelude::*;

use crate::{
    authored_entity::{BoxedEntity, PropertyValue, PropertyValueKind},
    capability_registry::CapabilityRegistry,
    plugins::{
        commands::ApplyEntityChangesCommand,
        palette::PaletteState,
        selection::Selected,
        transform::{ActiveTransformPreview, TransformState},
        ui::StatusBarData,
    },
};

const FEEDBACK_DURATION_SECONDS: f32 = 2.0;

pub struct PropertyEditPlugin;

impl Plugin for PropertyEditPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PropertyEditState>()
            .init_resource::<PropertyPanelState>()
            .init_resource::<PropertyPanelData>()
            .add_systems(Update, sync_property_panel_data.before(open_property_mode))
            .add_systems(
                Update,
                (
                    open_property_mode,
                    handle_property_mode_input,
                    update_property_mode_status,
                ),
            );
    }
}

#[derive(Resource, Default)]
pub struct PropertyEditState {
    session: Option<PropertySession>,
}

impl PropertyEditState {
    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    pub fn clear(&mut self) {
        self.session = None;
    }
}

#[derive(Resource, Default, Debug, Clone)]
pub struct PropertyPanelState {
    pub visible: bool,
    pub interacting: bool,
    pub active_field: Option<String>,
    pub buffer: String,
}

#[derive(Resource, Default, Clone)]
pub struct PropertyPanelData {
    pub snapshots: Vec<BoxedEntity>,
    pub entity_type: Option<&'static str>,
    pub mixed_selection: bool,
}

fn sync_property_panel_data(world: &mut World) {
    let snapshots = if !world.resource::<TransformState>().is_idle() {
        world.resource::<ActiveTransformPreview>().snapshots.clone()
    } else {
        collect_selected_snapshots(world)
    };
    let entity_type = selected_snapshots_type(&snapshots);
    let mixed_selection = !snapshots.is_empty() && entity_type.is_none();

    {
        let mut panel_data = world.resource_mut::<PropertyPanelData>();
        panel_data.snapshots = snapshots;
        panel_data.entity_type = entity_type;
        panel_data.mixed_selection = mixed_selection;
    }

    let visible = !world.resource::<PropertyPanelData>().snapshots.is_empty();
    let mut panel_state = world.resource_mut::<PropertyPanelState>();
    panel_state.visible = visible;
    if !visible {
        panel_state.interacting = false;
        panel_state.active_field = None;
        panel_state.buffer.clear();
    }
}

#[derive(Debug, Clone)]
struct PropertyField {
    name: &'static str,
    label: &'static str,
    kind: PropertyValueKind,
    original: Option<PropertyValue>,
    buffer: String,
    dirty: bool,
}

#[derive(Debug, Clone)]
struct PropertySession {
    originals: Vec<BoxedEntity>,
    entity_type: &'static str,
    fields: Vec<PropertyField>,
    active_index: usize,
    replace_on_next_input: bool,
}

fn open_property_mode(world: &mut World) {
    let should_open = {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        let transform_state = world.resource::<TransformState>();
        let property_edit_state = world.resource::<PropertyEditState>();
        let property_panel_state = world.resource::<PropertyPanelState>();
        let palette_state = world.resource::<PaletteState>();
        !palette_state.is_open()
            && !property_panel_state.visible
            && !property_edit_state.is_active()
            && transform_state.is_idle()
            && keys.just_pressed(KeyCode::Tab)
    };
    if !should_open {
        return;
    }

    let snapshots = collect_selected_snapshots(world);
    let Some(entity_type) = selected_snapshots_type(&snapshots) else {
        world.resource_mut::<StatusBarData>().set_feedback(
            "Select one or more entities of the same type".to_string(),
            FEEDBACK_DURATION_SECONDS,
        );
        return;
    };

    let fields = build_property_fields(&snapshots);
    if entity_type == "polyline" || fields.is_empty() {
        world.resource_mut::<StatusBarData>().set_feedback(
            "Polyline properties are not editable yet".to_string(),
            FEEDBACK_DURATION_SECONDS,
        );
        return;
    }

    world.resource_mut::<PropertyEditState>().session = Some(PropertySession {
        originals: snapshots,
        entity_type,
        fields,
        active_index: 0,
        replace_on_next_input: true,
    });
}

pub(crate) fn collect_selected_snapshots(world: &mut World) -> Vec<BoxedEntity> {
    let mut selected_query = world.query_filtered::<Entity, With<Selected>>();
    let selected_entities: Vec<Entity> = selected_query.iter(world).collect();
    let registry = world.resource::<CapabilityRegistry>();

    selected_entities
        .into_iter()
        .filter_map(|entity| world.get_entity(entity).ok())
        .filter_map(|entity_ref| registry.capture_snapshot(&entity_ref, world))
        .collect()
}

fn handle_property_mode_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut property_edit_state: ResMut<PropertyEditState>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut apply_entity_changes_commands: MessageWriter<ApplyEntityChangesCommand>,
) {
    let Some(session) = property_edit_state.session.as_mut() else {
        return;
    };

    if keys.just_pressed(KeyCode::Escape) {
        property_edit_state.clear();
        return;
    }

    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        match build_property_after_snapshots(session) {
            Ok(after) => {
                let before = session.originals.clone();
                if before != after {
                    apply_entity_changes_commands.write(ApplyEntityChangesCommand {
                        label: "Edit properties",
                        before,
                        after,
                    });
                }
                property_edit_state.clear();
            }
            Err(error) => status_bar_data.set_feedback(error, FEEDBACK_DURATION_SECONDS),
        }
        return;
    }

    let shift_pressed = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if keys.just_pressed(KeyCode::Tab) {
        navigate_fields(session, if shift_pressed { -1 } else { 1 });
        return;
    }
    if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::ArrowUp) {
        navigate_fields(session, -1);
        return;
    }
    if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::ArrowDown) {
        navigate_fields(session, 1);
        return;
    }

    if keys.just_pressed(KeyCode::Backspace) || keys.just_pressed(KeyCode::NumpadBackspace) {
        edit_active_field(session, None);
        return;
    }

    for key in keys.get_just_pressed() {
        if let Some(character) = property_input_char(*key) {
            edit_active_field(session, Some(character));
        }
    }
}

fn update_property_mode_status(
    property_edit_state: Res<PropertyEditState>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    status_bar_data.property_text = property_edit_state
        .session
        .as_ref()
        .map(format_property_session);
}

pub(crate) fn selected_snapshots_type(snapshots: &[BoxedEntity]) -> Option<&'static str> {
    let first = snapshots.first()?.type_name();
    snapshots
        .iter()
        .all(|snapshot| snapshot.type_name() == first)
        .then_some(first)
}

fn build_property_fields(snapshots: &[BoxedEntity]) -> Vec<PropertyField> {
    let Some(first) = snapshots.first() else {
        return Vec::new();
    };

    first
        .property_fields()
        .into_iter()
        .filter(|field| field.editable)
        .map(|field| PropertyField {
            name: field.name,
            label: field.label,
            kind: field.kind,
            original: shared_property_value(snapshots, field.name),
            buffer: String::new(),
            dirty: false,
        })
        .collect()
}

pub(crate) fn shared_property_value(
    snapshots: &[BoxedEntity],
    field_name: &str,
) -> Option<PropertyValue> {
    let first = snapshots
        .first()?
        .property_fields()
        .into_iter()
        .find(|field| field.name == field_name)?
        .value;

    snapshots
        .iter()
        .skip(1)
        .all(|snapshot| {
            snapshot
                .property_fields()
                .into_iter()
                .find(|field| field.name == field_name)
                .and_then(|field| field.value)
                == first
        })
        .then_some(first?)
}

fn format_property_session(session: &PropertySession) -> String {
    let fields = session
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let value_text = if field.dirty {
                field.buffer.clone()
            } else {
                field
                    .original
                    .as_ref()
                    .map(format_property_value)
                    .unwrap_or_else(|| "*".to_string())
            };

            if index == session.active_index {
                format!("[{}={}]", field.label, value_text)
            } else {
                format!("{}={}", field.label, value_text)
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");

    format!(
        "Properties ({}) : {fields} · Enter apply · Esc cancel",
        session.entity_type
    )
}

fn format_property_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Scalar(value) => format!("{value:.2}"),
        PropertyValue::Vec2(value) => format!("{:.2},{:.2}", value.x, value.y),
        PropertyValue::Vec3(value) => format!("{:.2},{:.2},{:.2}", value.x, value.y, value.z),
        PropertyValue::Text(value) => value.clone(),
    }
}

fn navigate_fields(session: &mut PropertySession, direction: isize) {
    if session.fields.is_empty() {
        return;
    }

    let len = session.fields.len() as isize;
    session.active_index = ((session.active_index as isize + direction).rem_euclid(len)) as usize;
    session.replace_on_next_input = true;
}

fn edit_active_field(session: &mut PropertySession, character: Option<char>) {
    let field = &mut session.fields[session.active_index];
    if session.replace_on_next_input {
        field.buffer.clear();
        field.dirty = true;
        session.replace_on_next_input = false;
    } else if !field.dirty {
        field.buffer = field
            .original
            .as_ref()
            .map(format_property_value)
            .unwrap_or_default();
        field.dirty = true;
    }

    match character {
        Some(character) => field.buffer.push(character),
        None => {
            field.buffer.pop();
        }
    }
}

fn property_input_char(key: KeyCode) -> Option<char> {
    match key {
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
        KeyCode::Period | KeyCode::NumpadDecimal => Some('.'),
        KeyCode::Comma | KeyCode::NumpadComma => Some(','),
        KeyCode::Minus | KeyCode::NumpadSubtract => Some('-'),
        _ => None,
    }
}

fn build_property_after_snapshots(session: &PropertySession) -> Result<Vec<BoxedEntity>, String> {
    let parsed_values = session
        .fields
        .iter()
        .map(parse_field_value)
        .collect::<Result<Vec<_>, _>>()?;

    session
        .originals
        .iter()
        .map(|snapshot| apply_field_values(snapshot, &session.fields, &parsed_values))
        .collect()
}

fn parse_field_value(field: &PropertyField) -> Result<Option<PropertyValue>, String> {
    if !field.dirty {
        return Ok(None);
    }

    if field.buffer.trim().is_empty() {
        return Err(format!("{} cannot be empty", field.label));
    }

    let value = parse_property_value(field.kind.clone(), &field.buffer)?;

    Ok(Some(value))
}

fn apply_field_values(
    snapshot: &BoxedEntity,
    fields: &[PropertyField],
    values: &[Option<PropertyValue>],
) -> Result<BoxedEntity, String> {
    let mut updated = snapshot.clone();
    for (field, value) in fields.iter().zip(values) {
        let Some(value) = value else {
            continue;
        };
        updated = updated.set_property_json(field.name, &value.to_json())?;
    }
    Ok(updated)
}

fn parse_scalar(buffer: &str) -> Result<f32, String> {
    buffer
        .trim()
        .parse::<f32>()
        .map_err(|_| format!("Invalid number: {buffer}"))
}

fn parse_vec2(buffer: &str) -> Result<Vec2, String> {
    let values = parse_components::<2>(buffer)?;
    Ok(Vec2::new(values[0], values[1]))
}

fn parse_vec3(buffer: &str) -> Result<Vec3, String> {
    let values = parse_components::<3>(buffer)?;
    Ok(Vec3::new(values[0], values[1], values[2]))
}

fn parse_components<const N: usize>(buffer: &str) -> Result<[f32; N], String> {
    let parts: Vec<&str> = buffer
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() != N {
        return Err(format!("Expected {N} comma-separated values"));
    }

    let mut values = [0.0; N];
    for (index, part) in parts.iter().enumerate() {
        values[index] = part
            .parse::<f32>()
            .map_err(|_| format!("Invalid number: {part}"))?;
    }
    Ok(values)
}

pub(crate) fn parse_property_value(
    kind: PropertyValueKind,
    buffer: &str,
) -> Result<PropertyValue, String> {
    Ok(match kind {
        PropertyValueKind::Scalar => PropertyValue::Scalar(parse_scalar(buffer)?),
        PropertyValueKind::Vec2 => PropertyValue::Vec2(parse_vec2(buffer)?),
        PropertyValueKind::Vec3 => PropertyValue::Vec3(parse_vec3(buffer)?),
        PropertyValueKind::Text => PropertyValue::Text(buffer.to_string()),
    })
}
