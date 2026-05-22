//! Unified keyboard map: the single source of truth that turns command
//! descriptors into dispatchable keyboard shortcuts.
//!
//! Every command/tool shortcut is declared once, in its [`CommandDescriptor`]'s
//! `default_shortcut` field. That same string drives three things that used to
//! drift apart:
//!
//! * **Display** — tooltips, menus and the palette render `default_shortcut`.
//! * **Dispatch** — [`dispatch_command_shortcuts`] parses it and invokes the
//!   command. There are no longer per-plugin `KeyCode` handlers competing for
//!   the same key.
//! * **Conflict detection** — [`install_keymap`] parses every descriptor at
//!   startup and refuses to boot (debug) / logs an error (release) if two
//!   commands claim the same chord. A clash can no longer be introduced
//!   silently: add a duplicate and the app fails fast.
//!
//! Modal interaction keys (Esc/Enter/Delete/Tab/arrows) are intentionally *not*
//! commands — they are owned by the active modal system and are excluded from
//! both dispatch and conflict detection.

use std::collections::HashMap;

use bevy::prelude::*;
use serde_json::Value;

use crate::plugins::{
    command_registry::{
        queue_command_invocation_resource, CommandDescriptor, CommandRegistry,
        PendingCommandInvocations,
    },
    egui_chrome::{all_menu_command_ids, EguiWantsInput},
    face_edit::FaceEditContext,
    input_ownership::InputOwnership,
    toolbar::ToolbarRegistry,
    tools::ActiveTool,
    transform::TransformState,
};

/// A normalized keyboard chord: one primary key plus modifier flags.
/// `primary` is Cmd on macOS and Ctrl elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: KeyCode,
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Where a shortcut is allowed to fire. Derived deterministically from the
/// descriptor (see [`context_for`]) so dispatch and conflict detection always
/// agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutContext {
    /// Fires whenever input is idle, regardless of the active tool
    /// (undo, save, group, zoom, ...).
    Global,
    /// Activates a tool; fires whenever input is idle, in any tool.
    Tool,
    /// Transform shortcuts (move/rotate/scale): only while the Select tool is
    /// active and no transform is already running.
    Select,
    /// Handled by an owning modal system (Esc/Enter/Delete/...). Never
    /// dispatched here, and excluded from conflict detection.
    Modal,
}

/// Keys that belong to modal interaction grammar rather than the command
/// system. Listing one of these in a `default_shortcut` is purely
/// documentation; the owning system handles the actual press.
const MODAL_KEYS: &[KeyCode] = &[
    KeyCode::Escape,
    KeyCode::Enter,
    KeyCode::NumpadEnter,
    KeyCode::Delete,
    KeyCode::Backspace,
    KeyCode::NumpadBackspace,
    KeyCode::Tab,
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
];

#[derive(Clone)]
struct Binding {
    chord: KeyChord,
    context: ShortcutContext,
    command_id: String,
}

/// Resolved keyboard map built from the [`CommandRegistry`] at startup.
#[derive(Resource, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

/// A detected clash between two commands claiming the same dispatchable chord.
#[derive(Debug, Clone)]
pub struct ShortcutConflict {
    pub chord: KeyChord,
    pub first: String,
    pub second: String,
}

impl std::fmt::Display for ShortcutConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` and `{}` both bind {:?}",
            self.first, self.second, self.chord
        )
    }
}

/// Register the keymap systems on the app. Called from `CommandRegistryPlugin`
/// after all commands are registered.
pub fn register(app: &mut App) {
    app.init_resource::<Keymap>()
        .add_systems(Startup, install_keymap);
}

fn letter_key(c: char) -> Option<KeyCode> {
    Some(match c.to_ascii_uppercase() {
        'A' => KeyCode::KeyA,
        'B' => KeyCode::KeyB,
        'C' => KeyCode::KeyC,
        'D' => KeyCode::KeyD,
        'E' => KeyCode::KeyE,
        'F' => KeyCode::KeyF,
        'G' => KeyCode::KeyG,
        'H' => KeyCode::KeyH,
        'I' => KeyCode::KeyI,
        'J' => KeyCode::KeyJ,
        'K' => KeyCode::KeyK,
        'L' => KeyCode::KeyL,
        'M' => KeyCode::KeyM,
        'N' => KeyCode::KeyN,
        'O' => KeyCode::KeyO,
        'P' => KeyCode::KeyP,
        'Q' => KeyCode::KeyQ,
        'R' => KeyCode::KeyR,
        'S' => KeyCode::KeyS,
        'T' => KeyCode::KeyT,
        'U' => KeyCode::KeyU,
        'V' => KeyCode::KeyV,
        'W' => KeyCode::KeyW,
        'X' => KeyCode::KeyX,
        'Y' => KeyCode::KeyY,
        'Z' => KeyCode::KeyZ,
        _ => return None,
    })
}

fn is_letter_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KeyA
            | KeyCode::KeyB
            | KeyCode::KeyC
            | KeyCode::KeyD
            | KeyCode::KeyE
            | KeyCode::KeyF
            | KeyCode::KeyG
            | KeyCode::KeyH
            | KeyCode::KeyI
            | KeyCode::KeyJ
            | KeyCode::KeyK
            | KeyCode::KeyL
            | KeyCode::KeyM
            | KeyCode::KeyN
            | KeyCode::KeyO
            | KeyCode::KeyP
            | KeyCode::KeyQ
            | KeyCode::KeyR
            | KeyCode::KeyS
            | KeyCode::KeyT
            | KeyCode::KeyU
            | KeyCode::KeyV
            | KeyCode::KeyW
            | KeyCode::KeyX
            | KeyCode::KeyY
            | KeyCode::KeyZ
    )
}

fn parse_key(token: &str) -> Option<KeyCode> {
    let token = token.trim();
    if token.chars().count() == 1 {
        return letter_key(token.chars().next().unwrap());
    }
    Some(match token {
        "Esc" | "Escape" => KeyCode::Escape,
        "Enter" | "Return" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "Delete" | "Del" => KeyCode::Delete,
        "Backspace" => KeyCode::Backspace,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Space" => KeyCode::Space,
        _ => return None,
    })
}

fn parse_chord(spec: &str) -> Option<KeyChord> {
    let mut primary = false;
    let mut shift = false;
    let mut alt = false;
    let mut key = None;
    for part in spec.split('+') {
        match part.trim() {
            "Ctrl/Cmd" | "Cmd" | "Ctrl" | "Super" | "Control" | "Command" => primary = true,
            "Shift" => shift = true,
            "Alt" | "Option" => alt = true,
            other => key = parse_key(other),
        }
    }
    Some(KeyChord {
        key: key?,
        primary,
        shift,
        alt,
    })
}

/// Parse a `default_shortcut` string into its chords. Multiple alternative
/// chords are comma-separated (e.g. `"Esc, Ctrl/Cmd+D"`).
pub fn parse_shortcut(spec: &str) -> Vec<KeyChord> {
    spec.split(',').filter_map(parse_chord).collect()
}

/// Deterministically classify a chord's [`ShortcutContext`] from its
/// descriptor. Shared by dispatch and conflict detection so they never
/// disagree.
fn context_for(descriptor: &CommandDescriptor, chord: &KeyChord) -> ShortcutContext {
    if MODAL_KEYS.contains(&chord.key) {
        return ShortcutContext::Modal;
    }
    let has_modifier = chord.primary || chord.shift || chord.alt;
    if has_modifier {
        ShortcutContext::Global
    } else if descriptor.activates_tool.is_some() {
        ShortcutContext::Tool
    } else if is_letter_key(chord.key) {
        ShortcutContext::Select
    } else {
        ShortcutContext::Global
    }
}

fn descriptor_bindings(descriptor: &CommandDescriptor) -> Vec<Binding> {
    let Some(spec) = descriptor.default_shortcut.as_deref() else {
        return Vec::new();
    };
    parse_shortcut(spec)
        .into_iter()
        .map(|chord| Binding {
            chord,
            context: context_for(descriptor, &chord),
            command_id: descriptor.id.clone(),
        })
        .collect()
}

fn collect_bindings(registry: &CommandRegistry) -> Vec<Binding> {
    registry.commands().flat_map(descriptor_bindings).collect()
}

/// Two dispatchable bindings conflict when they share a chord. The Select tool
/// in its idle state makes Global, Tool and Select contexts all live at once,
/// so any shared chord between distinct commands is a real ambiguity. Modal
/// bindings are excluded (they are not dispatched here).
fn detect_conflicts(bindings: &[Binding]) -> Vec<ShortcutConflict> {
    let mut seen: HashMap<KeyChord, &str> = HashMap::new();
    let mut conflicts = Vec::new();
    for binding in bindings
        .iter()
        .filter(|binding| binding.context != ShortcutContext::Modal)
    {
        match seen.get(&binding.chord) {
            Some(first) if *first != binding.command_id => conflicts.push(ShortcutConflict {
                chord: binding.chord,
                first: (*first).to_string(),
                second: binding.command_id.clone(),
            }),
            Some(_) => {}
            None => {
                seen.insert(binding.chord, &binding.command_id);
            }
        }
    }
    conflicts
}

/// Integrity problems that are meaningful in ANY world: duplicate command IDs
/// and conflicting shortcuts. These are computed only from registered commands,
/// so a partially-assembled world (e.g. a unit test) never yields false
/// positives. Pure, so it is unit-testable.
pub(crate) fn intrinsic_problems(registry: &CommandRegistry) -> Vec<String> {
    let mut problems = Vec::new();
    for id in registry.duplicate_ids() {
        problems.push(format!("command id `{id}` is registered more than once"));
    }
    let bindings = collect_bindings(registry);
    for conflict in detect_conflicts(&bindings) {
        problems.push(format!("shortcut conflict: {conflict}"));
    }
    problems
}

/// Reference-integrity problems: toolbar/menu items that point at a command id
/// that is not registered. Only valid against a fully-assembled registry, so the
/// caller gates this on the app being complete.
pub(crate) fn reference_problems(
    registry: &CommandRegistry,
    toolbars: &ToolbarRegistry,
    menu_command_ids: &[&str],
) -> Vec<String> {
    let mut problems = Vec::new();
    for toolbar in toolbars.toolbars() {
        for section in &toolbar.sections {
            for command_id in &section.command_ids {
                if registry.get(command_id).is_none() {
                    problems.push(format!(
                        "toolbar `{}` references unknown command `{command_id}`",
                        toolbar.id
                    ));
                }
            }
        }
    }
    for command_id in menu_command_ids {
        if registry.get(command_id).is_none() {
            problems.push(format!("menu references unknown command `{command_id}`"));
        }
    }
    problems
}

fn install_keymap(world: &mut World) {
    // Uniqueness of command IDs and shortcuts is the hard guarantee: fail fast in
    // debug so a clash can never ship, log loudly in release.
    let intrinsic = intrinsic_problems(world.resource::<CommandRegistry>());
    if !intrinsic.is_empty() {
        let report = intrinsic.join("; ");
        if cfg!(debug_assertions) {
            panic!("Command/shortcut uniqueness violated: {report}");
        } else {
            error!("Command/shortcut uniqueness violated: {report}");
        }
    }

    // Reference integrity (toolbar/menu items point at real commands) needs the
    // full plugin set; ToolbarRegistry presence signals the app is assembled (it
    // is absent in unit-test worlds that mount only the command registry). This
    // is reported loudly but is non-fatal, so an unusual plugin combination can
    // never brick startup.
    if world.get_resource::<ToolbarRegistry>().is_some() {
        let menu_command_ids = all_menu_command_ids();
        let registry = world.resource::<CommandRegistry>();
        let toolbars = world.resource::<ToolbarRegistry>();
        let references = reference_problems(registry, toolbars, &menu_command_ids);
        if !references.is_empty() {
            error!(
                "Toolbar/menu reference integrity: {}",
                references.join("; ")
            );
        }
    }

    let bindings = collect_bindings(world.resource::<CommandRegistry>());
    world.insert_resource(Keymap { bindings });
}

fn primary_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    }
}

/// The single dispatch path for command/tool keyboard shortcuts. Reads the
/// resolved [`Keymap`], matches the pressed chord against the current context,
/// and queues the command for execution.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_command_shortcuts(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    keymap: Res<Keymap>,
    egui_wants_input: Option<Res<EguiWantsInput>>,
    ownership: Option<Res<InputOwnership>>,
    active_tool: Option<Res<State<ActiveTool>>>,
    transform_state: Option<Res<TransformState>>,
    face_edit_context: Option<Res<FaceEditContext>>,
    mut pending: ResMut<PendingCommandInvocations>,
) {
    let Some(keys) = keys else {
        return;
    };
    if egui_wants_input.map(|e| e.keyboard).unwrap_or(false) {
        return;
    }
    if !ownership.map(|o| o.is_idle()).unwrap_or(true) {
        return;
    }

    let primary = primary_pressed(&keys);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    let in_select = active_tool
        .map(|tool| *tool.get() == ActiveTool::Select)
        .unwrap_or(true);
    let transform_idle = transform_state.map(|state| state.is_idle()).unwrap_or(true);
    let face_editing = face_edit_context
        .map(|context| context.is_active())
        .unwrap_or(false);

    for binding in &keymap.bindings {
        let chord = binding.chord;
        if !keys.just_pressed(chord.key) {
            continue;
        }
        if chord.primary != primary || chord.shift != shift || chord.alt != alt {
            continue;
        }
        let allowed = match binding.context {
            ShortcutContext::Global | ShortcutContext::Tool => true,
            ShortcutContext::Select => in_select && transform_idle && !face_editing,
            ShortcutContext::Modal => false,
        };
        if allowed {
            queue_command_invocation_resource(
                &mut pending,
                binding.command_id.clone(),
                Value::Null,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: &str, shortcut: &str, activates_tool: Option<&str>) -> CommandDescriptor {
        use crate::plugins::command_registry::CommandCategory;
        CommandDescriptor {
            id: id.to_string(),
            label: id.to_string(),
            description: String::new(),
            category: CommandCategory::Edit,
            parameters: None,
            version: 1,
            default_shortcut: Some(shortcut.to_string()),
            icon: None,
            hint: None,
            requires_selection: false,
            show_in_menu: true,
            activates_tool: activates_tool.map(str::to_string),
            capability_id: None,
        }
    }

    #[test]
    fn parses_modifier_chords() {
        assert_eq!(
            parse_shortcut("Ctrl/Cmd+Shift+Z"),
            vec![KeyChord {
                key: KeyCode::KeyZ,
                primary: true,
                shift: true,
                alt: false,
            }]
        );
        assert_eq!(
            parse_shortcut("Esc, Ctrl/Cmd+D"),
            vec![
                KeyChord {
                    key: KeyCode::Escape,
                    primary: false,
                    shift: false,
                    alt: false,
                },
                KeyChord {
                    key: KeyCode::KeyD,
                    primary: true,
                    shift: false,
                    alt: false,
                },
            ]
        );
    }

    #[test]
    fn classifies_contexts() {
        let tool = descriptor("guide", "G", Some("PlaceGuideLine"));
        let mv = descriptor("move", "M", None);
        let global = descriptor("undo", "Ctrl/Cmd+Z", None);
        let modal = descriptor("delete", "Delete", None);
        let zoom = descriptor("zoom", "Home", None);

        assert_eq!(descriptor_bindings(&tool)[0].context, ShortcutContext::Tool);
        assert_eq!(descriptor_bindings(&mv)[0].context, ShortcutContext::Select);
        assert_eq!(
            descriptor_bindings(&global)[0].context,
            ShortcutContext::Global
        );
        assert_eq!(
            descriptor_bindings(&modal)[0].context,
            ShortcutContext::Modal
        );
        assert_eq!(
            descriptor_bindings(&zoom)[0].context,
            ShortcutContext::Global
        );
    }

    #[test]
    fn move_on_g_clashes_with_guide_on_g() {
        let bindings: Vec<Binding> = [
            descriptor("modeling.move", "G", None),
            descriptor("guide_lines.place", "G", Some("PlaceGuideLine")),
        ]
        .iter()
        .flat_map(descriptor_bindings)
        .collect();
        assert_eq!(detect_conflicts(&bindings).len(), 1);
    }

    #[test]
    fn move_on_m_has_no_clash_with_guide_on_g() {
        let bindings: Vec<Binding> = [
            descriptor("modeling.move", "M", None),
            descriptor("guide_lines.place", "G", Some("PlaceGuideLine")),
        ]
        .iter()
        .flat_map(descriptor_bindings)
        .collect();
        assert!(detect_conflicts(&bindings).is_empty());
    }

    #[test]
    fn reference_problems_flag_unknown_command_ids() {
        use crate::plugins::toolbar::{
            ToolbarDescriptor, ToolbarDock, ToolbarRegistry, ToolbarSection,
        };
        let registry = CommandRegistry::default(); // empty: nothing is registered
        let mut toolbars = ToolbarRegistry::default();
        toolbars.register(ToolbarDescriptor {
            id: "core".to_string(),
            label: "Core".to_string(),
            default_dock: ToolbarDock::Top,
            default_visible: true,
            sections: vec![ToolbarSection {
                label: "X".to_string(),
                command_ids: vec!["core.undo".to_string()],
            }],
        });

        let problems = reference_problems(&registry, &toolbars, &["modeling.move"]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("core.undo")));
        assert!(problems.iter().any(|p| p.contains("modeling.move")));
    }

    #[test]
    fn menu_command_ids_are_unique() {
        let ids = all_menu_command_ids();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            ids.len(),
            sorted.len(),
            "menu defines a duplicate command id"
        );
        assert!(!ids.is_empty());
    }

    #[test]
    fn modifier_distinguishes_chords() {
        // Plain D (tool), Cmd+D (deselect) and Cmd+Shift+D (definitions) must
        // not be treated as conflicts.
        let bindings: Vec<Binding> = [
            descriptor("dim", "D", Some("PlaceDimensionLine")),
            descriptor("deselect", "Ctrl/Cmd+D", None),
            descriptor("definitions", "Ctrl/Cmd+Shift+D", None),
        ]
        .iter()
        .flat_map(descriptor_bindings)
        .collect();
        assert!(detect_conflicts(&bindings).is_empty());
    }
}
