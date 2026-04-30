use std::collections::HashMap;
use std::f32::consts::PI;
#[cfg(feature = "perf-stats")]
use std::time::Instant;

use bevy::{
    ecs::{system::SystemParam, world::EntityRef},
    prelude::*,
    window::PrimaryWindow,
};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, add_transform_preview_time, PerfStats};
use crate::{
    authored_entity::AuthoredEntity,
    authored_entity::BoxedEntity,
    capability_registry::CapabilityRegistry,
    plugins::{
        commands::{ApplyEntityChangesCommand, CreateEntityCommand},
        cursor::CursorWorldPos,
        face_edit::{FaceEditContext, PushPullContext, PushPullFace},
        handles::arm_move_handles,
        identity::ElementId,
        inference::InferenceEngine,
        input_ownership::{InputOwnership, InputPhase, ModalKind},
        modeling::{
            bsp_csg::BooleanOp,
            csg::{CsgNode, CsgParentMap, CsgSnapshot},
            generic_snapshot::PrimitiveSnapshot,
            group::GroupMembers,
            profile::ProfileExtrusion,
        },
        selection::Selected,
        snap::SnapResult,
        tools::ActiveTool,
        ui::StatusBarData,
    },
};

const ROTATION_SNAP_INCREMENT_RADIANS: f32 = PI / 12.0;
const AXIS_LINE_LENGTH_METRES: f32 = 50.0;
const AXIS_X_COLOR: Color = Color::srgb(0.95, 0.3, 0.3);
const AXIS_Y_COLOR: Color = Color::srgb(0.35, 0.9, 0.35);
const AXIS_Z_COLOR: Color = Color::srgb(0.35, 0.55, 0.95);
const TRANSFORM_PREVIEW_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);
const FEATURE_PUSH_PULL_EPSILON: f32 = 0.001;

pub struct TransformPlugin;

impl Plugin for TransformPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TransformState>()
            .init_resource::<ActiveTransformPreview>()
            .init_resource::<PivotPoint>()
            .configure_sets(
                Update,
                TransformVisualSystems::PreviewUpdate.before(TransformVisualSystems::PreviewDraw),
            )
            .configure_sets(
                Update,
                TransformVisualSystems::PreviewDraw.before(TransformVisualSystems::ConstraintDraw),
            )
            .add_systems(
                Update,
                (
                    begin_move
                        .in_set(TransformShortcuts)
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    begin_rotate
                        .in_set(TransformShortcuts)
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    begin_scale
                        .in_set(TransformShortcuts)
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    activate_pending_transform
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    handle_transform_input
                        .in_set(InputPhase::ModalInput)
                        .run_if(in_state(ActiveTool::Select)),
                    arm_scale_drag_on_mouse_press
                        .in_set(InputPhase::ModalInput)
                        .before(update_transform_preview)
                        .before(confirm_transform)
                        .run_if(in_state(ActiveTool::Select)),
                    update_transform_preview
                        .in_set(TransformVisualSystems::PreviewUpdate)
                        .run_if(in_state(ActiveTool::Select)),
                    confirm_transform
                        .in_set(InputPhase::ModalInput)
                        .run_if(in_state(ActiveTool::Select)),
                    cancel_transform
                        .in_set(InputPhase::ModalInput)
                        .run_if(in_state(ActiveTool::Select)),
                    update_transform_status
                        .in_set(InputPhase::ModalInput)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_transform_preview
                        .in_set(TransformVisualSystems::PreviewDraw)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_transform_constraint
                        .in_set(TransformVisualSystems::ConstraintDraw)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_rotation_protractor
                        .in_set(TransformVisualSystems::ConstraintDraw)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_scale_guides
                        .in_set(TransformVisualSystems::ConstraintDraw)
                        .run_if(in_state(ActiveTool::Select)),
                ),
            );
    }
}

#[derive(Resource, Default, Clone)]
pub struct ActiveTransformPreview {
    pub snapshots: Vec<BoxedEntity>,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransformVisualSystems {
    PreviewUpdate,
    PreviewDraw,
    ConstraintDraw,
}

/// System set for the generic G/R/S transform shortcut systems.
/// Face-edit push/pull is ordered `.before(TransformShortcuts)` so it can
/// consume the G key before `begin_move` sees it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransformShortcuts;

#[derive(Resource, Default, Clone)]
pub struct TransformState {
    pub mode: TransformMode,
    pub axis: AxisConstraint,
    pub numeric_buffer: Option<String>,
    pub initial_cursor: Option<Vec3>,
    pub initial_snapshots: Vec<(Entity, BoxedEntity)>,
    pub preview_entities: Vec<(ElementId, Entity)>,
    pub pivot_override: Option<Vec3>,
    pub confirm_on_release: bool,
    /// When set, the transform activates as soon as the cursor enters the viewport.
    pub pending_mode: Option<TransformMode>,
}

impl TransformState {
    /// Returns true when no transform operation is actively running.
    /// Note: pending state (waiting for cursor to enter viewport) is still considered idle
    /// so that handles and visual feedback remain visible.
    pub fn is_idle(&self) -> bool {
        self.mode == TransformMode::Idle
    }

    pub fn is_pending(&self) -> bool {
        self.pending_mode.is_some()
    }

    pub fn clear(&mut self) {
        self.mode = TransformMode::Idle;
        self.axis = AxisConstraint::None;
        self.numeric_buffer = None;
        self.initial_cursor = None;
        self.initial_snapshots.clear();
        self.preview_entities.clear();
        self.pivot_override = None;
        self.confirm_on_release = false;
        self.pending_mode = None;
    }
}

/// The pivot point is the center of rotation and scale operations.
/// Click a handle to set it; it resets when the selection changes.
/// When not set, defaults to the center of the selection.
#[derive(Resource, Default, Clone, Copy)]
pub struct PivotPoint {
    pub position: Option<Vec3>,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum TransformMode {
    #[default]
    Idle,
    Moving,
    Rotating,
    Scaling,
}

#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub enum AxisConstraint {
    #[default]
    None,
    X,
    Y,
    Z,
    PlaneYZ,
    PlaneXZ,
    PlaneXY,
    /// Constrain movement to an arbitrary direction (e.g., face normal for push/pull).
    Custom(Vec3),
}

#[derive(Clone)]
struct TransformPreview {
    after: Vec<BoxedEntity>,
    display_value: Option<f32>,
}

#[derive(Clone, Copy, Default)]
pub struct TransformStartOptions {
    pub axis: AxisConstraint,
    pub initial_cursor: Option<Vec3>,
    pub pivot_override: Option<Vec3>,
    pub confirm_on_release: bool,
}

fn begin_move(world: &mut World) {
    begin_move_shortcut(world, KeyCode::KeyG);
}

fn begin_move_shortcut(world: &mut World, key: KeyCode) {
    if !world.resource::<InputOwnership>().is_idle() {
        return;
    }

    if world.resource::<FaceEditContext>().is_active() {
        return;
    }

    let keys = world.resource::<ButtonInput<KeyCode>>();
    let key_pressed = keys.just_pressed(key);
    let has_modifier = keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::AltLeft)
        || keys.pressed(KeyCode::AltRight);
    if !key_pressed || has_modifier {
        return;
    }

    if world.resource::<TransformState>().is_idle() {
        let _ = arm_move_handles(world);
    }
}

fn begin_rotate(world: &mut World) {
    begin_transform_shortcut(world, KeyCode::KeyR, TransformMode::Rotating);
}

fn begin_scale(world: &mut World) {
    begin_transform_shortcut(world, KeyCode::KeyS, TransformMode::Scaling);
}

fn begin_transform_shortcut(world: &mut World, key: KeyCode, mode: TransformMode) {
    if !world.resource::<InputOwnership>().is_idle() {
        return;
    }

    // In face edit mode, G is reserved for push/pull — never start a generic move.
    // This pairs with the `.before(TransformShortcuts)` ordering on push/pull.
    if key == KeyCode::KeyG && world.resource::<FaceEditContext>().is_active() {
        return;
    }

    let keys = world.resource::<ButtonInput<KeyCode>>();
    let key_pressed = keys.just_pressed(key);
    let has_modifier = keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::AltLeft)
        || keys.pressed(KeyCode::AltRight);
    if !key_pressed || has_modifier {
        return;
    }

    // Allow switching pending mode even if already pending
    let state = world.resource::<TransformState>();
    if state.is_pending() {
        world.resource_mut::<TransformState>().pending_mode = Some(mode);
        return;
    }
    if !state.is_idle() {
        return;
    }

    match start_transform_mode(world, mode) {
        Ok(()) => {}
        Err(_) => {
            // Cursor not over viewport — enter pending state so transform
            // activates as soon as the cursor enters the viewport.
            world.resource_mut::<TransformState>().pending_mode = Some(mode);
        }
    }
}

pub fn start_transform_mode(world: &mut World, mode: TransformMode) -> Result<(), String> {
    start_transform_mode_with_options(world, mode, TransformStartOptions::default())
}

pub fn start_transform_mode_with_options(
    world: &mut World,
    mode: TransformMode,
    options: TransformStartOptions,
) -> Result<(), String> {
    if !world.resource::<InputOwnership>().is_idle() {
        return Err("Input is owned by another system".to_string());
    }
    if !world.resource::<TransformState>().is_idle() {
        return Err("A transform is already active".to_string());
    }

    let initial_snapshots = collect_selected_snapshots(world);
    if initial_snapshots.is_empty() {
        return Err("No selection to transform".to_string());
    }

    let Some(initial_cursor) = options
        .initial_cursor
        .or_else(|| current_transform_cursor(world))
    else {
        return Err("Cursor is not over the modeling viewport".to_string());
    };

    let mut transform_state = world.resource_mut::<TransformState>();
    transform_state.mode = mode;
    transform_state.axis = options.axis;
    transform_state.numeric_buffer = None;
    transform_state.initial_cursor = Some(initial_cursor);
    transform_state.initial_snapshots = initial_snapshots;
    transform_state.pivot_override = options.pivot_override;
    transform_state.confirm_on_release = options.confirm_on_release;

    // Immediately set InputOwnership so other systems in this same frame
    // see Modal and don't process stale input events.
    *world.resource_mut::<InputOwnership>() = InputOwnership::Modal(ModalKind::Transform);

    Ok(())
}

fn activate_pending_transform(world: &mut World) {
    let pending_mode = world.resource::<TransformState>().pending_mode;
    let Some(mode) = pending_mode else {
        return;
    };

    // Cancel pending on Escape
    if world
        .resource::<ButtonInput<KeyCode>>()
        .just_pressed(KeyCode::Escape)
    {
        world.resource_mut::<TransformState>().clear();
        world.resource_mut::<StatusBarData>().hint.clear();
        return;
    }

    // Try to activate — this succeeds once the cursor is over the viewport
    if current_transform_cursor(world).is_some() {
        world.resource_mut::<TransformState>().pending_mode = None;
        let _ = start_transform_mode(world, mode);
    } else {
        // Show pending status
        let mode_label = match mode {
            TransformMode::Moving => "Move",
            TransformMode::Rotating => "Rotate",
            TransformMode::Scaling => "Scale",
            TransformMode::Idle => return,
        };
        world.resource_mut::<StatusBarData>().hint =
            format!("{mode_label}: move cursor to viewport · Esc cancel");
    }
}

fn handle_transform_input(world: &mut World) {
    if !world.resource::<InputOwnership>().is_modal() {
        return;
    }

    let keys = world.resource::<ButtonInput<KeyCode>>();
    let shift_pressed = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let just_pressed: Vec<KeyCode> = keys.get_just_pressed().copied().collect();
    let _ = keys;

    let mut transform_state = world.resource_mut::<TransformState>();

    for key in just_pressed {
        match key {
            KeyCode::KeyX => {
                transform_state.axis = if shift_pressed {
                    AxisConstraint::PlaneYZ
                } else {
                    toggle_axis(transform_state.axis, AxisConstraint::X)
                };
            }
            KeyCode::KeyY => {
                transform_state.axis = if shift_pressed {
                    AxisConstraint::PlaneXZ
                } else {
                    toggle_axis(transform_state.axis, AxisConstraint::Y)
                };
            }
            KeyCode::KeyZ => {
                transform_state.axis = if shift_pressed {
                    AxisConstraint::PlaneXY
                } else {
                    toggle_axis(transform_state.axis, AxisConstraint::Z)
                };
            }
            KeyCode::Digit0 | KeyCode::Numpad0 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '0')
            }
            KeyCode::Digit1 | KeyCode::Numpad1 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '1')
            }
            KeyCode::Digit2 | KeyCode::Numpad2 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '2')
            }
            KeyCode::Digit3 | KeyCode::Numpad3 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '3')
            }
            KeyCode::Digit4 | KeyCode::Numpad4 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '4')
            }
            KeyCode::Digit5 | KeyCode::Numpad5 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '5')
            }
            KeyCode::Digit6 | KeyCode::Numpad6 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '6')
            }
            KeyCode::Digit7 | KeyCode::Numpad7 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '7')
            }
            KeyCode::Digit8 | KeyCode::Numpad8 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '8')
            }
            KeyCode::Digit9 | KeyCode::Numpad9 => {
                push_numeric_char(&mut transform_state.numeric_buffer, '9')
            }
            KeyCode::Period | KeyCode::NumpadDecimal | KeyCode::NumpadComma => {
                if !transform_state
                    .numeric_buffer
                    .as_deref()
                    .unwrap_or_default()
                    .contains('.')
                {
                    push_numeric_char(&mut transform_state.numeric_buffer, '.');
                }
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                push_minus(&mut transform_state.numeric_buffer)
            }
            KeyCode::Backspace | KeyCode::NumpadBackspace => {
                pop_numeric_char(&mut transform_state.numeric_buffer)
            }
            _ => {}
        }
    }
}

fn update_transform_preview(world: &mut World) {
    #[cfg(feature = "perf-stats")]
    let start = Instant::now();
    let Some(preview) = compute_preview(world) else {
        world
            .resource_mut::<ActiveTransformPreview>()
            .snapshots
            .clear();
        #[cfg(feature = "perf-stats")]
        {
            let elapsed = start.elapsed();
            let mut perf_stats = world.resource_mut::<PerfStats>();
            add_transform_preview_time(&mut perf_stats, elapsed);
        }
        return;
    };

    world.resource_mut::<ActiveTransformPreview>().snapshots = preview.after.clone();

    let push_pull_face = world.resource::<PushPullContext>().active_face.clone();
    let is_push_pull = push_pull_face.is_some();
    let mode = world.resource::<TransformState>().mode;

    if is_push_pull {
        // Push/pull genuinely deforms geometry (face extrusion) — this cannot
        // be represented as a Transform change, so apply directly to the entity.
        let pairs: Vec<_> = world
            .resource::<TransformState>()
            .initial_snapshots
            .iter()
            .map(|(_, s)| s.clone())
            .zip(preview.after.iter().cloned())
            .collect();
        for (before, after) in &pairs {
            after.apply_with_previous(world, Some(before));
        }
        if let Some(pp) = push_pull_face {
            sync_feature_push_pull_preview(world, &pp, preview.display_value.unwrap_or(0.0));
        }
        return;
    }

    // For Move/Rotate/Scale: update only the entity's Transform component.
    // The geometry is never mutated — confirm/cancel simply restores the
    // original Transform, and the history system applies real changes.
    if matches!(
        mode,
        TransformMode::Moving | TransformMode::Rotating | TransformMode::Scaling
    ) {
        let is_scaling = mode == TransformMode::Scaling;
        let preview_transforms: Vec<_> = world
            .resource::<TransformState>()
            .initial_snapshots
            .iter()
            .zip(preview.after.iter())
            .filter_map(|((entity, before), after)| {
                let mut transform = after.preview_transform()?;
                if is_scaling {
                    if let Some(scale) = preview_scale(before, after) {
                        transform = transform.with_scale(scale);
                    }
                }
                Some((*entity, transform))
            })
            .collect();

        for (entity, transform) in preview_transforms {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(transform);
            }
        }
    }

    #[cfg(feature = "perf-stats")]
    {
        let elapsed = start.elapsed();
        let mut perf_stats = world.resource_mut::<PerfStats>();
        add_transform_preview_time(&mut perf_stats, elapsed);
    }
}

fn arm_scale_drag_on_mouse_press(world: &mut World) {
    if !world.resource::<InputOwnership>().is_modal() {
        return;
    }

    let mouse_buttons = world.resource::<ButtonInput<MouseButton>>();
    if !should_rebase_scale_drag(world.resource::<TransformState>(), mouse_buttons) {
        return;
    }

    let Some(cursor) = current_transform_cursor(world) else {
        return;
    };

    let mut transform_state = world.resource_mut::<TransformState>();
    transform_state.initial_cursor = Some(cursor);
    transform_state.confirm_on_release = true;
}

fn confirm_transform(world: &mut World) {
    if !world.resource::<InputOwnership>().is_modal() {
        return;
    }
    if world.resource::<TransformState>().is_idle() {
        return;
    }

    let confirm = {
        let mouse_buttons = world.resource::<ButtonInput<MouseButton>>();
        let keys = world.resource::<ButtonInput<KeyCode>>();
        should_confirm_transform(world.resource::<TransformState>(), mouse_buttons, keys)
    };
    if !confirm {
        return;
    }

    let Some(preview) = compute_preview(world) else {
        return;
    };

    let before = world
        .resource::<TransformState>()
        .initial_snapshots
        .iter()
        .map(|(_, snapshot)| snapshot.clone())
        .collect::<Vec<_>>();
    let label = if world.resource::<PushPullContext>().active_face.is_some() {
        "Push/Pull face"
    } else {
        transform_label(world.resource::<TransformState>().mode)
    };
    let after = preview.after;

    if before != after {
        world
            .resource_mut::<Messages<ApplyEntityChangesCommand>>()
            .write(ApplyEntityChangesCommand {
                label,
                before,
                after,
            });
    }

    // Store last distance for Tab-repeat (distance echo)
    if let Some(display_value) = preview.display_value {
        let mode = world.resource::<TransformState>().mode;
        let mut engine = world.resource_mut::<InferenceEngine>();
        engine.last_distance = Some(display_value);
        engine.last_mode = Some(mode);
    }

    // Capture push/pull info before cleanup
    let push_pull_face = world.resource::<PushPullContext>().active_face.clone();
    let push_pull_distance = preview.display_value;

    let was_push_pull = push_pull_face.is_some();

    // Push/pull mutates entity geometry live — restore from initial snapshot.
    if was_push_pull {
        let originals: Vec<_> = world
            .resource::<TransformState>()
            .initial_snapshots
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        for snapshot in &originals {
            snapshot.apply_to(world);
        }
    }

    // Move/Rotate/Scale only modify the entity's Transform — restore it.
    restore_preview_transforms(world);

    cleanup_preview_entities(world);
    world
        .resource_mut::<ActiveTransformPreview>()
        .snapshots
        .clear();
    world.resource_mut::<TransformState>().clear();
    world.resource_mut::<PushPullContext>().active_face = None;
    world.resource_mut::<StatusBarData>().hint.clear();

    // Finalize push/pull with authored boolean feature creation when needed.
    if let Some(pp) = &push_pull_face {
        let distance = push_pull_distance.unwrap_or(0.0);
        if let Some(parent_id) = pp.feature_parent {
            world
                .resource_mut::<CsgParentMap>()
                .parents
                .remove(&pp.element_id);

            if distance.abs() <= FEATURE_PUSH_PULL_EPSILON {
                if let Some(live_csg_id) = pp.live_csg {
                    teardown_live_csg(world, live_csg_id);
                }
            } else {
                let csg_snapshot = CsgSnapshot {
                    element_id: pp.live_csg.unwrap_or_else(|| {
                        world
                            .resource::<crate::plugins::identity::ElementIdAllocator>()
                            .next_id()
                    }),
                    csg_node: CsgNode {
                        operand_a: parent_id,
                        operand_b: pp.element_id,
                        op: if distance < 0.0 {
                            BooleanOp::Difference
                        } else {
                            BooleanOp::Union
                        },
                    },
                };
                world
                    .resource_mut::<Messages<CreateEntityCommand>>()
                    .write(CreateEntityCommand {
                        snapshot: csg_snapshot.into(),
                    });
                world.resource_mut::<FaceEditContext>().exit();
            }
        }
    }

    // After push/pull confirm, deselect the entity and stay in face edit mode
    if was_push_pull {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        let entities: Vec<Entity> = query.iter(world).collect();
        for entity in entities {
            world.entity_mut(entity).remove::<Selected>();
        }
    }
}

fn cancel_transform(world: &mut World) {
    if !world.resource::<InputOwnership>().is_modal() {
        return;
    }

    // Pending state is cancelled in activate_pending_transform
    if world.resource::<TransformState>().mode == TransformMode::Idle {
        return;
    }

    let cancel = world
        .resource::<ButtonInput<KeyCode>>()
        .just_pressed(KeyCode::Escape);
    if !cancel {
        return;
    }

    let push_pull_face = world.resource::<PushPullContext>().active_face.clone();
    let was_push_pull = push_pull_face.is_some();

    // Tear down live CSG on cancel
    if let Some(pp) = &push_pull_face {
        if let Some(live_csg_id) = pp.live_csg {
            teardown_live_csg(world, live_csg_id);
        }
    }

    // Push/pull mutates entity geometry live — restore from initial snapshot.
    if was_push_pull {
        let originals: Vec<_> = world
            .resource::<TransformState>()
            .initial_snapshots
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        for snapshot in &originals {
            snapshot.apply_to(world);
        }
    }

    // Move/Rotate/Scale only modify the entity's Transform — restore it.
    restore_preview_transforms(world);
    cleanup_preview_entities(world);
    world
        .resource_mut::<ActiveTransformPreview>()
        .snapshots
        .clear();
    world.resource_mut::<TransformState>().clear();
    world.resource_mut::<PushPullContext>().active_face = None;
    world.resource_mut::<StatusBarData>().hint.clear();

    // After push/pull cancel, deselect the entity and stay in face edit mode
    if was_push_pull {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        let entities: Vec<Entity> = query.iter(world).collect();
        for entity in entities {
            world.entity_mut(entity).remove::<Selected>();
        }
    }
}

/// Remove a live CSG node and restore its operands to visible, non-operand state.
fn teardown_live_csg(world: &mut World, csg_element_id: ElementId) {
    use crate::plugins::commands::find_entity_by_element_id;
    use crate::plugins::modeling::csg::CsgOperand;

    if let Some(csg_entity) = find_entity_by_element_id(world, csg_element_id) {
        if let Some(csg_node) = world.get::<CsgNode>(csg_entity).cloned() {
            for operand_id in [csg_node.operand_a, csg_node.operand_b] {
                if let Some(op_entity) = find_entity_by_element_id(world, operand_id) {
                    world.entity_mut(op_entity).remove::<CsgOperand>();
                    world.entity_mut(op_entity).insert(Visibility::Visible);
                }
            }
        }
        world.despawn(csg_entity);
    }
}

fn update_transform_status(world: &mut World) {
    let Some(preview) = compute_preview(world) else {
        return;
    };

    let state = world.resource::<TransformState>().clone();
    let inference = world.resource::<InferenceEngine>();
    let is_push_pull = world.resource::<PushPullContext>().active_face.is_some();
    let mode_label = match state.mode {
        TransformMode::Idle => return,
        TransformMode::Moving if is_push_pull => "Push/Pull",
        TransformMode::Moving => "Move",
        TransformMode::Rotating => "Rotate",
        TransformMode::Scaling => "Scale",
    };

    let axis_label = axis_status_label(state.axis);
    let hint = match state.mode {
        TransformMode::Moving => preview
            .display_value
            .map(|value| format!("{mode_label}{axis_label}: {value:.2}m"))
            .unwrap_or_else(|| format!("{mode_label}{axis_label}: cursor")),
        TransformMode::Rotating => preview
            .display_value
            .map(|value| format!("{mode_label}{axis_label}: {value:.1}\u{b0}"))
            .unwrap_or_else(|| format!("{mode_label}{axis_label}: cursor")),
        TransformMode::Scaling => preview
            .display_value
            .map(|value| format!("{mode_label}{axis_label}: {value:.2}\u{d7}"))
            .unwrap_or_else(|| format!("{mode_label}{axis_label}: cursor")),
        TransformMode::Idle => String::new(),
    };

    let extra_hint = match state.mode {
        TransformMode::Rotating => " · Ctrl snap 15\u{b0} · X/Y/Z plane",
        TransformMode::Scaling => " · Shift uniform",
        _ => "",
    };
    let mut parts = vec![format!("{hint}{extra_hint} · Enter confirm · Esc cancel")];
    if let Some(label) = inference.best_label() {
        parts.push(label.to_string());
    }
    if inference.last_mode == Some(state.mode) {
        if let Some(last_dist) = inference.last_distance {
            parts.push(format!("Tab: {last_dist:.2}m"));
        }
    }
    world.resource_mut::<StatusBarData>().hint = parts.join(" | ");
}

fn draw_transform_constraint(
    pivot_point: Res<PivotPoint>,
    transform_state: Res<TransformState>,
    push_pull: Res<PushPullContext>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    if transform_state.is_idle() || transform_state.axis == AxisConstraint::None {
        return;
    }
    // During push/pull the constraint axis is the face normal — the user
    // already sees the face moving, so the big axis line is visual noise.
    if push_pull.active_face.is_some() {
        return;
    }

    let Some(center) = effective_pivot(&transform_state, &pivot_point) else {
        return;
    };
    let (direction, color) = constraint_visual(transform_state.axis);
    gizmos.line(
        center - direction * AXIS_LINE_LENGTH_METRES,
        center + direction * AXIS_LINE_LENGTH_METRES,
        color,
    );
    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, 1);
}

fn draw_transform_preview(world: &World, mut gizmos: Gizmos) {
    // During push/pull the real entity mesh is updated live — no gizmo
    // outline needed.
    if world.resource::<PushPullContext>().active_face.is_some() {
        return;
    }

    let mode = world.resource::<TransformState>().mode;
    let Some(preview) = compute_preview(world) else {
        return;
    };
    let live_transform_preview = mode != TransformMode::Idle;

    for snapshot in preview.after {
        if live_transform_preview && snapshot.preview_transform().is_some() {
            continue;
        }
        snapshot.draw_preview(&mut gizmos, TRANSFORM_PREVIEW_COLOR);
    }
}

fn sync_feature_push_pull_preview(world: &mut World, pp: &PushPullFace, distance: f32) {
    let Some(parent_id) = pp.feature_parent else {
        return;
    };

    if distance.abs() <= FEATURE_PUSH_PULL_EPSILON {
        if let Some(live_csg_id) = pp.live_csg {
            teardown_live_csg(world, live_csg_id);
            if let Some(active_face) = world.resource_mut::<PushPullContext>().active_face.as_mut()
            {
                active_face.live_csg = None;
            }
        }
        return;
    }

    let op = if distance < 0.0 {
        BooleanOp::Difference
    } else {
        BooleanOp::Union
    };

    let live_csg_id = if let Some(existing_id) = pp.live_csg {
        existing_id
    } else {
        let new_id = world
            .resource::<crate::plugins::identity::ElementIdAllocator>()
            .next_id();
        if let Some(active_face) = world.resource_mut::<PushPullContext>().active_face.as_mut() {
            active_face.live_csg = Some(new_id);
        }
        new_id
    };

    CsgSnapshot {
        element_id: live_csg_id,
        csg_node: CsgNode {
            operand_a: parent_id,
            operand_b: pp.element_id,
            op,
        },
    }
    .apply_to(world);
}

fn compute_preview(world: &World) -> Option<TransformPreview> {
    let state = world.resource::<TransformState>().clone();
    if state.is_idle() {
        return None;
    }

    let center = effective_pivot(&state, world.resource::<PivotPoint>())?;
    let numeric_value = state
        .numeric_buffer
        .as_deref()
        .and_then(|buffer| buffer.parse::<f32>().ok());

    match state.mode {
        TransformMode::Moving => {
            let push_pull = world.resource::<PushPullContext>().active_face.clone();
            if let Some(pp) = &push_pull {
                let distance = push_pull_distance(world, &state, pp, numeric_value)?;
                let delta = pp.normal * distance;
                // Push/pull mode: project delta onto face normal for signed distance
                Some(TransformPreview {
                    after: state
                        .initial_snapshots
                        .iter()
                        .map(|(_, snapshot)| {
                            feature_push_pull_snapshot(snapshot, pp, distance).unwrap_or_else(
                                || {
                                    snapshot
                                        .push_pull(pp.face_id, distance)
                                        .unwrap_or_else(|| snapshot.translate_by(delta))
                                },
                            )
                        })
                        .collect(),
                    display_value: numeric_value.or(Some(distance)),
                })
            } else {
                let delta = move_delta(world, &state, numeric_value)?;
                Some(TransformPreview {
                    after: state
                        .initial_snapshots
                        .iter()
                        .map(|(_, snapshot)| snapshot.translate_by(delta))
                        .collect(),
                    display_value: numeric_value.or_else(|| Some(delta.length())),
                })
            }
        }
        TransformMode::Rotating => {
            let delta_radians = rotation_delta(world, &state, center, numeric_value)?;
            let rotation = rotation_quat(state.axis, delta_radians);
            Some(TransformPreview {
                after: state
                    .initial_snapshots
                    .iter()
                    .map(|(_, snapshot)| {
                        // Rotate geometry in place AND orbit center around the pivot.
                        let rotated = snapshot.rotate_by(rotation);
                        let offset = snapshot.center() - center;
                        let orbited_offset = rotation * offset;
                        let translation = (center + orbited_offset) - rotated.center();
                        rotated.translate_by(translation)
                    })
                    .collect(),
                display_value: Some(delta_radians.to_degrees()),
            })
        }
        TransformMode::Scaling => {
            let factor = scale_factor(world, &state, center, numeric_value)?;
            let display_value =
                numeric_value.or_else(|| Some(display_scale_value(state.axis, factor)));
            Some(TransformPreview {
                after: state
                    .initial_snapshots
                    .iter()
                    .map(|(_, snapshot)| snapshot.scale_by(factor, center))
                    .collect(),
                display_value,
            })
        }
        TransformMode::Idle => None,
    }
}

fn feature_push_pull_snapshot(
    snapshot: &BoxedEntity,
    pp: &PushPullFace,
    distance: f32,
) -> Option<BoxedEntity> {
    let _ = pp.feature_parent?;
    let anchor_origin = pp.feature_anchor_origin?;
    if distance.abs() <= FEATURE_PUSH_PULL_EPSILON {
        return Some(snapshot.clone());
    }
    let profile = snapshot
        .0
        .as_any()
        .downcast_ref::<PrimitiveSnapshot<ProfileExtrusion>>()?;
    let height = distance.abs().max(0.005);

    Some(
        PrimitiveSnapshot {
            element_id: profile.element_id,
            primitive: ProfileExtrusion {
                centre: anchor_origin + pp.normal * (distance * 0.5),
                profile: profile.primitive.profile.clone(),
                height,
            },
            rotation: profile.rotation,
            material_assignment: profile.material_assignment.clone(),
        }
        .into(),
    )
}

fn move_delta(world: &World, state: &TransformState, numeric_value: Option<f32>) -> Option<Vec3> {
    let initial_cursor = state.initial_cursor?;
    let current_cursor = current_transform_cursor(world)?;
    let raw_delta = current_cursor - initial_cursor;
    let constrained_delta = apply_move_constraint(raw_delta, state.axis);

    numeric_value
        .map(|value| numeric_move_delta(state.axis, constrained_delta, value))
        .or(Some(constrained_delta))
}

fn push_pull_distance(
    world: &World,
    state: &TransformState,
    pp: &PushPullFace,
    numeric_value: Option<f32>,
) -> Option<f32> {
    if let Some(value) = numeric_value {
        return Some(value);
    }

    let initial_cursor = state.initial_cursor?;
    let current_cursor = current_transform_cursor(world)?;
    let world_distance = (current_cursor - initial_cursor).dot(pp.normal);

    let screen_sign = pp.screen_normal.and_then(|screen_normal| {
        let current_cursor_screen = current_viewport_cursor(world)?;
        let initial_cursor_screen = pp.initial_cursor_screen?;
        let screen_delta = current_cursor_screen - initial_cursor_screen;
        (screen_delta.length_squared() > 1e-6)
            .then_some(screen_delta.dot(screen_normal).signum())
            .filter(|sign| *sign != 0.0)
    });

    Some(world_distance.abs() * screen_sign.unwrap_or_else(|| world_distance.signum()))
}

fn rotation_delta(
    world: &World,
    state: &TransformState,
    center: Vec3,
    numeric_value: Option<f32>,
) -> Option<f32> {
    if let Some(value) = numeric_value {
        return Some(value.to_radians());
    }

    let initial_cursor = state.initial_cursor?;
    let current_cursor = current_transform_cursor(world)?;

    // Project cursor offsets onto the rotation plane determined by axis constraint.
    // Default (None/Y) = rotate around Y axis in XZ plane.
    // X = rotate around X axis in YZ plane.
    // Z = rotate around Z axis in XY plane.
    let (start_2d, current_2d) =
        rotation_plane_project(initial_cursor - center, current_cursor - center, state.axis);

    let start_vector = normalized_vector_or_default(start_2d);
    let current_vector = normalized_vector_or_default(current_2d);

    let mut delta_radians =
        current_vector.y.atan2(current_vector.x) - start_vector.y.atan2(start_vector.x);
    delta_radians = normalize_angle(delta_radians);

    let keys = world.resource::<ButtonInput<KeyCode>>();
    let snap_rotate = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if snap_rotate {
        delta_radians = snap_angle(delta_radians, ROTATION_SNAP_INCREMENT_RADIANS);
    }

    Some(delta_radians)
}

/// Builds the rotation quaternion for the given axis constraint and angle.
fn rotation_quat(axis: AxisConstraint, delta_radians: f32) -> Quat {
    match axis {
        AxisConstraint::X | AxisConstraint::PlaneYZ => Quat::from_rotation_x(delta_radians),
        AxisConstraint::Z | AxisConstraint::PlaneXY => Quat::from_rotation_z(delta_radians),
        AxisConstraint::Custom(dir) => Quat::from_axis_angle(dir, delta_radians),
        _ => Quat::from_rotation_y(delta_radians),
    }
}

/// Projects 3D offsets onto a 2D plane for angle computation.
/// Returns (start_2d, current_2d) in the chosen rotation plane.
/// The projection axes are chosen so that positive atan2 delta matches the
/// right-hand-rule quaternion rotation direction for each axis.
fn rotation_plane_project(
    start_offset: Vec3,
    current_offset: Vec3,
    axis: AxisConstraint,
) -> (Vec2, Vec2) {
    match axis {
        // Rotate around X axis → project onto YZ plane.
        // Quat::from_rotation_x(+) sends +Y → +Z, so use (y, z) for atan2(z, y).
        AxisConstraint::X | AxisConstraint::PlaneYZ => (
            Vec2::new(start_offset.y, start_offset.z),
            Vec2::new(current_offset.y, current_offset.z),
        ),
        // Rotate around Z axis → project onto XY plane.
        // Quat::from_rotation_z(+) sends +X → +Y, so use (x, y) for atan2(y, x).
        AxisConstraint::Z | AxisConstraint::PlaneXY => (
            Vec2::new(start_offset.x, start_offset.y),
            Vec2::new(current_offset.x, current_offset.y),
        ),
        // Default: rotate around Y axis → project onto XZ plane.
        // Quat::from_rotation_y(+) sends +X → -Z, so use (x, -z) for atan2(-z, x).
        AxisConstraint::Custom(dir) => {
            // Build a tangent frame around the custom axis
            let up = if dir.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
            let tangent = dir.cross(up).normalize();
            let bitangent = tangent.cross(dir).normalize();
            (
                Vec2::new(start_offset.dot(tangent), start_offset.dot(bitangent)),
                Vec2::new(current_offset.dot(tangent), current_offset.dot(bitangent)),
            )
        }
        AxisConstraint::None | AxisConstraint::Y | AxisConstraint::PlaneXZ => (
            Vec2::new(start_offset.x, -start_offset.z),
            Vec2::new(current_offset.x, -current_offset.z),
        ),
    }
}

fn scale_factor(
    world: &World,
    state: &TransformState,
    center: Vec3,
    numeric_value: Option<f32>,
) -> Option<Vec3> {
    let keys = world.resource::<ButtonInput<KeyCode>>();
    let shift_held = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    // Shift overrides axis constraint to produce uniform scaling
    let effective_axis = if shift_held {
        AxisConstraint::None
    } else {
        state.axis
    };

    if let Some(value) = numeric_value {
        return Some(numeric_scale_factor(effective_axis, value));
    }

    let initial_cursor = state.initial_cursor?;
    let current_cursor = current_transform_cursor(world)?;
    let initial_offset = initial_cursor - center;
    let current_offset = current_cursor - center;

    Some(cursor_scale_factor(
        effective_axis,
        initial_offset,
        current_offset,
    ))
}

fn current_transform_cursor(world: &World) -> Option<Vec3> {
    if world.resource::<PushPullContext>().active_face.is_some() {
        let cursor_world_pos = world.resource::<CursorWorldPos>();
        if let Some(position) = cursor_world_pos.snapped.or(cursor_world_pos.raw) {
            return Some(position);
        }
    } else {
        let snap_result = world.resource::<SnapResult>();
        if let Some(position) = snap_result.position.or(snap_result.raw_position) {
            return Some(position);
        }
    }

    world
        .resource::<CursorWorldPos>()
        .snapped
        .or(world.resource::<CursorWorldPos>().raw)
}

fn current_viewport_cursor(world: &World) -> Option<Vec2> {
    let mut window_query = world.try_query_filtered::<&Window, With<PrimaryWindow>>()?;
    let window = window_query.single(world).ok()?;
    let cursor_position = window.cursor_position()?;
    let mut camera_query = world.try_query::<&Camera>()?;
    let camera = camera_query
        .iter(world)
        .find(|camera| camera.is_active)
        .or_else(|| camera_query.iter(world).next())?;
    Some(match camera.logical_viewport_rect() {
        Some(rect) => cursor_position - rect.min,
        None => cursor_position,
    })
}

fn restore_preview_transforms(world: &mut World) {
    let original_transforms = world
        .resource::<TransformState>()
        .initial_snapshots
        .iter()
        .filter_map(|(entity, snapshot)| {
            snapshot
                .preview_transform()
                .map(|transform| (*entity, transform))
        })
        .collect::<Vec<_>>();

    for (entity, transform) in original_transforms {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.insert(transform);
        }
    }
}

#[cfg(test)]
fn sync_preview_entities(world: &mut World, snapshots: &[BoxedEntity]) {
    let existing = {
        let mut state = world.resource_mut::<TransformState>();
        std::mem::take(&mut state.preview_entities)
    };
    let mut existing_by_id: HashMap<ElementId, Entity> = existing.into_iter().collect();
    let mut updated = Vec::new();

    for snapshot in snapshots {
        let element_id = snapshot.element_id();
        let preview_entity =
            snapshot.sync_preview_entity(world, existing_by_id.remove(&element_id));
        if let Some(preview_entity) = preview_entity {
            updated.push((element_id, preview_entity));
        }
    }

    for (element_id, preview_entity) in existing_by_id {
        if let Some(snapshot) = snapshots
            .iter()
            .find(|snapshot| snapshot.element_id() == element_id)
        {
            snapshot.cleanup_preview_entity(world, preview_entity);
        } else {
            let _ = world.despawn(preview_entity);
        }
    }

    world.resource_mut::<TransformState>().preview_entities = updated;
}

fn cleanup_preview_entities(world: &mut World) {
    let preview_entities = {
        let mut state = world.resource_mut::<TransformState>();
        std::mem::take(&mut state.preview_entities)
    };

    let snapshots_by_id = world
        .resource::<TransformState>()
        .initial_snapshots
        .iter()
        .map(|(_, snapshot)| (snapshot.element_id(), snapshot.clone()))
        .collect::<HashMap<_, _>>();

    for (element_id, preview_entity) in preview_entities {
        if let Some(snapshot) = snapshots_by_id.get(&element_id) {
            snapshot.cleanup_preview_entity(world, preview_entity);
        } else {
            let _ = world.despawn(preview_entity);
        }
    }
}

fn collect_selected_snapshots(world: &mut World) -> Vec<(Entity, BoxedEntity)> {
    let selected_entities = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).collect::<Vec<_>>()
    };

    let selected_set = selected_entities
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();

    let mut result = Vec::new();

    for entity in selected_entities {
        if opening_parent_is_selected(world, entity, &selected_set) {
            continue;
        }

        // If this is a group, expand to member snapshots instead
        if let Some(members) = world.get::<GroupMembers>(entity) {
            let member_ids = members.member_ids.clone();
            expand_group_member_snapshots(world, &member_ids, &mut result);
        } else if let Some(snapshot) = capture_selected_snapshot(world, entity) {
            result.push((entity, snapshot));
        }
    }

    result
}

fn expand_group_member_snapshots(
    world: &World,
    member_ids: &[ElementId],
    out: &mut Vec<(Entity, BoxedEntity)>,
) {
    let registry = world.resource::<CapabilityRegistry>();
    for member_id in member_ids {
        let mut q = world.try_query::<EntityRef>().unwrap();
        let Some(member_entity) = q
            .iter(world)
            .find(|e| e.get::<ElementId>().copied() == Some(*member_id))
        else {
            continue;
        };
        if let Some(members) = member_entity.get::<GroupMembers>() {
            // Recurse into nested groups
            let nested_ids = members.member_ids.clone();
            expand_group_member_snapshots(world, &nested_ids, out);
        } else if let Some(snapshot) = registry.capture_snapshot(&member_entity, world) {
            out.push((member_entity.id(), snapshot));
        }
    }
}

fn opening_parent_is_selected(
    world: &World,
    entity: Entity,
    selected_set: &std::collections::HashSet<Entity>,
) -> bool {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return false;
    };
    let Some(snapshot) = world
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&entity_ref, world)
    else {
        return false;
    };
    let Some(parent_id) = snapshot.transform_parent() else {
        return false;
    };
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .find(|entity_ref| entity_ref.get::<ElementId>().copied() == Some(parent_id))
        .map(|entity_ref| selected_set.contains(&entity_ref.id()))
        .unwrap_or(false)
}

fn capture_selected_snapshot(world: &World, entity: Entity) -> Option<BoxedEntity> {
    let entity_ref = world.get_entity(entity).ok()?;
    world
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&entity_ref, world)
}

fn selection_center(initial_snapshots: &[(Entity, BoxedEntity)]) -> Option<Vec3> {
    let count = initial_snapshots.len();
    if count == 0 {
        return None;
    }

    Some(
        initial_snapshots
            .iter()
            .map(|(_, snapshot)| snapshot.center())
            .fold(Vec3::ZERO, |sum, center| sum + center)
            / count as f32,
    )
}

fn effective_pivot(state: &TransformState, pivot_point: &PivotPoint) -> Option<Vec3> {
    state
        .pivot_override
        .or(pivot_point.position)
        .or_else(|| selection_center(&state.initial_snapshots))
}

fn transform_label(mode: TransformMode) -> &'static str {
    match mode {
        TransformMode::Idle => "Transform",
        TransformMode::Moving => "Move selection",
        TransformMode::Rotating => "Rotate selection",
        TransformMode::Scaling => "Scale selection",
    }
}

fn toggle_axis(current: AxisConstraint, axis: AxisConstraint) -> AxisConstraint {
    if current == axis {
        AxisConstraint::None
    } else {
        axis
    }
}

fn push_numeric_char(buffer: &mut Option<String>, character: char) {
    buffer.get_or_insert_with(String::new).push(character);
}

fn push_minus(buffer: &mut Option<String>) {
    let value = buffer.get_or_insert_with(String::new);
    if value.starts_with('-') {
        value.remove(0);
    } else {
        value.insert(0, '-');
    }
}

fn pop_numeric_char(buffer: &mut Option<String>) {
    let Some(value) = buffer.as_mut() else {
        return;
    };
    value.pop();
    if value.is_empty() || value == "-" {
        *buffer = None;
    }
}

fn axis_status_label(axis: AxisConstraint) -> &'static str {
    match axis {
        AxisConstraint::None => "",
        AxisConstraint::X => " X",
        AxisConstraint::Y => " Y",
        AxisConstraint::Z => " Z",
        AxisConstraint::PlaneYZ => " YZ",
        AxisConstraint::PlaneXZ => " XZ",
        AxisConstraint::PlaneXY => " XY",
        AxisConstraint::Custom(_) => " Custom",
    }
}

fn constraint_visual(axis: AxisConstraint) -> (Vec3, Color) {
    match axis {
        AxisConstraint::X => (Vec3::X, AXIS_X_COLOR),
        AxisConstraint::Y => (Vec3::Y, AXIS_Y_COLOR),
        AxisConstraint::Z => (Vec3::Z, AXIS_Z_COLOR),
        AxisConstraint::PlaneYZ => (Vec3::X, AXIS_X_COLOR),
        AxisConstraint::PlaneXZ => (Vec3::Y, AXIS_Y_COLOR),
        AxisConstraint::PlaneXY => (Vec3::Z, AXIS_Z_COLOR),
        AxisConstraint::Custom(dir) => (dir, Color::srgb(0.9, 0.6, 0.2)),
        AxisConstraint::None => (Vec3::ZERO, Color::WHITE),
    }
}

fn apply_move_constraint(delta: Vec3, axis: AxisConstraint) -> Vec3 {
    match axis {
        AxisConstraint::None | AxisConstraint::PlaneXZ => Vec3::new(delta.x, 0.0, delta.z),
        AxisConstraint::X => Vec3::new(delta.x, 0.0, 0.0),
        AxisConstraint::Y => Vec3::new(0.0, delta.y, 0.0),
        AxisConstraint::Z => Vec3::new(0.0, 0.0, delta.z),
        AxisConstraint::PlaneYZ => Vec3::new(0.0, delta.y, delta.z),
        AxisConstraint::PlaneXY => Vec3::new(delta.x, delta.y, 0.0),
        AxisConstraint::Custom(dir) => dir * delta.dot(dir),
    }
}

fn numeric_move_delta(axis: AxisConstraint, constrained_delta: Vec3, value: f32) -> Vec3 {
    match axis {
        AxisConstraint::X => Vec3::X * value,
        AxisConstraint::Y => Vec3::Y * value,
        AxisConstraint::Z => Vec3::Z * value,
        AxisConstraint::Custom(dir) => dir * value,
        _ => constrained_delta
            .try_normalize()
            .map(|direction| direction * value)
            .unwrap_or(Vec3::ZERO),
    }
}

fn numeric_scale_factor(axis: AxisConstraint, value: f32) -> Vec3 {
    match axis {
        AxisConstraint::None | AxisConstraint::Custom(_) => Vec3::splat(value),
        AxisConstraint::X => Vec3::new(value, 1.0, 1.0),
        AxisConstraint::Y => Vec3::new(1.0, value, 1.0),
        AxisConstraint::Z => Vec3::new(1.0, 1.0, value),
        AxisConstraint::PlaneYZ => Vec3::new(1.0, value, value),
        AxisConstraint::PlaneXZ => Vec3::new(value, 1.0, value),
        AxisConstraint::PlaneXY => Vec3::new(value, value, 1.0),
    }
}

fn should_rebase_scale_drag(
    state: &TransformState,
    mouse_buttons: &ButtonInput<MouseButton>,
) -> bool {
    state.mode == TransformMode::Scaling
        && !state.confirm_on_release
        && mouse_buttons.just_pressed(MouseButton::Left)
}

fn should_confirm_transform(
    state: &TransformState,
    mouse_buttons: &ButtonInput<MouseButton>,
    keys: &ButtonInput<KeyCode>,
) -> bool {
    (state.confirm_on_release && mouse_buttons.just_released(MouseButton::Left))
        || (!state.confirm_on_release
            && state.mode != TransformMode::Scaling
            && mouse_buttons.just_pressed(MouseButton::Left))
        || keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::NumpadEnter)
}

fn cursor_scale_factor(axis: AxisConstraint, initial_offset: Vec3, current_offset: Vec3) -> Vec3 {
    match axis {
        AxisConstraint::None | AxisConstraint::Custom(_) => {
            let factor = ratio_2d(initial_offset.xz().length(), current_offset.xz().length());
            Vec3::splat(factor)
        }
        AxisConstraint::X => Vec3::new(ratio_1d(initial_offset.x, current_offset.x), 1.0, 1.0),
        AxisConstraint::Y => Vec3::new(1.0, ratio_1d(initial_offset.y, current_offset.y), 1.0),
        AxisConstraint::Z => Vec3::new(1.0, 1.0, ratio_1d(initial_offset.z, current_offset.z)),
        AxisConstraint::PlaneYZ => {
            let factor = ratio_2d(
                Vec2::new(initial_offset.y, initial_offset.z).length(),
                Vec2::new(current_offset.y, current_offset.z).length(),
            );
            Vec3::new(1.0, factor, factor)
        }
        AxisConstraint::PlaneXZ => {
            let factor = ratio_2d(initial_offset.xz().length(), current_offset.xz().length());
            Vec3::new(factor, 1.0, factor)
        }
        AxisConstraint::PlaneXY => {
            let factor = ratio_2d(
                Vec2::new(initial_offset.x, initial_offset.y).length(),
                Vec2::new(current_offset.x, current_offset.y).length(),
            );
            Vec3::new(factor, factor, 1.0)
        }
    }
}

/// Compute the visual scale factor for the Transform preview by comparing
/// the before/after bounding boxes.  Returns `None` when bounds are
/// unavailable or degenerate, in which case the wireframe gizmo provides
/// the visual feedback instead.
fn preview_scale(before: &BoxedEntity, after: &BoxedEntity) -> Option<Vec3> {
    let bb = before.bounds()?;
    let ab = after.bounds()?;
    let bs = bb.max - bb.min;
    let a = ab.max - ab.min;
    let safe_div = |num: f32, den: f32| -> f32 {
        if den.abs() > f32::EPSILON {
            num / den
        } else {
            1.0
        }
    };
    Some(Vec3::new(
        safe_div(a.x, bs.x),
        safe_div(a.y, bs.y),
        safe_div(a.z, bs.z),
    ))
}

fn display_scale_value(axis: AxisConstraint, factor: Vec3) -> f32 {
    match axis {
        AxisConstraint::None | AxisConstraint::Custom(_) => factor.x,
        AxisConstraint::X => factor.x,
        AxisConstraint::Y => factor.y,
        AxisConstraint::Z => factor.z,
        AxisConstraint::PlaneYZ => factor.y,
        AxisConstraint::PlaneXZ => factor.x,
        AxisConstraint::PlaneXY => factor.x,
    }
}

fn ratio_1d(initial: f32, current: f32) -> f32 {
    if initial.abs() <= f32::EPSILON {
        1.0
    } else {
        current / initial
    }
}

fn ratio_2d(initial: f32, current: f32) -> f32 {
    if initial <= f32::EPSILON {
        1.0
    } else {
        current / initial
    }
}

fn normalized_vector_or_default(vector: Vec2) -> Vec2 {
    vector.try_normalize().unwrap_or(Vec2::X)
}

fn normalize_angle(angle: f32) -> f32 {
    let tau = PI * 2.0;
    ((angle + PI).rem_euclid(tau)) - PI
}

fn snap_angle(angle: f32, increment: f32) -> f32 {
    (angle / increment).round() * increment
}

// --- Rotation Protractor ---

const PROTRACTOR_COLOR: Color = Color::srgb(0.42, 0.95, 0.58);
const PROTRACTOR_SEGMENTS: usize = 48;
const PROTRACTOR_TICK_COUNT: usize = 24; // every 15 degrees
const PROTRACTOR_MIN_RADIUS: f32 = 0.1;
const PROTRACTOR_MAX_RADIUS: f32 = 100.0;
/// Fraction of camera distance used as protractor radius (~120px at typical FOV).
const PROTRACTOR_SCREEN_FACTOR: f32 = 0.12;
const PROTRACTOR_TICK_INNER: f32 = 0.9;
const PROTRACTOR_TICK_INNER_MAJOR: f32 = 0.82;

#[derive(SystemParam)]
struct RotationProtractorContext<'w, 's> {
    transform_state: Res<'w, TransformState>,
    pivot_point: Res<'w, PivotPoint>,
    cursor_world_pos: Res<'w, CursorWorldPos>,
    snap_result: Res<'w, SnapResult>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    camera_query: Query<'w, 's, &'static GlobalTransform, With<Camera>>,
    gizmos: Gizmos<'w, 's>,
    #[cfg(feature = "perf-stats")]
    perf_stats: ResMut<'w, PerfStats>,
}

fn draw_rotation_protractor(mut cx: RotationProtractorContext) {
    if cx.transform_state.mode != TransformMode::Rotating {
        return;
    }

    let Some(center) = effective_pivot(&cx.transform_state, &cx.pivot_point) else {
        return;
    };
    let Some(initial_cursor) = cx.transform_state.initial_cursor else {
        return;
    };

    let axis = cx.transform_state.axis;

    // Project initial offset onto the rotation plane (for angle computation)
    let (initial_2d, _) =
        rotation_plane_project(initial_cursor - center, initial_cursor - center, axis);

    // Compute viewport-adaptive radius from camera distance
    let radius = cx
        .camera_query
        .iter()
        .next()
        .map(|cam_tf| {
            let camera_distance = (cam_tf.translation() - center).length();
            (camera_distance * PROTRACTOR_SCREEN_FACTOR)
                .clamp(PROTRACTOR_MIN_RADIUS, PROTRACTOR_MAX_RADIUS)
        })
        .unwrap_or(2.0);

    // Choose the protractor color based on axis constraint
    let protractor_color = match axis {
        AxisConstraint::X | AxisConstraint::PlaneYZ => AXIS_X_COLOR,
        AxisConstraint::Z | AxisConstraint::PlaneXY => AXIS_Z_COLOR,
        _ => PROTRACTOR_COLOR, // default green for Y/None
    };
    let dim_color = Color::srgba(
        protractor_color.to_linear().red,
        protractor_color.to_linear().green,
        protractor_color.to_linear().blue,
        0.35,
    );

    #[cfg(feature = "perf-stats")]
    let mut line_count = 0usize;

    // Draw outer circle in the correct plane
    let mut previous = None;
    for index in 0..=PROTRACTOR_SEGMENTS {
        let angle = (index as f32 / PROTRACTOR_SEGMENTS as f32) * std::f32::consts::TAU;
        let point = center + protractor_point(angle, radius, axis);
        if let Some(previous) = previous {
            cx.gizmos.line(previous, point, dim_color);
            #[cfg(feature = "perf-stats")]
            {
                line_count += 1;
            }
        }
        previous = Some(point);
    }

    // Draw tick marks at 15-degree intervals
    for tick in 0..PROTRACTOR_TICK_COUNT {
        let angle = (tick as f32 / PROTRACTOR_TICK_COUNT as f32) * std::f32::consts::TAU;
        let is_major = tick % 6 == 0;
        let inner_factor = if is_major {
            PROTRACTOR_TICK_INNER_MAJOR
        } else {
            PROTRACTOR_TICK_INNER
        };
        let inner = center + protractor_point(angle, radius * inner_factor, axis);
        let outer = center + protractor_point(angle, radius, axis);
        cx.gizmos.line(inner, outer, dim_color);
        #[cfg(feature = "perf-stats")]
        {
            line_count += 1;
        }
    }

    // Draw initial angle radial line
    let start_angle = initial_2d.y.atan2(initial_2d.x);
    let start_point = center + protractor_point(start_angle, radius, axis);
    cx.gizmos.line(center, start_point, dim_color);
    #[cfg(feature = "perf-stats")]
    {
        line_count += 1;
    }

    // Compute current angle from numeric input or cursor position
    let current_cursor = cx
        .snap_result
        .position
        .or(cx.snap_result.raw_position)
        .or(cx.cursor_world_pos.snapped)
        .or(cx.cursor_world_pos.raw);

    let current_angle_opt: Option<f32> = cx
        .transform_state
        .numeric_buffer
        .as_deref()
        .and_then(|buffer| buffer.parse::<f32>().ok())
        .map(|degrees| start_angle + degrees.to_radians())
        .or_else(|| {
            let cursor = current_cursor?;
            let (_, current_2d) =
                rotation_plane_project(initial_cursor - center, cursor - center, axis);
            let start_vec = normalized_vector_or_default(initial_2d);
            let current_vec = normalized_vector_or_default(current_2d);
            let mut delta = current_vec.y.atan2(current_vec.x) - start_vec.y.atan2(start_vec.x);
            delta = normalize_angle(delta);
            let snap_rotate =
                cx.keys.pressed(KeyCode::ControlLeft) || cx.keys.pressed(KeyCode::ControlRight);
            if snap_rotate {
                delta = snap_angle(delta, ROTATION_SNAP_INCREMENT_RADIANS);
            }
            Some(start_angle + delta)
        });

    if let Some(current_angle) = current_angle_opt {
        let current_point = center + protractor_point(current_angle, radius, axis);
        cx.gizmos.line(center, current_point, protractor_color);
        #[cfg(feature = "perf-stats")]
        {
            line_count += 1;
        }

        // Draw sweep arc
        let delta = normalize_angle(current_angle - start_angle);
        let arc_segments = ((delta.abs() / std::f32::consts::TAU) * PROTRACTOR_SEGMENTS as f32)
            .ceil()
            .max(2.0) as usize;
        let mut arc_prev = None;
        for index in 0..=arc_segments {
            let t = index as f32 / arc_segments as f32;
            let angle = start_angle + delta * t;
            let point = center + protractor_point(angle, radius, axis);
            if let Some(prev) = arc_prev {
                cx.gizmos.line(prev, point, protractor_color);
                #[cfg(feature = "perf-stats")]
                {
                    line_count += 1;
                }
            }
            arc_prev = Some(point);
        }
    }

    // Draw rotation axis indicator line through center
    let axis_direction = rotation_axis_direction(axis);
    if axis_direction != Vec3::ZERO {
        let axis_color = match axis {
            AxisConstraint::X | AxisConstraint::PlaneYZ => AXIS_X_COLOR,
            AxisConstraint::Z | AxisConstraint::PlaneXY => AXIS_Z_COLOR,
            _ => AXIS_Y_COLOR,
        };
        let half = radius * 0.3;
        cx.gizmos.line(
            center - axis_direction * half,
            center + axis_direction * half,
            axis_color,
        );
        #[cfg(feature = "perf-stats")]
        {
            line_count += 1;
        }
    }

    #[cfg(feature = "perf-stats")]
    if line_count > 0 {
        add_gizmo_line_count(&mut cx.perf_stats, line_count);
    }
}

/// Convert an angle + radius to a 3D point offset in the correct rotation plane.
/// Matches `rotation_plane_project` conventions so the protractor visual
/// sweeps in the same direction as the quaternion rotation.
fn protractor_point(angle: f32, radius: f32, axis: AxisConstraint) -> Vec3 {
    let (cos, sin) = (angle.cos() * radius, angle.sin() * radius);
    match axis {
        // Rotate around X → disc in YZ plane: angle=0 at +Y, increases toward +Z
        AxisConstraint::X | AxisConstraint::PlaneYZ => Vec3::new(0.0, cos, sin),
        // Rotate around Z → disc in XY plane: angle=0 at +X, increases toward +Y
        AxisConstraint::Z | AxisConstraint::PlaneXY => Vec3::new(cos, sin, 0.0),
        // Custom axis: build tangent frame
        AxisConstraint::Custom(dir) => {
            let up = if dir.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
            let tangent = dir.cross(up).normalize();
            let bitangent = tangent.cross(dir).normalize();
            tangent * cos + bitangent * sin
        }
        // Rotate around Y → disc in XZ plane: angle=0 at +X, increases toward -Z
        _ => Vec3::new(cos, 0.0, -sin),
    }
}

/// Returns the unit vector along the rotation axis.
fn rotation_axis_direction(axis: AxisConstraint) -> Vec3 {
    match axis {
        AxisConstraint::X | AxisConstraint::PlaneYZ => Vec3::X,
        AxisConstraint::Z | AxisConstraint::PlaneXY => Vec3::Z,
        AxisConstraint::Y | AxisConstraint::PlaneXZ => Vec3::Y,
        AxisConstraint::Custom(dir) => dir,
        AxisConstraint::None => Vec3::ZERO,
    }
}

// --- Scale Guides ---

const SCALE_REFERENCE_COLOR: Color = Color::srgba(1.0, 0.8, 0.2, 0.4);
const SCALE_ACTIVE_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);

fn draw_scale_guides(
    transform_state: Res<TransformState>,
    pivot_point: Res<PivotPoint>,
    cursor_world_pos: Res<CursorWorldPos>,
    snap_result: Res<SnapResult>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    if transform_state.mode != TransformMode::Scaling {
        return;
    }

    let Some(center) = effective_pivot(&transform_state, &pivot_point) else {
        return;
    };
    let Some(initial_cursor) = transform_state.initial_cursor else {
        return;
    };
    let current_cursor = snap_result
        .position
        .or(snap_result.raw_position)
        .or(cursor_world_pos.snapped)
        .or(cursor_world_pos.raw);

    // Reference line: pivot to initial cursor
    gizmos.line(center, initial_cursor, SCALE_REFERENCE_COLOR);

    // Active line: pivot through current cursor
    if let Some(current) = current_cursor {
        let direction = (current - center).try_normalize().unwrap_or(Vec3::X);
        let initial_distance = (initial_cursor - center).length();
        let current_distance = (current - center).length();
        let line_end = center + direction * current_distance.max(initial_distance * 0.1);
        gizmos.line(center, line_end, SCALE_ACTIVE_COLOR);
    }

    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, 2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authored_entity::{AuthoredEntity, HandleInfo, PropertyFieldDef};
    use crate::plugins::tools::Preview;
    use serde_json::Value;

    #[derive(Clone)]
    struct TestPreviewSnapshot {
        element_id: ElementId,
    }

    impl AuthoredEntity for TestPreviewSnapshot {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn type_name(&self) -> &'static str {
            "test_preview"
        }

        fn element_id(&self) -> ElementId {
            self.element_id
        }

        fn label(&self) -> String {
            "Test Preview".to_string()
        }

        fn center(&self) -> Vec3 {
            Vec3::ZERO
        }

        fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
            self.clone().into()
        }

        fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
            self.clone().into()
        }

        fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
            self.clone().into()
        }

        fn property_fields(&self) -> Vec<PropertyFieldDef> {
            Vec::new()
        }

        fn set_property_json(
            &self,
            _property_name: &str,
            _value: &Value,
        ) -> Result<BoxedEntity, String> {
            Ok(self.clone().into())
        }

        fn handles(&self) -> Vec<HandleInfo> {
            Vec::new()
        }

        fn to_json(&self) -> Value {
            Value::Null
        }

        fn apply_to(&self, _world: &mut World) {}

        fn remove_from(&self, _world: &mut World) {}

        fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

        fn sync_preview_entity(
            &self,
            world: &mut World,
            existing: Option<Entity>,
        ) -> Option<Entity> {
            if let Some(existing) = existing {
                return Some(existing);
            }
            Some(world.spawn((Preview, Name::new("test-preview"))).id())
        }

        fn cleanup_preview_entity(&self, world: &mut World, preview_entity: Entity) {
            let _ = world.despawn(preview_entity);
        }

        fn box_clone(&self) -> BoxedEntity {
            self.clone().into()
        }

        fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
            other
                .as_any()
                .downcast_ref::<Self>()
                .map(|snapshot| snapshot.element_id == self.element_id)
                .unwrap_or(false)
        }
    }

    impl From<TestPreviewSnapshot> for BoxedEntity {
        fn from(snapshot: TestPreviewSnapshot) -> Self {
            BoxedEntity(Box::new(snapshot))
        }
    }

    fn transform_input_world(state: TransformState, keys: ButtonInput<KeyCode>) -> World {
        let mut world = World::new();
        world.insert_resource(InputOwnership::Modal(ModalKind::Transform));
        world.insert_resource(state);
        world.insert_resource(keys);
        world
    }

    fn press_key(key: KeyCode) -> ButtonInput<KeyCode> {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(key);
        keys
    }

    fn press_shift_key(key: KeyCode) -> ButtonInput<KeyCode> {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(key);
        keys
    }

    fn assert_vec3_near(actual: Vec3, expected: Vec3) {
        assert!(
            actual.abs_diff_eq(expected, 0.0001),
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn pending_mode_is_idle_for_visual_systems() {
        let mut state = TransformState::default();
        assert!(state.is_idle());
        assert!(!state.is_pending());

        // Set pending mode — should still be considered idle for visual systems
        state.pending_mode = Some(TransformMode::Rotating);
        assert!(
            state.is_idle(),
            "pending state should still be idle for visuals"
        );
        assert!(state.is_pending());

        // Active transform is not idle
        state.mode = TransformMode::Rotating;
        state.pending_mode = None;
        assert!(!state.is_idle());
        assert!(!state.is_pending());
    }

    #[test]
    fn pending_mode_switches_on_different_key() {
        let mut state = TransformState {
            pending_mode: Some(TransformMode::Rotating),
            ..Default::default()
        };

        // Switching to Scale should update pending_mode
        state.pending_mode = Some(TransformMode::Scaling);
        assert_eq!(state.pending_mode, Some(TransformMode::Scaling));
        assert!(state.is_idle());
    }

    #[test]
    fn clear_resets_pending_mode() {
        let mut state = TransformState {
            pending_mode: Some(TransformMode::Rotating),
            ..Default::default()
        };
        state.clear();
        assert!(state.is_idle());
        assert!(!state.is_pending());
        assert_eq!(state.pending_mode, None);
    }

    #[test]
    fn axis_input_toggles_axes_and_shift_selects_exclusion_planes() {
        let mut world = transform_input_world(
            TransformState {
                mode: TransformMode::Moving,
                ..Default::default()
            },
            press_key(KeyCode::KeyX),
        );
        handle_transform_input(&mut world);
        assert_eq!(world.resource::<TransformState>().axis, AxisConstraint::X);

        let mut world = transform_input_world(
            TransformState {
                mode: TransformMode::Moving,
                axis: AxisConstraint::X,
                ..Default::default()
            },
            press_key(KeyCode::KeyX),
        );
        handle_transform_input(&mut world);
        assert_eq!(
            world.resource::<TransformState>().axis,
            AxisConstraint::None
        );

        let mut world = transform_input_world(
            TransformState {
                mode: TransformMode::Moving,
                ..Default::default()
            },
            press_shift_key(KeyCode::KeyX),
        );
        handle_transform_input(&mut world);
        assert_eq!(
            world.resource::<TransformState>().axis,
            AxisConstraint::PlaneYZ
        );
    }

    #[test]
    fn numeric_buffer_supports_negative_decimal_backspace_flow() {
        let mut buffer = None;
        push_minus(&mut buffer);
        push_numeric_char(&mut buffer, '2');
        push_numeric_char(&mut buffer, '.');
        push_numeric_char(&mut buffer, '5');
        assert_eq!(buffer.as_deref(), Some("-2.5"));

        pop_numeric_char(&mut buffer);
        assert_eq!(buffer.as_deref(), Some("-2."));
        pop_numeric_char(&mut buffer);
        assert_eq!(buffer.as_deref(), Some("-2"));
        pop_numeric_char(&mut buffer);
        assert_eq!(buffer, None);
    }

    #[test]
    fn move_constraints_keep_default_xz_plane_and_exact_axis_numeric_values() {
        assert_vec3_near(
            apply_move_constraint(Vec3::new(1.0, 2.0, 3.0), AxisConstraint::None),
            Vec3::new(1.0, 0.0, 3.0),
        );
        assert_vec3_near(
            apply_move_constraint(Vec3::new(1.0, 2.0, 3.0), AxisConstraint::PlaneYZ),
            Vec3::new(0.0, 2.0, 3.0),
        );
        assert_vec3_near(
            numeric_move_delta(AxisConstraint::X, Vec3::ZERO, -2.5),
            Vec3::new(-2.5, 0.0, 0.0),
        );
    }

    #[test]
    fn numeric_scale_factor_supports_uniform_axis_and_plane_constraints() {
        assert_vec3_near(
            numeric_scale_factor(AxisConstraint::None, 2.0),
            Vec3::splat(2.0),
        );
        assert_vec3_near(
            numeric_scale_factor(AxisConstraint::Y, 0.5),
            Vec3::new(1.0, 0.5, 1.0),
        );
        assert_vec3_near(
            numeric_scale_factor(AxisConstraint::PlaneXY, 1.5),
            Vec3::new(1.5, 1.5, 1.0),
        );
    }

    #[test]
    fn preview_entities_are_reused_and_cleaned_up() {
        let mut world = World::new();
        world.insert_resource(TransformState::default());
        world.resource_mut::<TransformState>().initial_snapshots = vec![(
            Entity::PLACEHOLDER,
            TestPreviewSnapshot {
                element_id: ElementId(1),
            }
            .into(),
        )];

        let snapshots = vec![TestPreviewSnapshot {
            element_id: ElementId(1),
        }
        .into()];
        sync_preview_entities(&mut world, &snapshots);

        let preview_entity = world.resource::<TransformState>().preview_entities[0].1;
        assert!(world.entity(preview_entity).contains::<Preview>());

        sync_preview_entities(&mut world, &snapshots);
        let reused_entity = world.resource::<TransformState>().preview_entities[0].1;
        assert_eq!(reused_entity, preview_entity);

        cleanup_preview_entities(&mut world);
        assert!(world
            .resource::<TransformState>()
            .preview_entities
            .is_empty());
        assert!(!world.entities().contains(preview_entity));
    }

    #[test]
    fn scaling_rebases_on_mouse_press_instead_of_confirming() {
        let state = TransformState {
            mode: TransformMode::Scaling,
            ..Default::default()
        };
        let mut mouse_buttons = ButtonInput::<MouseButton>::default();
        let keys = ButtonInput::<KeyCode>::default();
        mouse_buttons.press(MouseButton::Left);

        assert!(should_rebase_scale_drag(&state, &mouse_buttons));
        assert!(!should_confirm_transform(&state, &mouse_buttons, &keys));
    }

    #[test]
    fn scaling_confirms_on_release_after_drag_is_armed() {
        let state = TransformState {
            mode: TransformMode::Scaling,
            confirm_on_release: true,
            ..Default::default()
        };
        let mut mouse_buttons = ButtonInput::<MouseButton>::default();
        let keys = ButtonInput::<KeyCode>::default();
        mouse_buttons.press(MouseButton::Left);
        mouse_buttons.release(MouseButton::Left);

        assert!(should_confirm_transform(&state, &mouse_buttons, &keys));
    }
}
