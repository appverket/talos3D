use bevy::{camera::Projection, ecs::system::SystemParam, prelude::*, window::PrimaryWindow};

use crate::{
    authored_entity::{BoxedEntity, EntityBounds, HandleKind},
    capability_registry::CapabilityRegistry,
    plugins::{
        commands::ApplyEntityChangesCommand,
        cursor::CursorWorldPos,
        egui_chrome::EguiWantsInput,
        face_edit::FaceEditContext,
        input_ownership::{InputOwnership, InputPhase},
        selection::Selected,
        snap::SnapResult,
        tools::ActiveTool,
        transform::{
            start_transform_mode_with_options, AxisConstraint, PivotPoint, TransformMode,
            TransformStartOptions, TransformState, TransformVisualSystems,
        },
        ui::StatusBarData,
    },
};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};

const HANDLE_SCREEN_DIAMETER_PX: f32 = 8.0;
const HANDLE_MIN_RADIUS_METRES: f32 = 0.035;
const HANDLE_MAX_RADIUS_METRES: f32 = 0.18;
const HANDLE_HIT_TOLERANCE_PX: f32 = 12.0;
const HANDLE_DRAG_THRESHOLD_PX: f32 = 6.0;
const HANDLE_MOVE_COLOR: Color = Color::srgb(0.95, 0.95, 0.95);
const HANDLE_SCALE_COLOR: Color = Color::srgb(1.0, 0.82, 0.28);
const HANDLE_ENDPOINT_COLOR: Color = Color::srgb(0.42, 0.92, 1.0);
const HANDLE_ROTATE_COLOR: Color = Color::srgb(0.45, 0.94, 0.55);
const HANDLE_PARAMETER_COLOR: Color = Color::srgb(1.0, 0.82, 0.28);
const HANDLE_HOVER_BRIGHTNESS: f32 = 1.25;
const PIVOT_COLOR: Color = Color::srgb(0.96, 0.35, 0.8);
const ROTATE_RING_COLOR: Color = Color::srgb(0.42, 0.95, 0.58);
const ROTATE_RING_SEGMENTS: usize = 24;
const MOVE_HANDLE_HINT: &str = "Move: drag a corner or control point · snaps to nearby markers";

pub struct HandlesPlugin;

impl Plugin for HandlesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HandleContext>()
            .init_resource::<HandleInteractionState>()
            .add_systems(
                Update,
                (
                    reset_handle_context_on_selection_change,
                    sync_handle_display_mode
                        .in_set(InputPhase::HandleInput)
                        .run_if(in_state(ActiveTool::Select)),
                    update_hovered_handle
                        .in_set(InputPhase::HandleInput)
                        .run_if(in_state(ActiveTool::Select)),
                    handle_handle_pointer_input
                        .in_set(InputPhase::HandleInput)
                        .run_if(in_state(ActiveTool::Select)),
                    update_property_handle_drag
                        .in_set(InputPhase::HandleInput)
                        .run_if(in_state(ActiveTool::Select)),
                    update_handle_status.run_if(in_state(ActiveTool::Select)),
                ),
            )
            .add_systems(
                Update,
                (
                    draw_property_handle_preview
                        .after(TransformVisualSystems::PreviewDraw)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_pivot_indicator
                        .after(TransformVisualSystems::PreviewDraw)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_rotate_ring
                        .after(TransformVisualSystems::PreviewDraw)
                        .run_if(in_state(ActiveTool::Select)),
                    draw_selected_handles
                        .after(TransformVisualSystems::PreviewDraw)
                        .run_if(in_state(ActiveTool::Select)),
                ),
            );
    }
}

pub fn arm_move_handles(world: &mut World) -> Result<(), String> {
    if !world.resource::<InputOwnership>().is_idle() {
        return Err("Input is owned by another system".to_string());
    }

    let has_selection = {
        let mut query = world.query_filtered::<Entity, With<Selected>>();
        query.iter(world).next().is_some()
    };
    if !has_selection {
        return Err("No selection to transform".to_string());
    }

    world.resource_mut::<HandleContext>().display_mode = HandleDisplayMode::Move;
    clear_pending_handle_press(world);
    world.resource_mut::<StatusBarData>().hint = MOVE_HANDLE_HINT.to_string();
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandleDisplayMode {
    #[default]
    Move,
    Scale,
    Rotate,
}

#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HandleContext {
    pub display_mode: HandleDisplayMode,
}

#[derive(Resource, Default)]
pub struct HandleInteractionState {
    hovered: Option<ResolvedHandle>,
    pressed: Option<PendingHandlePress>,
    property_drag: Option<PropertyHandleDrag>,
}

impl HandleInteractionState {
    pub fn is_busy(&self) -> bool {
        self.pressed.is_some() || self.property_drag.is_some()
    }

    pub fn captures_pointer(&self) -> bool {
        self.hovered.is_some() || self.pressed.is_some() || self.property_drag.is_some()
    }

    pub fn property_drag_active(&self) -> bool {
        self.property_drag.is_some()
    }
}

#[derive(Clone)]
struct ResolvedHandle {
    entity: Entity,
    snapshot: BoxedEntity,
    id: String,
    label: String,
    position: Vec3,
    render_style: HandleRenderStyle,
    interaction: HandleInteraction,
    screen_position: Vec2,
}

impl ResolvedHandle {
    fn matches(&self, entity: Entity, handle_id: &str) -> bool {
        self.entity == entity && self.id == handle_id
    }
}

#[derive(Clone)]
struct PendingHandlePress {
    handle: ResolvedHandle,
    pressed_cursor_screen: Vec2,
}

#[derive(Clone, Copy)]
enum HandleRenderStyle {
    Move,
    Scale,
    Rotate,
    Vertex,
    Center,
    Control,
    Parameter,
}

#[derive(Clone)]
enum HandleInteraction {
    Authored {
        handle_id: String,
    },
    Transform {
        mode: TransformMode,
        axis: AxisConstraint,
        pivot_override: Option<Vec3>,
    },
}

#[derive(Clone)]
struct PropertyHandleDrag {
    label: String,
    before: BoxedEntity,
    current: BoxedEntity,
    handle_id: String,
    preview_entity: Option<Entity>,
}

#[derive(SystemParam)]
struct HandleViewportQuery<'w, 's> {
    window_query: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    camera_query: Query<
        'w,
        's,
        (
            &'static Camera,
            &'static GlobalTransform,
            &'static Projection,
        ),
    >,
}

#[derive(SystemParam)]
struct RotateRingContext<'w, 's> {
    selected_query: Query<'w, 's, Entity, With<Selected>>,
    registry: Res<'w, CapabilityRegistry>,
    handle_context: Res<'w, HandleContext>,
    transform_state: Res<'w, TransformState>,
    pivot_point: Res<'w, PivotPoint>,
    viewport: HandleViewportQuery<'w, 's>,
}

#[derive(SystemParam)]
struct SelectedHandleContext<'w, 's> {
    selected_query: Query<'w, 's, Entity, With<Selected>>,
    registry: Res<'w, CapabilityRegistry>,
    handle_context: Res<'w, HandleContext>,
    pivot_point: Res<'w, PivotPoint>,
    ownership: Res<'w, InputOwnership>,
    viewport: HandleViewportQuery<'w, 's>,
    handle_state: Res<'w, HandleInteractionState>,
}

#[derive(SystemParam)]
struct HoverHandleContext<'w, 's> {
    selected_query: Query<'w, 's, Entity, With<Selected>>,
    registry: Res<'w, CapabilityRegistry>,
    handle_context: Res<'w, HandleContext>,
    pivot_point: Res<'w, PivotPoint>,
    ownership: Res<'w, InputOwnership>,
    egui_wants_input: Res<'w, EguiWantsInput>,
    face_edit_context: Res<'w, FaceEditContext>,
    viewport: HandleViewportQuery<'w, 's>,
    handle_state: Res<'w, HandleInteractionState>,
}

impl HandleViewportQuery<'_, '_> {
    fn first_camera(&self) -> Option<(&Camera, &GlobalTransform, &Projection)> {
        self.camera_query.iter().next()
    }

    fn cursor_and_camera(&self) -> Option<(Vec2, &Camera, &GlobalTransform, &Projection)> {
        let window = self.window_query.single().ok()?;
        let cursor_position = window.cursor_position()?;
        let (camera, camera_transform, projection) = self.camera_query.iter().next()?;
        Some((cursor_position, camera, camera_transform, projection))
    }
}

fn reset_handle_context_on_selection_change(
    mut commands: Commands,
    added_selection: Query<(), Added<Selected>>,
    mut removed_selection: RemovedComponents<Selected>,
    mut handle_context: ResMut<HandleContext>,
    mut handle_state: ResMut<HandleInteractionState>,
    mut pivot_point: ResMut<PivotPoint>,
) {
    if added_selection.is_empty() && removed_selection.read().next().is_none() {
        return;
    }

    if let Some(drag) = handle_state.property_drag.take() {
        if let Some(preview_entity) = drag.preview_entity {
            commands.entity(preview_entity).despawn();
        }
    }
    handle_state.hovered = None;
    handle_state.pressed = None;
    handle_context.display_mode = HandleDisplayMode::Move;
    pivot_point.position = None;
}

fn sync_handle_display_mode(
    keys: Res<ButtonInput<KeyCode>>,
    transform_state: Res<TransformState>,
    face_edit_context: Res<FaceEditContext>,
    mut handle_context: ResMut<HandleContext>,
) {
    // Allow mode switching in idle or pending state
    if transform_state.mode != TransformMode::Idle {
        return;
    }

    // Don't respond to G/S/R when a modifier key is held (Cmd+G = Group, etc.)
    let has_modifier = keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::AltLeft)
        || keys.pressed(KeyCode::AltRight);
    if has_modifier {
        return;
    }

    // In face edit mode, G is reserved for push/pull — don't switch handle mode
    let face_editing = face_edit_context.is_active();

    if keys.just_pressed(KeyCode::KeyG) && !face_editing {
        handle_context.display_mode = HandleDisplayMode::Move;
    } else if keys.just_pressed(KeyCode::KeyS) {
        handle_context.display_mode = HandleDisplayMode::Scale;
    } else if keys.just_pressed(KeyCode::KeyR) {
        handle_context.display_mode = HandleDisplayMode::Rotate;
    }
}

fn update_hovered_handle(world: &World, mut commands: Commands, hover_handles: HoverHandleContext) {
    let mut next_state = HandleInteractionState {
        hovered: hover_handles.handle_state.hovered.clone(),
        pressed: hover_handles.handle_state.pressed.clone(),
        property_drag: hover_handles.handle_state.property_drag.clone(),
    };

    if !hover_handles.ownership.is_idle()
        || hover_handles.egui_wants_input.pointer
        || hover_handles.face_edit_context.is_active()
        || hover_handles.handle_state.property_drag.is_some()
    {
        next_state.hovered = None;
        commands.insert_resource(next_state);
        return;
    }

    let Some((cursor_position, camera, camera_transform, _projection)) =
        hover_handles.viewport.cursor_and_camera()
    else {
        next_state.hovered = None;
        commands.insert_resource(next_state);
        return;
    };

    next_state.hovered = hover_handles
        .selected_query
        .iter()
        .filter_map(|entity| {
            resolve_entity_handles(
                world,
                &hover_handles.registry,
                entity,
                camera,
                camera_transform,
                hover_handles.handle_context.display_mode,
                hover_handles.pivot_point.position,
            )
        })
        .flatten()
        .filter_map(|handle| {
            let distance = handle.screen_position.distance(cursor_position);
            (distance <= HANDLE_HIT_TOLERANCE_PX).then_some((distance, handle))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, handle)| handle);
    commands.insert_resource(next_state);
}

fn handle_handle_pointer_input(world: &mut World) {
    if !world.resource::<InputOwnership>().is_idle() {
        clear_pending_handle_press(world);
        return;
    }

    if world
        .resource::<HandleInteractionState>()
        .property_drag
        .is_some()
    {
        return;
    }

    let (left_pressed, left_released, cursor_screen, hovered_handle) = {
        let mouse_buttons = world.resource::<ButtonInput<MouseButton>>();
        let left_pressed = mouse_buttons.just_pressed(MouseButton::Left);
        let left_released = mouse_buttons.just_released(MouseButton::Left);
        let cursor_screen = current_cursor_screen(world);
        let hovered_handle = world.resource::<HandleInteractionState>().hovered.clone();
        (left_pressed, left_released, cursor_screen, hovered_handle)
    };

    if left_pressed {
        world.resource_mut::<HandleInteractionState>().pressed =
            hovered_handle.map(|handle| PendingHandlePress {
                handle,
                pressed_cursor_screen: cursor_screen.unwrap_or(Vec2::ZERO),
            });
        return;
    }

    let Some(pending) = world.resource::<HandleInteractionState>().pressed.clone() else {
        return;
    };

    let Some(cursor_screen) = cursor_screen else {
        return;
    };

    if cursor_screen.distance(pending.pressed_cursor_screen) >= HANDLE_DRAG_THRESHOLD_PX {
        begin_handle_drag(world, pending);
        world.resource_mut::<HandleInteractionState>().pressed = None;
        return;
    }

    if left_released {
        world.resource_mut::<PivotPoint>().position = Some(pending.handle.position);
        world.resource_mut::<HandleInteractionState>().pressed = None;
    }
}

fn begin_handle_drag(world: &mut World, pending: PendingHandlePress) {
    let Some(cursor_world) = current_cursor_world(world) else {
        return;
    };
    let display_mode = world.resource::<HandleContext>().display_mode;

    if let HandleInteraction::Authored { handle_id } = &pending.handle.interaction {
        if let Some(after) = pending.handle.snapshot.drag_handle(handle_id, cursor_world) {
            let mut preview_entity = None;
            preview_entity = after.sync_preview_entity(world, preview_entity);
            world.resource_mut::<HandleInteractionState>().property_drag =
                Some(PropertyHandleDrag {
                    label: pending.handle.label.clone(),
                    before: pending.handle.snapshot,
                    current: after,
                    handle_id: handle_id.clone(),
                    preview_entity,
                });
            return;
        }
    }

    let start_result = match pending.handle.interaction.clone() {
        HandleInteraction::Transform {
            mode,
            axis,
            pivot_override,
        } => start_transform_mode_with_options(
            world,
            mode,
            TransformStartOptions {
                axis,
                initial_cursor: transform_handle_initial_cursor(
                    mode,
                    pending.handle.position,
                    Some(cursor_world),
                ),
                pivot_override,
                confirm_on_release: true,
            },
        ),
        HandleInteraction::Authored { .. } => match display_mode {
            HandleDisplayMode::Move
                if matches!(pending.handle.render_style, HandleRenderStyle::Center) =>
            {
                start_transform_mode_with_options(
                    world,
                    TransformMode::Moving,
                    TransformStartOptions {
                        initial_cursor: transform_handle_initial_cursor(
                            TransformMode::Moving,
                            pending.handle.position,
                            Some(cursor_world),
                        ),
                        confirm_on_release: true,
                        ..default()
                    },
                )
            }
            HandleDisplayMode::Rotate
                if matches!(pending.handle.render_style, HandleRenderStyle::Center) =>
            {
                start_transform_mode_with_options(
                    world,
                    TransformMode::Rotating,
                    TransformStartOptions {
                        initial_cursor: Some(cursor_world),
                        confirm_on_release: true,
                        ..default()
                    },
                )
            }
            HandleDisplayMode::Scale
                if matches!(pending.handle.render_style, HandleRenderStyle::Vertex) =>
            {
                start_transform_mode_with_options(
                    world,
                    TransformMode::Scaling,
                    TransformStartOptions {
                        initial_cursor: transform_handle_initial_cursor(
                            TransformMode::Scaling,
                            pending.handle.position,
                            Some(cursor_world),
                        ),
                        pivot_override: Some(mirrored_pivot(
                            pending.handle.snapshot.center(),
                            pending.handle.position,
                        )),
                        confirm_on_release: true,
                        ..default()
                    },
                )
            }
            _ => {
                world.resource_mut::<PivotPoint>().position = Some(pending.handle.position);
                return;
            }
        },
    };

    if start_result.is_err() {
        world.resource_mut::<PivotPoint>().position = Some(pending.handle.position);
    }
}

fn transform_handle_initial_cursor(
    mode: TransformMode,
    handle_position: Vec3,
    fallback_cursor: Option<Vec3>,
) -> Option<Vec3> {
    match mode {
        TransformMode::Moving | TransformMode::Scaling => Some(handle_position),
        TransformMode::Rotating | TransformMode::Idle => fallback_cursor,
    }
}

fn update_property_handle_drag(world: &mut World) {
    let Some(mut drag) = take_property_handle_drag(world) else {
        return;
    };

    let cancel = world
        .resource::<ButtonInput<KeyCode>>()
        .just_pressed(KeyCode::Escape);
    if cancel {
        cleanup_property_drag_preview(world, &mut drag);
        return;
    }

    if let Some(cursor_world) = current_cursor_world(world) {
        if let Some(after) = drag.before.drag_handle(&drag.handle_id, cursor_world) {
            drag.current = after;
        }
    }

    drag.preview_entity = drag
        .current
        .sync_preview_entity(world, drag.preview_entity.take());

    let confirm = {
        let mouse_buttons = world.resource::<ButtonInput<MouseButton>>();
        let keys = world.resource::<ButtonInput<KeyCode>>();
        mouse_buttons.just_released(MouseButton::Left)
            || keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::NumpadEnter)
    };

    if confirm {
        if drag.before != drag.current {
            world
                .resource_mut::<Messages<ApplyEntityChangesCommand>>()
                .write(ApplyEntityChangesCommand {
                    label: "Drag handle",
                    before: vec![drag.before.clone()],
                    after: vec![drag.current.clone()],
                });
        }
        cleanup_property_drag_preview(world, &mut drag);
        return;
    }

    world.resource_mut::<HandleInteractionState>().property_drag = Some(drag);
}

fn update_handle_status(
    selected_query: Query<Entity, With<Selected>>,
    handle_context: Res<HandleContext>,
    handle_state: Res<HandleInteractionState>,
    ownership: Res<InputOwnership>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if !ownership.is_idle() {
        return;
    }

    if let Some(drag) = &handle_state.property_drag {
        status_bar_data.hint = format!("Drag {} · Release confirm · Esc cancel", drag.label);
        return;
    }

    if let Some(pending) = &handle_state.pressed {
        status_bar_data.hint = format!("Pivot/drag {} · Esc cancel", pending.handle.label);
        return;
    }

    if let Some(handle) = &handle_state.hovered {
        status_bar_data.command_hint = Some(handle.label.clone());
    } else if status_bar_data.command_hint.is_some() {
        status_bar_data.command_hint = None;
    } else if !selected_query.is_empty() && handle_context.display_mode == HandleDisplayMode::Move {
        status_bar_data.hint = MOVE_HANDLE_HINT.to_string();
    } else if status_bar_data.hint == MOVE_HANDLE_HINT {
        status_bar_data.hint.clear();
    }
}

fn draw_property_handle_preview(
    handle_state: Res<HandleInteractionState>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    let Some(drag) = &handle_state.property_drag else {
        return;
    };

    drag.current
        .draw_preview(&mut gizmos, HANDLE_PARAMETER_COLOR);
    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, drag.current.preview_line_count());
}

fn draw_pivot_indicator(
    pivot_point: Res<PivotPoint>,
    selected_query: Query<Entity, With<Selected>>,
    viewport: HandleViewportQuery,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    if selected_query.is_empty() {
        return;
    }
    let Some(position) = pivot_point.position else {
        return;
    };
    let Some((camera, camera_transform, projection)) = viewport.first_camera() else {
        return;
    };
    let radius = handle_world_radius(camera, camera_transform, projection, position) * 1.2;
    draw_diamond_handle(&mut gizmos, position, radius, PIVOT_COLOR);
    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, 4);
}

fn draw_rotate_ring(world: &World, rotate_ring: RotateRingContext, mut gizmos: Gizmos) {
    if rotate_ring.handle_context.display_mode != HandleDisplayMode::Rotate
        || !rotate_ring.transform_state.is_idle()
        || rotate_ring.selected_query.is_empty()
    {
        return;
    }

    let Some((camera, camera_transform, projection)) = rotate_ring.viewport.first_camera() else {
        return;
    };

    let center = rotate_ring.pivot_point.position.or_else(|| {
        let snapshots = rotate_ring
            .selected_query
            .iter()
            .filter_map(|entity| {
                let entity_ref = world.get_entity(entity).ok()?;
                rotate_ring.registry.capture_snapshot(&entity_ref, world)
            })
            .collect::<Vec<_>>();
        selection_center(&snapshots)
    });
    let Some(center) = center else {
        return;
    };

    let radius = handle_world_radius(camera, camera_transform, projection, center) * 3.0;
    draw_ring(&mut gizmos, center, radius, ROTATE_RING_COLOR);
}

fn draw_selected_handles(
    world: &World,
    selected_handles: SelectedHandleContext,
    mut gizmos: Gizmos,
) {
    if !selected_handles.ownership.is_idle()
        || selected_handles.selected_query.is_empty()
        || selected_handles.handle_state.property_drag.is_some()
    {
        return;
    }

    let Some((camera, camera_transform, projection)) = selected_handles.viewport.first_camera()
    else {
        return;
    };

    for entity in &selected_handles.selected_query {
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        let Some(snapshot) = selected_handles
            .registry
            .capture_snapshot(&entity_ref, world)
        else {
            continue;
        };

        let Some(handles) = build_display_handles(
            entity,
            &snapshot,
            selected_handles.handle_context.display_mode,
            selected_handles.pivot_point.position,
        ) else {
            continue;
        };

        for handle in handles {
            let highlighted = selected_handles
                .handle_state
                .hovered
                .as_ref()
                .map(|hovered| hovered.matches(entity, &handle.id))
                .unwrap_or(false)
                || selected_handles
                    .handle_state
                    .pressed
                    .as_ref()
                    .map(|pressed| pressed.handle.matches(entity, &handle.id))
                    .unwrap_or(false);
            let radius = handle_world_radius(camera, camera_transform, projection, handle.position);
            draw_handle(&mut gizmos, &handle, radius, highlighted);
        }
    }
}

fn resolve_entity_handles(
    world: &World,
    registry: &CapabilityRegistry,
    entity: Entity,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    display_mode: HandleDisplayMode,
    pivot_position: Option<Vec3>,
) -> Option<Vec<ResolvedHandle>> {
    let entity_ref = world.get_entity(entity).ok()?;
    let snapshot = registry.capture_snapshot(&entity_ref, world)?;
    let viewport_offset = camera
        .logical_viewport_rect()
        .map(|rect| rect.min)
        .unwrap_or(Vec2::ZERO);
    let handles = build_display_handles(entity, &snapshot, display_mode, pivot_position)?
        .into_iter()
        .filter_map(|handle| {
            let viewport_pos = camera
                .world_to_viewport(camera_transform, handle.position)
                .ok()?;
            Some(ResolvedHandle {
                screen_position: viewport_pos + viewport_offset,
                ..handle
            })
        })
        .collect::<Vec<_>>();
    Some(handles)
}

fn build_display_handles(
    entity: Entity,
    snapshot: &BoxedEntity,
    display_mode: HandleDisplayMode,
    pivot_position: Option<Vec3>,
) -> Option<Vec<ResolvedHandle>> {
    let mut handles = snapshot
        .handles()
        .into_iter()
        .map(|handle| ResolvedHandle {
            entity,
            snapshot: snapshot.clone(),
            id: handle.id.clone(),
            label: handle.label.clone(),
            position: handle.position,
            render_style: authored_render_style(handle.kind),
            interaction: HandleInteraction::Authored {
                handle_id: handle.id,
            },
            screen_position: Vec2::ZERO,
        })
        .collect::<Vec<_>>();

    let Some(bounds) = snapshot.bounds() else {
        return Some(handles);
    };

    match display_mode {
        HandleDisplayMode::Move => {
            handles.extend(move_handles(entity, snapshot, bounds));
        }
        HandleDisplayMode::Scale => {
            handles.extend(scale_handles(entity, snapshot, bounds));
        }
        HandleDisplayMode::Rotate => {
            handles.extend(rotate_handles(
                entity,
                snapshot,
                pivot_position.unwrap_or(bounds.center()),
            ));
        }
    }

    Some(handles)
}

fn move_handles(
    entity: Entity,
    snapshot: &BoxedEntity,
    bounds: EntityBounds,
) -> Vec<ResolvedHandle> {
    bounds
        .corners()
        .into_iter()
        .enumerate()
        .map(|(index, position)| ResolvedHandle {
            entity,
            snapshot: snapshot.clone(),
            id: format!("move_corner_{index}"),
            label: "Move".to_string(),
            position,
            render_style: HandleRenderStyle::Move,
            interaction: HandleInteraction::Transform {
                mode: TransformMode::Moving,
                axis: AxisConstraint::None,
                pivot_override: None,
            },
            screen_position: Vec2::ZERO,
        })
        .collect()
}

fn scale_handles(
    entity: Entity,
    snapshot: &BoxedEntity,
    bounds: EntityBounds,
) -> Vec<ResolvedHandle> {
    let center = bounds.center();
    let mut handles = bounds
        .corners()
        .into_iter()
        .enumerate()
        .map(|(index, position)| ResolvedHandle {
            entity,
            snapshot: snapshot.clone(),
            id: format!("scale_corner_{index}"),
            label: "Scale".to_string(),
            position,
            render_style: HandleRenderStyle::Scale,
            interaction: HandleInteraction::Transform {
                mode: TransformMode::Scaling,
                axis: AxisConstraint::None,
                pivot_override: Some(mirrored_pivot(center, position)),
            },
            screen_position: Vec2::ZERO,
        })
        .collect::<Vec<_>>();

    handles.extend(bounds.face_centers().into_iter().enumerate().map(
        |(index, (position, normal))| {
            let axis = axis_for_direction(normal);
            ResolvedHandle {
                entity,
                snapshot: snapshot.clone(),
                id: format!("scale_face_{index}"),
                label: format!("Scale {}", axis_label(axis)),
                position,
                render_style: HandleRenderStyle::Scale,
                interaction: HandleInteraction::Transform {
                    mode: TransformMode::Scaling,
                    axis,
                    pivot_override: Some(mirrored_pivot(center, position)),
                },
                screen_position: Vec2::ZERO,
            }
        },
    ));

    handles
}

fn rotate_handles(entity: Entity, snapshot: &BoxedEntity, position: Vec3) -> Vec<ResolvedHandle> {
    vec![ResolvedHandle {
        entity,
        snapshot: snapshot.clone(),
        id: "rotate_center".to_string(),
        label: "Rotate".to_string(),
        position,
        render_style: HandleRenderStyle::Rotate,
        interaction: HandleInteraction::Transform {
            mode: TransformMode::Rotating,
            axis: AxisConstraint::None,
            pivot_override: Some(position),
        },
        screen_position: Vec2::ZERO,
    }]
}

fn authored_render_style(kind: HandleKind) -> HandleRenderStyle {
    match kind {
        HandleKind::Vertex => HandleRenderStyle::Vertex,
        HandleKind::Center => HandleRenderStyle::Center,
        HandleKind::Control => HandleRenderStyle::Control,
        HandleKind::Parameter => HandleRenderStyle::Parameter,
    }
}

fn axis_for_direction(direction: Vec3) -> AxisConstraint {
    let absolute = direction.abs();
    if absolute.x >= absolute.y && absolute.x >= absolute.z {
        AxisConstraint::X
    } else if absolute.y >= absolute.z {
        AxisConstraint::Y
    } else {
        AxisConstraint::Z
    }
}

fn axis_label(axis: AxisConstraint) -> &'static str {
    match axis {
        AxisConstraint::X => "X",
        AxisConstraint::Y => "Y",
        AxisConstraint::Z => "Z",
        AxisConstraint::PlaneYZ => "YZ",
        AxisConstraint::PlaneXZ => "XZ",
        AxisConstraint::PlaneXY => "XY",
        AxisConstraint::Custom(_) => "Custom",
        AxisConstraint::None => "Uniform",
    }
}

fn draw_handle(gizmos: &mut Gizmos, handle: &ResolvedHandle, radius: f32, highlighted: bool) {
    let color = handle_color(handle.render_style, highlighted);
    match handle.render_style {
        HandleRenderStyle::Scale => draw_cube_handle(gizmos, handle.position, radius, color),
        HandleRenderStyle::Move
        | HandleRenderStyle::Rotate
        | HandleRenderStyle::Vertex
        | HandleRenderStyle::Center
        | HandleRenderStyle::Control
        | HandleRenderStyle::Parameter => {
            draw_sphere_handle(gizmos, handle.position, radius, color)
        }
    }
}

fn handle_color(style: HandleRenderStyle, highlighted: bool) -> Color {
    let base = match style {
        HandleRenderStyle::Move => HANDLE_MOVE_COLOR,
        HandleRenderStyle::Scale => HANDLE_SCALE_COLOR,
        HandleRenderStyle::Rotate => HANDLE_ROTATE_COLOR,
        HandleRenderStyle::Vertex => HANDLE_ENDPOINT_COLOR,
        HandleRenderStyle::Center => HANDLE_MOVE_COLOR,
        HandleRenderStyle::Control => HANDLE_ROTATE_COLOR,
        HandleRenderStyle::Parameter => HANDLE_PARAMETER_COLOR,
    };

    if !highlighted {
        return base;
    }

    let linear = base.to_linear();
    Color::linear_rgba(
        (linear.red * HANDLE_HOVER_BRIGHTNESS).min(1.0),
        (linear.green * HANDLE_HOVER_BRIGHTNESS).min(1.0),
        (linear.blue * HANDLE_HOVER_BRIGHTNESS).min(1.0),
        linear.alpha,
    )
}

fn handle_world_radius(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    projection: &Projection,
    handle_position: Vec3,
) -> f32 {
    let viewport_height = camera
        .logical_viewport_size()
        .map(|size| size.y.max(1.0))
        .unwrap_or(1.0);
    let diameter = match projection {
        Projection::Perspective(perspective) => {
            let distance = camera_transform
                .translation()
                .distance(handle_position)
                .max(0.001);
            2.0 * distance
                * (perspective.fov * 0.5).tan()
                * (HANDLE_SCREEN_DIAMETER_PX / viewport_height)
        }
        Projection::Orthographic(orthographic) => {
            let height = (orthographic.area.max.y - orthographic.area.min.y).abs();
            height * (HANDLE_SCREEN_DIAMETER_PX / viewport_height)
        }
        _ => {
            // Custom or unknown projection: fall back to a reasonable default
            HANDLE_SCREEN_DIAMETER_PX / viewport_height
        }
    };

    (diameter * 0.5).clamp(HANDLE_MIN_RADIUS_METRES, HANDLE_MAX_RADIUS_METRES)
}

fn draw_cube_handle(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    gizmos.cube(
        Transform::from_translation(center).with_scale(Vec3::splat(radius * 2.0)),
        color,
    );
}

fn draw_sphere_handle(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    gizmos
        .sphere(Isometry3d::from_translation(center), radius, color)
        .resolution(10);
}

fn draw_diamond_handle(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    let half = radius;
    let corners = [
        center + Vec3::new(0.0, 0.0, -half),
        center + Vec3::new(-half, 0.0, 0.0),
        center + Vec3::new(0.0, 0.0, half),
        center + Vec3::new(half, 0.0, 0.0),
    ];
    draw_loop(gizmos, corners, color);
}

fn draw_loop(gizmos: &mut Gizmos, corners: [Vec3; 4], color: Color) {
    for index in 0..corners.len() {
        let next = (index + 1) % corners.len();
        gizmos.line(corners[index], corners[next], color);
    }
}

fn current_cursor_screen(world: &mut World) -> Option<Vec2> {
    world
        .query_filtered::<&Window, With<PrimaryWindow>>()
        .single(world)
        .ok()?
        .cursor_position()
}

fn current_cursor_world(world: &World) -> Option<Vec3> {
    let snap_result = world.resource::<SnapResult>();
    if let Some(position) = snap_result.position.or(snap_result.raw_position) {
        return Some(position);
    }

    world
        .resource::<CursorWorldPos>()
        .snapped
        .or(world.resource::<CursorWorldPos>().raw)
}

fn take_property_handle_drag(world: &mut World) -> Option<PropertyHandleDrag> {
    world
        .resource_mut::<HandleInteractionState>()
        .property_drag
        .take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_and_scale_handles_use_handle_position_as_anchor() {
        let handle_position = Vec3::new(1.0, 2.0, 3.0);
        let fallback = Some(Vec3::new(9.0, 9.0, 9.0));

        assert_eq!(
            transform_handle_initial_cursor(TransformMode::Moving, handle_position, fallback),
            Some(handle_position)
        );
        assert_eq!(
            transform_handle_initial_cursor(TransformMode::Scaling, handle_position, fallback),
            Some(handle_position)
        );
    }

    #[test]
    fn rotate_handles_keep_cursor_based_anchor() {
        let handle_position = Vec3::new(1.0, 2.0, 3.0);
        let fallback = Some(Vec3::new(9.0, 9.0, 9.0));

        assert_eq!(
            transform_handle_initial_cursor(TransformMode::Rotating, handle_position, fallback),
            fallback
        );
    }
}

fn clear_pending_handle_press(world: &mut World) {
    world.resource_mut::<HandleInteractionState>().pressed = None;
}

fn cleanup_property_drag_preview(world: &mut World, drag: &mut PropertyHandleDrag) {
    if let Some(preview_entity) = drag.preview_entity.take() {
        drag.current.cleanup_preview_entity(world, preview_entity);
    }
    world.resource_mut::<HandleInteractionState>().property_drag = None;
}

fn mirrored_pivot(center: Vec3, handle_position: Vec3) -> Vec3 {
    center - (handle_position - center)
}

fn selection_center(snapshots: &[BoxedEntity]) -> Option<Vec3> {
    let count = snapshots.len();
    if count == 0 {
        return None;
    }

    Some(
        snapshots
            .iter()
            .map(BoxedEntity::center)
            .fold(Vec3::ZERO, |sum, center| sum + center)
            / count as f32,
    )
}

fn draw_ring(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    let mut previous = None;
    for index in 0..=ROTATE_RING_SEGMENTS {
        let angle = (index as f32 / ROTATE_RING_SEGMENTS as f32) * std::f32::consts::TAU;
        let point = center + Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius);
        if let Some(previous) = previous {
            gizmos.line(previous, point, color);
        }
        previous = Some(point);
    }
}
