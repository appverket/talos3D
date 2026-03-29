use bevy::prelude::*;

use super::{
    egui_chrome::EguiWantsInput, face_edit::PushPullContext, handles::HandleInteractionState,
    palette::PaletteState, property_edit::PropertyEditState, transform::TransformState,
};

/// Central authority for who currently owns viewport input.
///
/// Every input-handling system checks this resource instead of ad-hoc
/// guard combinations.  When a modal operation is active (transform,
/// push/pull, handle drag), only the modal's own systems process input;
/// all other systems (selection, tools, handles) are locked out.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub enum InputOwnership {
    /// No modal operation — selection, tool activation, handle hover, and
    /// camera navigation are all available.
    #[default]
    Idle,
    /// A modal operator exclusively owns keyboard + left-mouse input.
    /// Camera navigation (right/middle mouse) remains available.
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
    /// Camera orbit / pan / zoom (always available via right/middle mouse).
    CameraInput,
}

pub struct InputOwnershipPlugin;

impl Plugin for InputOwnershipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputOwnership>()
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
                sync_input_ownership.in_set(InputPhase::SyncOwnership),
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
    egui_wants: Res<EguiWantsInput>,
    palette: Res<PaletteState>,
    property_edit: Res<PropertyEditState>,
    transform: Res<TransformState>,
    push_pull: Res<PushPullContext>,
    handle_state: Res<HandleInteractionState>,
) {
    let new_ownership = if palette.is_open() || property_edit.is_active() || egui_wants.keyboard {
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
