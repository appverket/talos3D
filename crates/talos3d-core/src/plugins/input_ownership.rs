use bevy::prelude::*;

use super::{
    egui_chrome::{ChromeInputCapture, EguiChromeSystems, EguiWantsInput},
    face_edit::PushPullContext,
    handles::HandleInteractionState,
    palette::PaletteState,
    property_edit::PropertyEditState,
    transform::TransformState,
};

/// Central authority for who currently owns viewport input.
///
/// Every input-handling system checks this resource instead of ad-hoc
/// guard combinations.  When a modal operation is active (transform,
/// push/pull, handle drag), only the modal's own systems process input;
/// all other systems (selection, tools, handles) are locked out.
///
/// GUI integration contract for new systems:
///
/// - Egui/chrome producers publish aggregate claims only. Use
///   `bevy_egui::EguiWantsInput` for ordinary egui state and
///   `ChromeInputCapture` only for Talos3D chrome semantics that egui does not
///   model, such as the viewport context menu and same-drag release ownership.
/// - Viewport consumers must read `InputOwnership`, not rescan egui layers,
///   widget rects, command menus, or raw pointer state. That keeps ownership
///   checks O(1) regardless of model size or UI complexity.
/// - Register viewport systems in `InputPhase`. Tool/selection systems should
///   require `is_idle()`, modal systems should require their `ModalKind`, and
///   camera/cursor-style systems that remain usable during modals should only
///   stop on `is_ui_capture()`.
/// - Do not enable `bevy_egui`'s global input absorption for Talos3D viewport
///   logic. It clears shared Bevy input buffers and can starve non-egui systems;
///   this resource is the explicit arbitration boundary.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub enum InputOwnership {
    /// No modal operation — selection, tool activation, handle hover, and
    /// camera navigation are all available.
    #[default]
    Idle,
    /// A modal operator exclusively owns keyboard + left-mouse input.
    /// Camera navigation (middle/right mouse, Alt-orbit, Space-pan, scroll-zoom)
    /// remains available.
    Modal(ModalKind),
    /// The egui UI has focus (palette, property editing, text fields).
    /// Nothing in the 3D viewport processes input.
    UiCapture,
}

/// Which modal operation is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalKind {
    /// Generic transform: move (G), rotate (R), scale (S).
    Transform,
    /// Face push/pull — a transform constrained to the face normal.
    PushPull,
    /// Dragging an authored property handle.
    HandleDrag,
}

impl InputOwnership {
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn is_modal(&self) -> bool {
        matches!(self, Self::Modal(_))
    }

    pub fn is_ui_capture(&self) -> bool {
        matches!(self, Self::UiCapture)
    }
}

/// Ordered phases for input processing each frame.
///
/// Systems are assigned to these sets and the sets are chained so that
/// higher-priority consumers (UI, modal operators) run before lower-
/// priority ones (tool input, camera navigation).  A system that claims
/// an event in an earlier phase prevents later phases from seeing it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum InputPhase {
    /// Derive `InputOwnership` from existing state (runs first).
    SyncOwnership,
    /// Modal operator input — transform confirm/cancel, axis switching,
    /// numeric entry.  Only runs when `InputOwnership::Modal(_)`.
    ModalInput,
    /// Gizmo handle hover, click, and drag initiation.
    HandleInput,
    /// Active tool input — selection click, box select, face click,
    /// tool activation, and keyboard shortcuts that start new modals.
    ToolInput,
    /// Camera orbit / pan / zoom. Pan: middle-drag, Space+drag, or
    /// Shift+right-drag. Orbit: Alt+drag (or three-finger trackpad). Zoom: scroll.
    CameraInput,
}

pub struct InputOwnershipPlugin;

impl Plugin for InputOwnershipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChromeInputCapture>()
            .init_resource::<InputOwnership>()
            .configure_sets(
                Update,
                (
                    InputPhase::SyncOwnership,
                    InputPhase::ModalInput,
                    InputPhase::HandleInput,
                    InputPhase::ToolInput,
                    InputPhase::CameraInput,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                sync_input_ownership
                    .in_set(InputPhase::SyncOwnership)
                    .after(EguiChromeSystems),
            );
    }
}

/// Derives `InputOwnership` from existing state resources each frame.
///
/// This is the bridge for incremental migration: old state machines
/// (`TransformState`, `PaletteState`, etc.) still drive behaviour, and
/// this system translates their state into the single `InputOwnership`
/// resource that new code checks.
fn sync_input_ownership(
    mut ownership: ResMut<InputOwnership>,
    chrome_capture: Res<ChromeInputCapture>,
    egui_wants: Res<EguiWantsInput>,
    palette: Res<PaletteState>,
    property_edit: Res<PropertyEditState>,
    transform: Res<TransformState>,
    push_pull: Res<PushPullContext>,
    handle_state: Res<HandleInteractionState>,
) {
    let new_ownership = if palette.is_open()
        || property_edit.is_active()
        || chrome_capture.wants_any_keyboard_input()
        || chrome_capture.wants_any_pointer_input()
        || egui_wants.wants_any_keyboard_input()
        || egui_wants.wants_any_pointer_input()
    {
        InputOwnership::UiCapture
    } else if handle_state.property_drag_active() {
        InputOwnership::Modal(ModalKind::HandleDrag)
    } else if !transform.is_idle() {
        if push_pull.active_face.is_some() {
            InputOwnership::Modal(ModalKind::PushPull)
        } else {
            InputOwnership::Modal(ModalKind::Transform)
        }
    } else {
        InputOwnership::Idle
    };

    if *ownership != new_ownership {
        *ownership = new_ownership;
    }
}
