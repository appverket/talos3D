use std::collections::VecDeque;

use bevy::{
    ecs::{message::Messages, world::EntityRef},
    input::{
        keyboard::{Key, KeyCode, KeyboardInput},
        mouse::{MouseButton, MouseButtonInput, MouseMotion},
        ButtonState,
    },
    prelude::*,
    window::{CursorMoved, PrimaryWindow},
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "model-api")]
use rmcp::schemars::JsonSchema;

use crate::{
    authored_entity::EntityScope,
    capability_registry::CapabilityRegistry,
    plugins::{
        camera::OrbitCamera, cursor::cursor_window_position, egui_chrome::EguiWantsInput,
        identity::ElementId,
        input_ownership::{InputOwnership, InputPhase},
        tools::ActiveTool,
    },
};

pub struct UxHarnessPlugin;

impl Plugin for UxHarnessPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UxHarnessState>()
            .add_systems(
                Update,
                process_ux_harness_step
                    .after(InputPhase::SyncOwnership)
                    .before(InputPhase::ToolInput),
            );
    }
}

#[derive(Resource, Default)]
struct UxHarnessState {
    next_sequence: u64,
    completed_sequence: u64,
    steps: VecDeque<UxStep>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
struct UxStep {
    sequence: u64,
    action: UxStepAction,
}

#[derive(Debug, Clone)]
enum UxStepAction {
    MovePointer(Vec2),
    MouseButton {
        position: Vec2,
        button: MouseButton,
        state: ButtonState,
    },
    Key {
        key_code: KeyCode,
        logical_key: Key,
        state: ButtonState,
    },
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxPointerMoveRequest {
    pub x: f32,
    pub y: f32,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxClickRequest {
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub button: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxDragRequest {
    pub start: [f32; 2],
    pub end: [f32; 2],
    #[serde(default)]
    pub button: Option<String>,
    #[serde(default)]
    pub steps: Option<u32>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxPressKeyRequest {
    pub key_code: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxInputResult {
    pub sequence: u64,
    pub queued_steps: usize,
    pub pending_steps: usize,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxHarnessSnapshot {
    pub pending_steps: usize,
    pub completed_sequence: u64,
    pub last_error: Option<String>,
    pub input_ownership: String,
    pub egui_pointer: bool,
    pub egui_keyboard: bool,
    pub window: Option<UxWindowSnapshot>,
    pub cursor: Option<UxCursorSnapshot>,
    pub active_tool: Option<String>,
    pub selected_element_ids: Vec<u64>,
    pub entities: Vec<UxEntityProjection>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxWindowSnapshot {
    pub width: f32,
    pub height: f32,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxCursorSnapshot {
    pub window: [f32; 2],
    pub viewport: [f32; 2],
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxEntityProjection {
    pub element_id: u64,
    pub entity_type: String,
    pub label: String,
    pub selected: bool,
    pub world_bounds: Option<UxBounds3>,
    pub screen_bounds: Option<UxRect>,
    pub screen_center: Option<[f32; 2]>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxBounds3 {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

pub fn enqueue_pointer_move(
    world: &mut World,
    request: UxPointerMoveRequest,
) -> Result<UxInputResult, String> {
    enqueue_steps(
        world,
        vec![UxStepAction::MovePointer(Vec2::new(request.x, request.y))],
    )
}

pub fn enqueue_click(world: &mut World, request: UxClickRequest) -> Result<UxInputResult, String> {
    let button = parse_mouse_button(request.button.as_deref())?;
    let position = Vec2::new(request.x, request.y);
    enqueue_steps(
        world,
        vec![
            UxStepAction::MovePointer(position),
            UxStepAction::MouseButton {
                position,
                button,
                state: ButtonState::Pressed,
            },
            UxStepAction::MouseButton {
                position,
                button,
                state: ButtonState::Released,
            },
        ],
    )
}

pub fn enqueue_drag(world: &mut World, request: UxDragRequest) -> Result<UxInputResult, String> {
    let button = parse_mouse_button(request.button.as_deref())?;
    let start = Vec2::new(request.start[0], request.start[1]);
    let end = Vec2::new(request.end[0], request.end[1]);
    let step_count = request.steps.unwrap_or(12).clamp(1, 120);
    let mut actions = Vec::new();
    actions.push(UxStepAction::MovePointer(start));
    actions.push(UxStepAction::MouseButton {
        position: start,
        button,
        state: ButtonState::Pressed,
    });
    for index in 1..=step_count {
        let t = index as f32 / step_count as f32;
        actions.push(UxStepAction::MovePointer(start.lerp(end, t)));
    }
    actions.push(UxStepAction::MouseButton {
        position: end,
        button,
        state: ButtonState::Released,
    });
    enqueue_steps(world, actions)
}

pub fn enqueue_press_key(
    world: &mut World,
    request: UxPressKeyRequest,
) -> Result<UxInputResult, String> {
    let (key_code, logical_key) = parse_key_code(&request.key_code)?;
    enqueue_steps(
        world,
        vec![
            UxStepAction::Key {
                key_code,
                logical_key: logical_key.clone(),
                state: ButtonState::Pressed,
            },
            UxStepAction::Key {
                key_code,
                logical_key,
                state: ButtonState::Released,
            },
        ],
    )
}

pub fn observe_ux(world: &mut World) -> Result<UxHarnessSnapshot, String> {
    let (pending_steps, completed_sequence, last_error) = world
        .get_resource::<UxHarnessState>()
        .map(|state| {
            (
                state.steps.len(),
                state.completed_sequence,
                state.last_error.clone(),
            )
        })
        .unwrap_or_default();
    let (window, cursor) = observe_window_cursor(world);
    let selected_element_ids = selected_element_ids(world);
    let entities = observe_entity_projections(world);

    Ok(UxHarnessSnapshot {
        pending_steps,
        completed_sequence,
        last_error,
        input_ownership: world
            .get_resource::<InputOwnership>()
            .map(|ownership| format!("{ownership:?}"))
            .unwrap_or_else(|| "Unavailable".to_string()),
        egui_pointer: world
            .get_resource::<EguiWantsInput>()
            .map(|wants| wants.pointer)
            .unwrap_or(false),
        egui_keyboard: world
            .get_resource::<EguiWantsInput>()
            .map(|wants| wants.keyboard)
            .unwrap_or(false),
        window,
        cursor,
        active_tool: active_tool(world),
        selected_element_ids,
        entities,
    })
}

fn enqueue_steps(world: &mut World, actions: Vec<UxStepAction>) -> Result<UxInputResult, String> {
    if actions.is_empty() {
        return Err("No UX input steps requested".to_string());
    }
    let mut state = world.resource_mut::<UxHarnessState>();
    state.next_sequence += 1;
    let sequence = state.next_sequence;
    let queued_steps = actions.len();
    for action in actions {
        state.steps.push_back(UxStep { sequence, action });
    }
    Ok(UxInputResult {
        sequence,
        queued_steps,
        pending_steps: state.steps.len(),
    })
}

fn process_ux_harness_step(world: &mut World) {
    let Some(step) = pop_next_step(world) else {
        return;
    };
    let result = apply_step(world, &step.action);
    let mut state = world.resource_mut::<UxHarnessState>();
    state.completed_sequence = step.sequence;
    if let Err(error) = result {
        state.last_error = Some(error);
    }
}

fn pop_next_step(world: &mut World) -> Option<UxStep> {
    world
        .get_resource_mut::<UxHarnessState>()
        .and_then(|mut state| state.steps.pop_front())
}

fn apply_step(world: &mut World, action: &UxStepAction) -> Result<(), String> {
    match action {
        UxStepAction::MovePointer(position) => move_pointer(world, *position),
        UxStepAction::MouseButton {
            position,
            button,
            state,
        } => {
            move_pointer(world, *position)?;
            let window = primary_window_entity(world)?;
            world
                .resource_mut::<Messages<MouseButtonInput>>()
                .write(MouseButtonInput {
                    button: *button,
                    state: *state,
                    window,
                });
            let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
            match state {
                ButtonState::Pressed => buttons.press(*button),
                ButtonState::Released => buttons.release(*button),
            }
            Ok(())
        }
        UxStepAction::Key {
            key_code,
            logical_key,
            state,
        } => {
            let window = primary_window_entity(world)?;
            world
                .resource_mut::<Messages<KeyboardInput>>()
                .write(KeyboardInput {
                    key_code: *key_code,
                    logical_key: logical_key.clone(),
                    state: *state,
                    text: match (logical_key, *state) {
                        (Key::Character(value), ButtonState::Pressed) => Some(value.clone()),
                        _ => None,
                    },
                    repeat: false,
                    window,
                });
            let mut keys = world.resource_mut::<ButtonInput<KeyCode>>();
            match state {
                ButtonState::Pressed => keys.press(*key_code),
                ButtonState::Released => keys.release(*key_code),
            }
            Ok(())
        }
    }
}

fn move_pointer(world: &mut World, position: Vec2) -> Result<(), String> {
    let (window_entity, previous) = {
        let mut window_query = world.query_filtered::<(Entity, &mut Window), With<PrimaryWindow>>();
        let (window_entity, mut window) = window_query
            .single_mut(world)
            .map_err(|_| "UX harness requires exactly one primary window".to_string())?;
        let previous = cursor_window_position(&window);
        window.set_cursor_position(Some(position));
        (window_entity, previous)
    };
    world
        .resource_mut::<Messages<CursorMoved>>()
        .write(CursorMoved {
            window: window_entity,
            position,
            delta: previous.map(|prev| position - prev),
        });
    if let Some(previous) = previous {
        world
            .resource_mut::<Messages<MouseMotion>>()
            .write(MouseMotion {
                delta: position - previous,
            });
    }
    Ok(())
}

fn primary_window_entity(world: &mut World) -> Result<Entity, String> {
    let mut window_query = world.query_filtered::<Entity, With<PrimaryWindow>>();
    window_query
        .single(world)
        .map_err(|_| "UX harness requires exactly one primary window".to_string())
}

fn parse_mouse_button(value: Option<&str>) -> Result<MouseButton, String> {
    match value.unwrap_or("left").to_ascii_lowercase().as_str() {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        other => Err(format!(
            "Unsupported mouse button '{other}'. Expected left, right, or middle."
        )),
    }
}

fn parse_key_code(value: &str) -> Result<(KeyCode, Key), String> {
    let normalized = value.trim();
    if normalized.len() == 1 {
        let ch = normalized
            .chars()
            .next()
            .expect("one-character key string has a char");
        return key_code_for_letter(ch).map(|key_code| {
            (
                key_code,
                Key::Character(ch.to_ascii_lowercase().to_string().into()),
            )
        });
    }
    match normalized.to_ascii_lowercase().as_str() {
        "escape" | "esc" => Ok((KeyCode::Escape, Key::Escape)),
        "delete" | "del" => Ok((KeyCode::Delete, Key::Delete)),
        "backspace" => Ok((KeyCode::Backspace, Key::Backspace)),
        "enter" | "return" => Ok((KeyCode::Enter, Key::Enter)),
        "shiftleft" | "shift_left" | "leftshift" | "left_shift" => {
            Ok((KeyCode::ShiftLeft, Key::Shift))
        }
        "shiftright" | "shift_right" | "rightshift" | "right_shift" => {
            Ok((KeyCode::ShiftRight, Key::Shift))
        }
        value if value.starts_with("key") && value.len() == 4 => {
            let ch = value.chars().nth(3).expect("keyX has a char");
            key_code_for_letter(ch).map(|key_code| {
                (
                    key_code,
                    Key::Character(ch.to_ascii_lowercase().to_string().into()),
                )
            })
        }
        other => Err(format!(
            "Unsupported key_code '{other}'. Use a letter, KeyG-style name, Escape, Delete, Backspace, Enter, ShiftLeft, or ShiftRight."
        )),
    }
}

fn key_code_for_letter(ch: char) -> Result<KeyCode, String> {
    match ch.to_ascii_lowercase() {
        'a' => Ok(KeyCode::KeyA),
        'b' => Ok(KeyCode::KeyB),
        'c' => Ok(KeyCode::KeyC),
        'd' => Ok(KeyCode::KeyD),
        'e' => Ok(KeyCode::KeyE),
        'f' => Ok(KeyCode::KeyF),
        'g' => Ok(KeyCode::KeyG),
        'h' => Ok(KeyCode::KeyH),
        'i' => Ok(KeyCode::KeyI),
        'j' => Ok(KeyCode::KeyJ),
        'k' => Ok(KeyCode::KeyK),
        'l' => Ok(KeyCode::KeyL),
        'm' => Ok(KeyCode::KeyM),
        'n' => Ok(KeyCode::KeyN),
        'o' => Ok(KeyCode::KeyO),
        'p' => Ok(KeyCode::KeyP),
        'q' => Ok(KeyCode::KeyQ),
        'r' => Ok(KeyCode::KeyR),
        's' => Ok(KeyCode::KeyS),
        't' => Ok(KeyCode::KeyT),
        'u' => Ok(KeyCode::KeyU),
        'v' => Ok(KeyCode::KeyV),
        'w' => Ok(KeyCode::KeyW),
        'x' => Ok(KeyCode::KeyX),
        'y' => Ok(KeyCode::KeyY),
        'z' => Ok(KeyCode::KeyZ),
        other => Err(format!("Unsupported letter key '{other}'.")),
    }
}

fn observe_window_cursor(
    world: &mut World,
) -> (Option<UxWindowSnapshot>, Option<UxCursorSnapshot>) {
    let mut window_query = world.query_filtered::<&Window, With<PrimaryWindow>>();
    let Ok(window) = window_query.single(world) else {
        return (None, None);
    };
    let window_snapshot = Some(UxWindowSnapshot {
        width: window.width(),
        height: window.height(),
    });
    let Some(window_cursor) = cursor_window_position(window) else {
        return (window_snapshot, None);
    };
    let viewport_cursor = viewport_position(world, window_cursor).unwrap_or(window_cursor);
    (
        window_snapshot,
        Some(UxCursorSnapshot {
            window: vec2_array(window_cursor),
            viewport: vec2_array(viewport_cursor),
        }),
    )
}

fn viewport_position(world: &mut World, window_position: Vec2) -> Option<Vec2> {
    let mut camera_query = world.query::<(&Camera, Option<&OrbitCamera>)>();
    camera_query
        .iter(world)
        .find(|(camera, orbit)| camera.is_active && orbit.is_some())
        .or_else(|| camera_query.iter(world).next())
        .map(|(camera, _)| match camera.logical_viewport_rect() {
            Some(rect) => window_position - rect.min,
            None => window_position,
        })
}

fn selected_element_ids(world: &mut World) -> Vec<u64> {
    let mut query = world.query_filtered::<&ElementId, With<crate::plugins::selection::Selected>>();
    let mut ids: Vec<u64> = query.iter(world).map(|id| id.0).collect();
    ids.sort_unstable();
    ids
}

fn active_tool(world: &World) -> Option<String> {
    world
        .get_resource::<State<ActiveTool>>()
        .map(|tool| format!("{:?}", tool.get()))
}

fn observe_entity_projections(world: &mut World) -> Vec<UxEntityProjection> {
    let camera = active_camera(world);
    let selected = selected_element_ids(world);
    let registry = world.resource::<CapabilityRegistry>();
    let mut query = world
        .try_query::<EntityRef>()
        .expect("EntityRef query should be available");
    let mut entries = Vec::new();

    for entity_ref in query.iter(world) {
        if !registry.is_user_facing_entity(world, entity_ref.id()) {
            continue;
        }
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        if snapshot.scope() != EntityScope::AuthoredModel {
            continue;
        }
        let bounds = snapshot.bounds();
        let screen_bounds = camera.as_ref().and_then(|camera| {
            bounds.and_then(|bounds| project_bounds(bounds, &camera.0, &camera.1))
        });
        let screen_center = camera
            .as_ref()
            .and_then(|camera| project_world_point(snapshot.center(), &camera.0, &camera.1));
        let element_id = snapshot.element_id().0;
        entries.push(UxEntityProjection {
            element_id,
            entity_type: snapshot.type_name().to_string(),
            label: snapshot.label(),
            selected: selected.contains(&element_id),
            world_bounds: bounds.map(bounds3),
            screen_bounds,
            screen_center,
        });
    }
    entries.sort_by_key(|entry| entry.element_id);
    entries
}

fn active_camera(world: &mut World) -> Option<(Camera, GlobalTransform)> {
    let mut camera_query = world.query::<(&Camera, &GlobalTransform, Option<&OrbitCamera>)>();
    camera_query
        .iter(world)
        .find(|(camera, _, orbit)| camera.is_active && orbit.is_some())
        .or_else(|| camera_query.iter(world).next())
        .map(|(camera, transform, _)| (camera.clone(), *transform))
}

fn project_bounds(
    bounds: crate::authored_entity::EntityBounds,
    camera: &Camera,
    camera_transform: &GlobalTransform,
) -> Option<UxRect> {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut any = false;
    for corner in bounds.corners() {
        if let Some(point) = project_world_point(corner, camera, camera_transform) {
            min = min.min(Vec2::from(point));
            max = max.max(Vec2::from(point));
            any = true;
        }
    }
    any.then_some(UxRect {
        min: vec2_array(min),
        max: vec2_array(max),
    })
}

fn project_world_point(
    point: Vec3,
    camera: &Camera,
    camera_transform: &GlobalTransform,
) -> Option<[f32; 2]> {
    let mut screen = camera.world_to_viewport(camera_transform, point).ok()?;
    if let Some(rect) = camera.logical_viewport_rect() {
        screen += rect.min;
    }
    Some(vec2_array(screen))
}

fn bounds3(bounds: crate::authored_entity::EntityBounds) -> UxBounds3 {
    UxBounds3 {
        min: bounds.min.into(),
        max: bounds.max.into(),
    }
}

fn vec2_array(value: Vec2) -> [f32; 2] {
    [value.x, value.y]
}
