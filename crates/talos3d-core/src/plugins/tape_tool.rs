use std::collections::{HashMap, HashSet};

use bevy::{ecs::world::EntityRef, prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts};
use serde_json::Value;

use crate::{
    authored_entity::EntityBounds,
    capability_registry::CapabilityRegistry,
    plugins::{
        command_registry::{
            activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
            CommandResult,
        },
        cursor::{cursor_window_position, CursorWorldPos},
        document_properties::DocumentProperties,
        drawing_export::ViewportExportState,
        egui_chrome::{ChromeInputCapture, EguiWantsInput},
        identity::ElementId,
        modeling::group::{GroupEditContext, GroupMembers},
        scene_ray,
        snap::{SnapResult, SnapSystems},
        tools::ActiveTool,
        ui::StatusBarData,
    },
};

const TAPE_COLOR: Color = Color::srgb(1.0, 0.82, 0.18);
const TAPE_LAST_COLOR: Color = Color::srgba(1.0, 0.82, 0.18, 0.72);
const TAPE_HOVER_COLOR: Color = Color::srgb(0.25, 0.95, 1.0);
const TAPE_INFERENCE_COLOR: Color = Color::srgba(0.3, 0.9, 1.0, 0.58);
const TAPE_DOT_RADIUS: f32 = 0.055;
const TAPE_MIN_DISTANCE_METRES: f32 = 0.001;
const TAPE_CONTROL_HOVER_RADIUS_PX: f32 = 14.0;
const TAPE_EDGE_HOVER_RADIUS_PX: f32 = 9.0;
const TAPE_AUTO_AXIS_ALIGNMENT: f32 = 0.94;
const TAPE_DEPTH_TIE_METRES: f32 = 0.05;
const TAPE_BOUND_EDGE_INDICES: [(usize, usize); 12] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 0),
    (4, 5),
    (5, 6),
    (6, 7),
    (7, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

pub struct TapeToolPlugin;

impl Plugin for TapeToolPlugin {
    fn build(&self, app: &mut App) {
        app.register_command(
            CommandDescriptor {
                id: "tools.tape".to_string(),
                label: "Tape".to_string(),
                description: "Measure point-to-point distances".to_string(),
                category: CommandCategory::Tools,
                parameters: None,
                default_shortcut: Some("Shift+T".to_string()),
                icon: None,
                hint: Some("Click start point, then click end point to measure".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: Some("Tape".to_string()),
                capability_id: None,
            },
            execute_tape_tool,
        )
        .add_systems(OnEnter(ActiveTool::Tape), initialize_tape_tool)
        .add_systems(OnExit(ActiveTool::Tape), cleanup_tape_tool)
        .add_systems(
            Update,
            (
                update_tape_preview,
                handle_tape_input,
                draw_tape_gizmos,
                draw_tape_overlay.after(draw_tape_gizmos),
            )
                .chain()
                .after(SnapSystems::Resolve)
                .run_if(in_state(ActiveTool::Tape)),
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct TapeMeasurement {
    start: Vec3,
    end: Vec3,
    mode: TapeMeasureMode,
}

impl TapeMeasurement {
    fn distance(self) -> f32 {
        self.start.distance(self.end)
    }

    fn midpoint(self) -> Vec3 {
        (self.start + self.end) * 0.5
    }
}

#[derive(Resource, Default)]
struct TapeToolState {
    start: Option<TapeStart>,
    last_measurement: Option<TapeMeasurement>,
}

#[derive(Debug, Clone)]
struct TapeStart {
    point: Vec3,
    mode: TapeStartMode,
}

#[derive(Debug, Clone)]
enum TapeStartMode {
    Control,
    Edge(TapeEdgeConstraint),
}

#[derive(Debug, Clone)]
struct TapeEdgeConstraint {
    edge_start: Vec3,
    edge_end: Vec3,
    edge_direction: Vec3,
    surface_normal: Vec3,
    perpendicular_direction: Vec3,
}

#[derive(Debug, Clone, Default, Resource)]
struct TapePreview {
    cursor_point: Option<Vec3>,
    hover: Option<TapeHoverTarget>,
    measurement: Option<TapeMeasurement>,
    axis_lock: Option<TapeAxis>,
    inference: Option<TapeInference>,
}

#[derive(Debug, Clone)]
enum TapeHoverTarget {
    Control(TapeControlTarget),
    Edge(TapeEdgeTarget),
}

impl TapeHoverTarget {
    fn point(&self) -> Vec3 {
        match self {
            Self::Control(target) => target.point,
            Self::Edge(target) => target.point,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Control(target) => target.label.as_str(),
            Self::Edge(_) => "Edge",
        }
    }
}

#[derive(Debug, Clone)]
struct TapeControlTarget {
    point: Vec3,
    label: String,
}

#[derive(Debug, Clone)]
struct TapeEdgeTarget {
    start: Vec3,
    end: Vec3,
    point: Vec3,
    constraint: TapeEdgeConstraint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapeAxis {
    X,
    Y,
    Z,
}

impl TapeAxis {
    fn direction(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TapeInference {
    axis: TapeAxis,
    point: Vec3,
    automatic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapeMeasureMode {
    Free,
    PerpendicularToEdge,
    AxisLocked(TapeAxis),
    AxisInferred(TapeAxis),
}

struct TapeCameraContext {
    camera: Camera,
    camera_transform: GlobalTransform,
    camera_position: Vec3,
}

#[derive(Debug, Clone)]
struct TapeHoverCandidate {
    screen_score: f32,
    depth: f32,
    target: TapeHoverTarget,
}

fn execute_tape_tool(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::Tape)
}

fn initialize_tape_tool(mut commands: Commands, mut status: ResMut<StatusBarData>) {
    commands.insert_resource(TapeToolState::default());
    commands.insert_resource(TapePreview::default());
    status.tool_name = "Tape".to_string();
    status.hint =
        "Hover an edge or control point · click to start · hold X/Y/Z to lock axis".to_string();
}

fn cleanup_tape_tool(mut commands: Commands) {
    commands.remove_resource::<TapeToolState>();
    commands.remove_resource::<TapePreview>();
}

fn update_tape_preview(world: &mut World) {
    let hover = resolve_tape_hover(world);
    let raw_point = current_tape_cursor_point(world);
    let cursor_point = hover.as_ref().map(TapeHoverTarget::point).or(raw_point);
    let axis_lock = world
        .get_resource::<ButtonInput<KeyCode>>()
        .and_then(tape_axis_lock);
    let camera_ray = scene_ray::build_camera_ray(world);
    let start = world
        .get_resource::<TapeToolState>()
        .and_then(|state| state.start.clone());
    let measurement = start.as_ref().and_then(|start| {
        resolve_tape_measurement(start, raw_point, hover.as_ref(), axis_lock, camera_ray)
    });
    let inference = measurement.and_then(|measurement| match measurement.mode {
        TapeMeasureMode::AxisLocked(axis) => hover.as_ref().map(|target| TapeInference {
            axis,
            point: target.point(),
            automatic: false,
        }),
        TapeMeasureMode::AxisInferred(axis) => hover.as_ref().map(|target| TapeInference {
            axis,
            point: target.point(),
            automatic: true,
        }),
        _ => None,
    });

    if let Some(mut preview) = world.get_resource_mut::<TapePreview>() {
        *preview = TapePreview {
            cursor_point,
            hover,
            measurement,
            axis_lock,
            inference,
        };
    }
}

fn handle_tape_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    chrome_capture: Res<ChromeInputCapture>,
    egui_wants_input: Res<EguiWantsInput>,
    preview: Res<TapePreview>,
    doc_props: Res<DocumentProperties>,
    mut state: ResMut<TapeToolState>,
    mut status: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if !chrome_capture.wants_any_keyboard_input()
        && !egui_wants_input.wants_any_keyboard_input()
        && keys.just_pressed(KeyCode::Escape)
    {
        if state.start.is_some() || state.last_measurement.is_some() {
            state.start = None;
            state.last_measurement = None;
            status.hint = "Click start point".to_string();
        } else {
            next_active_tool.set(ActiveTool::Select);
        }
        return;
    }

    if chrome_capture.wants_any_pointer_input()
        || egui_wants_input.wants_any_pointer_input()
        || !mouse_buttons.just_pressed(MouseButton::Left)
    {
        return;
    }

    match state.start.as_ref() {
        None => {
            let Some(start) = preview
                .hover
                .as_ref()
                .map(tape_start_from_hover)
                .or_else(|| {
                    preview.cursor_point.map(|point| TapeStart {
                        point,
                        mode: TapeStartMode::Control,
                    })
                })
            else {
                return;
            };
            state.start = Some(start);
            state.last_measurement = None;
            status.hint =
                "Click end point · hold X/Y/Z to lock · hover point/edge to infer".to_string();
        }
        Some(_) => {
            let Some(measurement) = preview.measurement else {
                status.set_feedback("Tape points are too close".to_string(), 1.5);
                return;
            };
            let text = tape_distance_label(measurement.distance(), &doc_props);
            state.start = None;
            state.last_measurement = Some(measurement);
            status.hint =
                "Hover an edge or control point · click to start · hold X/Y/Z to lock axis"
                    .to_string();
            status.set_feedback(format!("Distance: {text}"), 3.0);
        }
    }
}

fn draw_tape_gizmos(
    state: Option<Res<TapeToolState>>,
    preview: Option<Res<TapePreview>>,
    mut gizmos: Gizmos,
) {
    let Some(state) = state else {
        return;
    };
    let Some(preview) = preview else {
        return;
    };

    if let Some(hover) = &preview.hover {
        draw_tape_hover_gizmo(&mut gizmos, hover);
    }

    if let Some(measurement) = preview.measurement {
        draw_tape_measurement_gizmo(&mut gizmos, measurement, TAPE_COLOR);
        draw_tape_inference_gizmo(&mut gizmos, &preview, measurement);
        return;
    }

    if let Some(measurement) = state.last_measurement {
        draw_tape_measurement_gizmo(&mut gizmos, measurement, TAPE_LAST_COLOR);
    }
}

fn draw_tape_hover_gizmo(gizmos: &mut Gizmos, hover: &TapeHoverTarget) {
    match hover {
        TapeHoverTarget::Control(target) => {
            gizmos
                .sphere(
                    Isometry3d::from_translation(target.point),
                    TAPE_DOT_RADIUS * 1.6,
                    TAPE_HOVER_COLOR,
                )
                .resolution(12);
        }
        TapeHoverTarget::Edge(target) => {
            gizmos.line(target.start, target.end, TAPE_HOVER_COLOR);
            gizmos.line(
                target.constraint.edge_start,
                target.constraint.edge_end,
                TAPE_HOVER_COLOR,
            );
            let edge_tick = target.constraint.edge_direction * TAPE_DOT_RADIUS * 3.0;
            gizmos.line(
                target.point - edge_tick,
                target.point + edge_tick,
                TAPE_HOVER_COLOR,
            );
            gizmos.sphere(
                Isometry3d::from_translation(target.point),
                TAPE_DOT_RADIUS,
                TAPE_HOVER_COLOR,
            );
        }
    }
}

fn draw_tape_measurement_gizmo(gizmos: &mut Gizmos, measurement: TapeMeasurement, color: Color) {
    gizmos.line(measurement.start, measurement.end, color);
    gizmos.sphere(
        Isometry3d::from_translation(measurement.start),
        TAPE_DOT_RADIUS,
        color,
    );
    gizmos.sphere(
        Isometry3d::from_translation(measurement.end),
        TAPE_DOT_RADIUS,
        color,
    );
}

fn draw_tape_inference_gizmo(
    gizmos: &mut Gizmos,
    preview: &TapePreview,
    measurement: TapeMeasurement,
) {
    let Some(inference) = preview.inference else {
        return;
    };
    gizmos.line(inference.point, measurement.end, TAPE_INFERENCE_COLOR);
    if preview.axis_lock.is_some() || inference.automatic {
        let axis_tick = inference.axis.direction() * TAPE_DOT_RADIUS * 3.0;
        gizmos.line(
            measurement.end - axis_tick,
            measurement.end + axis_tick,
            TAPE_INFERENCE_COLOR,
        );
        gizmos.sphere(
            Isometry3d::from_translation(inference.point),
            TAPE_DOT_RADIUS * 0.8,
            TAPE_INFERENCE_COLOR,
        );
    }
}

fn draw_tape_overlay(
    mut contexts: EguiContexts,
    viewport_export_state: Res<ViewportExportState>,
    doc_props: Res<DocumentProperties>,
    state: Option<Res<TapeToolState>>,
    preview: Option<Res<TapePreview>>,
    camera_query: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    window_query: Query<&Window, With<PrimaryWindow>>,
) {
    if viewport_export_state.annotation_overlays_suppressed() {
        return;
    }
    let Some(state) = state else {
        return;
    };
    let measurement = preview
        .as_ref()
        .and_then(|preview| preview.measurement)
        .or(state.last_measurement);
    let hover = preview.as_ref().and_then(|preview| preview.hover.as_ref());
    let Some((overlay_point, text)) = measurement
        .map(|measurement| {
            (
                measurement.midpoint(),
                tape_measurement_label(
                    measurement,
                    preview.as_deref().and_then(|preview| preview.inference),
                    &doc_props,
                ),
            )
        })
        .or_else(|| hover.map(|hover| (hover.point(), hover.label().to_string())))
    else {
        return;
    };
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    let Ok(window) = window_query.single() else {
        return;
    };
    let Ok(ctx_ref) = contexts.ctx_mut() else {
        return;
    };
    let Ok(screen_pos) = camera.world_to_viewport(camera_transform, overlay_point) else {
        return;
    };
    if screen_pos.x < 0.0
        || screen_pos.y < 0.0
        || screen_pos.x > window.width()
        || screen_pos.y > window.height()
    {
        return;
    }

    let pos = egui::pos2(screen_pos.x, screen_pos.y - 18.0);
    let rect = egui::Rect::from_center_size(
        pos,
        egui::vec2(text.chars().count() as f32 * 8.0 + 18.0, 24.0),
    );
    let painter = ctx_ref.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("tape_measure_overlay"),
    ));
    painter.rect_filled(rect, 5.0, egui::Color32::from_black_alpha(185));
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 210, 46)),
        egui::StrokeKind::Outside,
    );
    painter.text(
        pos,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(13.0),
        egui::Color32::WHITE,
    );
}

fn tape_cursor_point(cursor_world_pos: &CursorWorldPos, snap_result: &SnapResult) -> Option<Vec3> {
    snap_result
        .position
        .or(snap_result.raw_position)
        .or(cursor_world_pos.snapped)
        .or(cursor_world_pos.raw)
}

fn tape_measurement_with_mode(
    start: Vec3,
    end: Vec3,
    mode: TapeMeasureMode,
) -> Option<TapeMeasurement> {
    (start.distance(end) >= TAPE_MIN_DISTANCE_METRES).then_some(TapeMeasurement {
        start,
        end,
        mode,
    })
}

#[cfg(test)]
fn tape_measurement(start: Vec3, end: Vec3) -> Option<TapeMeasurement> {
    tape_measurement_with_mode(start, end, TapeMeasureMode::Free)
}

fn tape_distance_label(distance_metres: f32, doc_props: &DocumentProperties) -> String {
    doc_props
        .display_unit
        .format_value(distance_metres, doc_props.precision)
}

fn tape_measurement_label(
    measurement: TapeMeasurement,
    inference: Option<TapeInference>,
    doc_props: &DocumentProperties,
) -> String {
    let distance = tape_distance_label(measurement.distance(), doc_props);
    match measurement.mode {
        TapeMeasureMode::Free => distance,
        TapeMeasureMode::PerpendicularToEdge => format!("Perp {distance}"),
        TapeMeasureMode::AxisLocked(axis) => format!("{} {distance}", axis.label()),
        TapeMeasureMode::AxisInferred(axis) => {
            let suffix = if inference
                .map(|inference| inference.automatic)
                .unwrap_or(true)
            {
                " infer"
            } else {
                ""
            };
            format!("{}{suffix} {distance}", axis.label())
        }
    }
}

fn current_tape_cursor_point(world: &World) -> Option<Vec3> {
    let cursor_world_pos = world.get_resource::<CursorWorldPos>()?;
    let snap_result = world.get_resource::<SnapResult>()?;
    tape_cursor_point(cursor_world_pos, snap_result)
}

fn tape_start_from_hover(hover: &TapeHoverTarget) -> TapeStart {
    match hover {
        TapeHoverTarget::Control(target) => TapeStart {
            point: target.point,
            mode: TapeStartMode::Control,
        },
        TapeHoverTarget::Edge(target) => TapeStart {
            point: target.point,
            mode: TapeStartMode::Edge(target.constraint.clone()),
        },
    }
}

fn tape_axis_lock(keys: &ButtonInput<KeyCode>) -> Option<TapeAxis> {
    if keys.pressed(KeyCode::KeyX) {
        Some(TapeAxis::X)
    } else if keys.pressed(KeyCode::KeyY) {
        Some(TapeAxis::Y)
    } else if keys.pressed(KeyCode::KeyZ) {
        Some(TapeAxis::Z)
    } else {
        None
    }
}

fn resolve_tape_measurement(
    start: &TapeStart,
    raw_point: Option<Vec3>,
    hover: Option<&TapeHoverTarget>,
    axis_lock: Option<TapeAxis>,
    camera_ray: Option<Ray3d>,
) -> Option<TapeMeasurement> {
    let mut target = hover.map(TapeHoverTarget::point).or(raw_point)?;
    let mut mode = TapeMeasureMode::Free;

    if let Some(axis) = axis_lock {
        let source = hover.map(TapeHoverTarget::point).unwrap_or(target);
        target = constrain_to_axis(start.point, source, axis);
        mode = TapeMeasureMode::AxisLocked(axis);
    } else if let TapeStartMode::Edge(edge) = &start.mode {
        let surface_point = camera_ray
            .and_then(|ray| scene_ray::project_ray_to_plane(ray, start.point, edge.surface_normal))
            .or(raw_point
                .map(|point| project_point_to_plane(point, start.point, edge.surface_normal)))
            .unwrap_or(target);
        target = constrain_perpendicular_to_edge(start.point, surface_point, edge);
        mode = TapeMeasureMode::PerpendicularToEdge;
    } else if hover.is_some() {
        if let Some(axis) = dominant_axis_delta(target - start.point) {
            target = constrain_to_axis(start.point, target, axis);
            mode = TapeMeasureMode::AxisInferred(axis);
        }
    }

    tape_measurement_with_mode(start.point, target, mode)
}

fn constrain_to_axis(start: Vec3, source: Vec3, axis: TapeAxis) -> Vec3 {
    let direction = axis.direction();
    start + direction * (source - start).dot(direction)
}

fn dominant_axis_delta(delta: Vec3) -> Option<TapeAxis> {
    if delta.length_squared() <= f32::EPSILON {
        return None;
    }
    let normalized = delta.normalize();
    [
        (TapeAxis::X, normalized.x.abs()),
        (TapeAxis::Y, normalized.y.abs()),
        (TapeAxis::Z, normalized.z.abs()),
    ]
    .into_iter()
    .filter(|(_, alignment)| *alignment >= TAPE_AUTO_AXIS_ALIGNMENT)
    .max_by(|(_, a), (_, b)| a.total_cmp(b))
    .map(|(axis, _)| axis)
}

fn project_point_to_plane(point: Vec3, plane_point: Vec3, plane_normal: Vec3) -> Vec3 {
    let normal = plane_normal.normalize_or_zero();
    if normal == Vec3::ZERO {
        return point;
    }
    point - normal * (point - plane_point).dot(normal)
}

fn constrain_perpendicular_to_edge(
    start: Vec3,
    surface_point: Vec3,
    edge: &TapeEdgeConstraint,
) -> Vec3 {
    let signed_distance = (surface_point - start).dot(edge.perpendicular_direction);
    start + edge.perpendicular_direction * signed_distance
}

fn resolve_tape_hover(world: &mut World) -> Option<TapeHoverTarget> {
    let cursor_screen = current_cursor_screen_position(world)?;
    let camera_context = current_camera_context(world)?;
    let group_visibility = TapeGroupVisibility::from_world(world);
    let mut query = world
        .try_query::<EntityRef>()
        .expect("EntityRef query should always be constructible");
    let registry = world.resource::<CapabilityRegistry>();
    let mut best: Option<TapeHoverCandidate> = None;

    for entity_ref in query.iter(world) {
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        if !group_visibility.accepts(snapshot.element_id()) {
            continue;
        }

        for handle in snapshot.handles() {
            add_control_candidate(
                &mut best,
                cursor_screen,
                &camera_context,
                handle.position,
                handle.label,
                0.0,
            );
        }

        if let Some(bounds) = snapshot.bounds() {
            for (index, corner) in bounds.corners().into_iter().enumerate() {
                add_control_candidate(
                    &mut best,
                    cursor_screen,
                    &camera_context,
                    corner,
                    format!("Corner {}", index + 1),
                    1.5,
                );
            }

            let snap_segments = snapshot.0.snap_segments();
            if snap_segments.is_empty() {
                add_bounds_edge_candidates(&mut best, cursor_screen, &camera_context, bounds);
            } else {
                add_edge_segment_candidates(
                    &mut best,
                    cursor_screen,
                    &camera_context,
                    bounds,
                    snap_segments.iter().copied(),
                    2.0,
                );
            }
        }
    }

    best.map(|candidate| candidate.target)
}

struct TapeGroupVisibility {
    active_group: Option<ElementId>,
    group_ids: HashSet<ElementId>,
    parent_by_child: HashMap<ElementId, ElementId>,
}

impl TapeGroupVisibility {
    fn from_world(world: &mut World) -> Self {
        let active_group = world
            .get_resource::<GroupEditContext>()
            .and_then(GroupEditContext::current_group);
        let mut parent_by_child = HashMap::new();
        let mut group_ids = HashSet::new();
        if let Some(mut group_query) = world.try_query::<(&ElementId, &GroupMembers)>() {
            for (group_id, members) in group_query.iter(world) {
                group_ids.insert(*group_id);
                for member_id in &members.member_ids {
                    parent_by_child.entry(*member_id).or_insert(*group_id);
                }
            }
        }

        Self {
            active_group,
            group_ids,
            parent_by_child,
        }
    }

    fn accepts(&self, element_id: ElementId) -> bool {
        if self.group_ids.contains(&element_id) {
            return false;
        }
        match self.active_group {
            Some(active_group) => self.parent_by_child.get(&element_id) == Some(&active_group),
            None => !self.parent_by_child.contains_key(&element_id),
        }
    }
}

fn current_cursor_screen_position(world: &mut World) -> Option<Vec2> {
    let mut window_query = world.query_filtered::<&Window, With<PrimaryWindow>>();
    let window = window_query.iter(world).next()?;
    cursor_window_position(window)
}

fn current_camera_context(world: &mut World) -> Option<TapeCameraContext> {
    let mut camera_query = world.query_filtered::<(&Camera, &GlobalTransform), With<Camera3d>>();
    let (camera, camera_transform) = camera_query.iter(world).next()?;
    Some(TapeCameraContext {
        camera: camera.clone(),
        camera_transform: *camera_transform,
        camera_position: camera_transform.translation(),
    })
}

fn add_control_candidate(
    best: &mut Option<TapeHoverCandidate>,
    cursor_screen: Vec2,
    camera_context: &TapeCameraContext,
    point: Vec3,
    label: String,
    priority_offset: f32,
) {
    let Some(screen) = project_world_to_screen(camera_context, point) else {
        return;
    };
    let distance = screen.distance(cursor_screen);
    if distance > TAPE_CONTROL_HOVER_RADIUS_PX {
        return;
    }
    let Some(depth) = camera_depth(camera_context, point) else {
        return;
    };
    replace_best(
        best,
        TapeHoverCandidate {
            screen_score: distance + priority_offset,
            depth,
            target: TapeHoverTarget::Control(TapeControlTarget { point, label }),
        },
    );
}

fn add_bounds_edge_candidates(
    best: &mut Option<TapeHoverCandidate>,
    cursor_screen: Vec2,
    camera_context: &TapeCameraContext,
    bounds: EntityBounds,
) {
    let corners = bounds.corners();
    let segments = TAPE_BOUND_EDGE_INDICES
        .into_iter()
        .map(|(a_index, b_index)| (corners[a_index], corners[b_index]));
    add_edge_segment_candidates(best, cursor_screen, camera_context, bounds, segments, 4.0);
}

fn add_edge_segment_candidates(
    best: &mut Option<TapeHoverCandidate>,
    cursor_screen: Vec2,
    camera_context: &TapeCameraContext,
    bounds: EntityBounds,
    segments: impl IntoIterator<Item = (Vec3, Vec3)>,
    screen_score_offset: f32,
) {
    for (start, end) in segments {
        let Some(screen_start) = project_world_to_screen(camera_context, start) else {
            continue;
        };
        let Some(screen_end) = project_world_to_screen(camera_context, end) else {
            continue;
        };
        let Some((distance, t)) =
            screen_distance_to_segment(cursor_screen, screen_start, screen_end)
        else {
            continue;
        };
        if distance > TAPE_EDGE_HOVER_RADIUS_PX {
            continue;
        }
        let point = start.lerp(end, t);
        let Some(depth) = camera_depth(camera_context, point) else {
            continue;
        };
        let Some(constraint) = edge_constraint(bounds, start, end, camera_context.camera_position)
        else {
            continue;
        };
        replace_best(
            best,
            TapeHoverCandidate {
                screen_score: distance + screen_score_offset,
                depth,
                target: TapeHoverTarget::Edge(TapeEdgeTarget {
                    start,
                    end,
                    point,
                    constraint,
                }),
            },
        );
    }
}

fn project_world_to_screen(camera_context: &TapeCameraContext, point: Vec3) -> Option<Vec2> {
    camera_context
        .camera
        .world_to_viewport(&camera_context.camera_transform, point)
        .ok()
}

fn camera_depth(camera_context: &TapeCameraContext, point: Vec3) -> Option<f32> {
    let depth = point.distance(camera_context.camera_position);
    depth
        .is_finite()
        .then_some(depth)
        .filter(|depth| *depth > f32::EPSILON)
}

fn screen_distance_to_segment(cursor: Vec2, start: Vec2, end: Vec2) -> Option<(f32, f32)> {
    let segment = end - start;
    let segment_len_sq = segment.length_squared();
    if segment_len_sq < 16.0 {
        return None;
    }
    let t = ((cursor - start).dot(segment) / segment_len_sq).clamp(0.0, 1.0);
    let nearest = start + segment * t;
    Some((cursor.distance(nearest), t))
}

fn replace_best(best: &mut Option<TapeHoverCandidate>, candidate: TapeHoverCandidate) {
    if best
        .as_ref()
        .map(|best| hover_candidate_is_better(&candidate, best))
        .unwrap_or(true)
    {
        *best = Some(candidate);
    }
}

fn hover_candidate_is_better(candidate: &TapeHoverCandidate, best: &TapeHoverCandidate) -> bool {
    if candidate.depth + TAPE_DEPTH_TIE_METRES < best.depth {
        return true;
    }
    if best.depth + TAPE_DEPTH_TIE_METRES < candidate.depth {
        return false;
    }
    candidate.screen_score < best.screen_score
}

fn edge_constraint(
    bounds: EntityBounds,
    edge_start: Vec3,
    edge_end: Vec3,
    camera_position: Vec3,
) -> Option<TapeEdgeConstraint> {
    let edge_delta = edge_end - edge_start;
    let edge_direction = edge_delta.normalize_or_zero();
    if edge_direction == Vec3::ZERO {
        return None;
    }
    let midpoint = (edge_start + edge_end) * 0.5;
    let edge_axis = dominant_component_axis(edge_direction);
    let mut best_normal = None;
    let mut best_score = f32::NEG_INFINITY;
    let view_direction = (camera_position - midpoint).normalize_or_zero();

    for axis in [TapeAxis::X, TapeAxis::Y, TapeAxis::Z] {
        if axis == edge_axis {
            continue;
        }
        let normal = bounds_face_normal(bounds, midpoint, axis);
        let score = if view_direction == Vec3::ZERO {
            0.0
        } else {
            normal.dot(view_direction)
        };
        if score > best_score {
            best_score = score;
            best_normal = Some(normal);
        }
    }

    let surface_normal = best_normal?;
    let perpendicular_direction = surface_normal.cross(edge_direction).normalize_or_zero();
    if perpendicular_direction == Vec3::ZERO {
        return None;
    }

    Some(TapeEdgeConstraint {
        edge_start,
        edge_end,
        edge_direction,
        surface_normal,
        perpendicular_direction,
    })
}

fn dominant_component_axis(direction: Vec3) -> TapeAxis {
    let abs = direction.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        TapeAxis::X
    } else if abs.y >= abs.z {
        TapeAxis::Y
    } else {
        TapeAxis::Z
    }
}

fn bounds_face_normal(bounds: EntityBounds, point: Vec3, axis: TapeAxis) -> Vec3 {
    let min_distance = (axis_value(point, axis) - axis_value(bounds.min, axis)).abs();
    let max_distance = (axis_value(bounds.max, axis) - axis_value(point, axis)).abs();
    let sign = if max_distance <= min_distance {
        1.0
    } else {
        -1.0
    };
    axis.direction() * sign
}

fn axis_value(point: Vec3, axis: TapeAxis) -> f32 {
    match axis {
        TapeAxis::X => point.x,
        TapeAxis::Y => point.y,
        TapeAxis::Z => point.z,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::units::DisplayUnit;
    use crate::plugins::{
        identity::ElementId,
        modeling::group::{GroupEditContext, GroupFrame, GroupMembers},
    };

    fn tape_group_visibility_world() -> (World, ElementId, ElementId, ElementId, ElementId) {
        let mut world = World::new();
        world.insert_resource(GroupEditContext::default());

        let standalone_id = ElementId(1);
        let child_id = ElementId(10);
        let nested_group_id = ElementId(20);
        let root_group_id = ElementId(30);
        world.spawn(standalone_id);
        world.spawn(child_id);
        world.spawn((
            nested_group_id,
            GroupMembers {
                name: "Nested".to_string(),
                member_ids: vec![child_id],
                frame: GroupFrame::default(),
                linked_model: None,
            },
        ));
        world.spawn((
            root_group_id,
            GroupMembers {
                name: "Root".to_string(),
                member_ids: vec![nested_group_id],
                frame: GroupFrame::default(),
                linked_model: None,
            },
        ));

        (
            world,
            standalone_id,
            child_id,
            nested_group_id,
            root_group_id,
        )
    }

    #[test]
    fn tape_rejects_coincident_points() {
        assert!(tape_measurement(Vec3::ZERO, Vec3::splat(0.0001)).is_none());
        assert!(tape_measurement(Vec3::ZERO, Vec3::X).is_some());
    }

    #[test]
    fn tape_distance_label_uses_document_units() {
        let doc_props = DocumentProperties {
            display_unit: DisplayUnit::Millimetres,
            precision: 0,
            ..Default::default()
        };
        assert_eq!(tape_distance_label(1.25, &doc_props), "1250mm");
    }

    #[test]
    fn tape_cursor_prefers_authored_snap_result_over_grid_cursor() {
        let cursor_world_pos = CursorWorldPos {
            raw: Some(Vec3::new(0.1, 0.0, 0.1)),
            snapped: Some(Vec3::new(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        let snap_result = SnapResult {
            raw_position: cursor_world_pos.raw,
            position: Some(Vec3::new(5.0, 2.0, 3.0)),
            ..Default::default()
        };

        assert_eq!(
            tape_cursor_point(&cursor_world_pos, &snap_result),
            Some(Vec3::new(5.0, 2.0, 3.0))
        );
    }

    #[test]
    fn tape_axis_lock_projects_endpoint_onto_locked_axis() {
        let start = TapeStart {
            point: Vec3::new(1.0, 2.0, 3.0),
            mode: TapeStartMode::Control,
        };

        let measurement = resolve_tape_measurement(
            &start,
            Some(Vec3::new(4.0, 9.0, 8.0)),
            None,
            Some(TapeAxis::Z),
            None,
        )
        .expect("axis-locked measurement");

        assert_eq!(measurement.end, Vec3::new(1.0, 2.0, 8.0));
        assert_eq!(measurement.mode, TapeMeasureMode::AxisLocked(TapeAxis::Z));
    }

    #[test]
    fn tape_hover_inference_projects_hovered_point_to_dominant_axis() {
        let start = TapeStart {
            point: Vec3::ZERO,
            mode: TapeStartMode::Control,
        };
        let hover = TapeHoverTarget::Control(TapeControlTarget {
            point: Vec3::new(4.0, 0.1, 0.2),
            label: "corner".to_string(),
        });

        let measurement =
            resolve_tape_measurement(&start, None, Some(&hover), None, None).expect("measurement");

        assert_eq!(measurement.end, Vec3::X * 4.0);
        assert_eq!(measurement.mode, TapeMeasureMode::AxisInferred(TapeAxis::X));
    }

    #[test]
    fn tape_edge_start_constrains_measurement_perpendicular_to_edge_on_surface() {
        let edge = TapeEdgeConstraint {
            edge_start: Vec3::ZERO,
            edge_end: Vec3::X,
            edge_direction: Vec3::X,
            surface_normal: Vec3::Y,
            perpendicular_direction: -Vec3::Z,
        };
        let start = TapeStart {
            point: Vec3::ZERO,
            mode: TapeStartMode::Edge(edge),
        };

        let measurement =
            resolve_tape_measurement(&start, Some(Vec3::new(8.0, 3.0, -2.5)), None, None, None)
                .expect("perpendicular measurement");

        assert_eq!(measurement.end, Vec3::new(0.0, 0.0, -2.5));
        assert_eq!(measurement.mode, TapeMeasureMode::PerpendicularToEdge);
    }

    #[test]
    fn screen_distance_to_segment_reports_nearest_edge_point() {
        let (distance, t) =
            screen_distance_to_segment(Vec2::new(5.0, 3.0), Vec2::ZERO, Vec2::new(10.0, 0.0))
                .expect("screen segment hit");

        assert_eq!(distance, 3.0);
        assert_eq!(t, 0.5);
    }

    #[test]
    fn hover_ranking_prefers_front_candidate_over_screen_closer_back_candidate() {
        let front = TapeHoverCandidate {
            screen_score: 8.0,
            depth: 5.0,
            target: TapeHoverTarget::Control(TapeControlTarget {
                point: Vec3::ZERO,
                label: "front".to_string(),
            }),
        };
        let back = TapeHoverCandidate {
            screen_score: 0.0,
            depth: 8.0,
            target: TapeHoverTarget::Control(TapeControlTarget {
                point: Vec3::Z,
                label: "back".to_string(),
            }),
        };

        assert!(hover_candidate_is_better(&front, &back));
        assert!(!hover_candidate_is_better(&back, &front));
    }

    #[test]
    fn hover_ranking_uses_screen_score_for_near_equal_depth() {
        let closer_screen = TapeHoverCandidate {
            screen_score: 1.0,
            depth: 5.02,
            target: TapeHoverTarget::Control(TapeControlTarget {
                point: Vec3::ZERO,
                label: "closer screen".to_string(),
            }),
        };
        let farther_screen = TapeHoverCandidate {
            screen_score: 4.0,
            depth: 5.0,
            target: TapeHoverTarget::Control(TapeControlTarget {
                point: Vec3::Z,
                label: "farther screen".to_string(),
            }),
        };

        assert!(hover_candidate_is_better(&closer_screen, &farther_screen));
        assert!(!hover_candidate_is_better(&farther_screen, &closer_screen));
    }

    #[test]
    fn camera_depth_does_not_depend_on_signed_forward_convention() {
        let context = TapeCameraContext {
            camera: Camera::default(),
            camera_transform: GlobalTransform::IDENTITY,
            camera_position: Vec3::ZERO,
        };

        assert_eq!(camera_depth(&context, Vec3::Z), Some(1.0));
        assert_eq!(camera_depth(&context, -Vec3::Z), Some(1.0));
    }

    #[test]
    fn tape_hover_visibility_at_root_rejects_grouped_members_and_group_bounds() {
        let (mut world, standalone_id, child_id, nested_group_id, root_group_id) =
            tape_group_visibility_world();

        let visibility = TapeGroupVisibility::from_world(&mut world);

        assert!(visibility.accepts(standalone_id));
        assert!(!visibility.accepts(root_group_id));
        assert!(!visibility.accepts(nested_group_id));
        assert!(!visibility.accepts(child_id));
    }

    #[test]
    fn tape_hover_visibility_inside_group_rejects_nested_group_until_drilled_in() {
        let (mut world, standalone_id, child_id, nested_group_id, root_group_id) =
            tape_group_visibility_world();
        let mut context = GroupEditContext::default();
        context.enter(root_group_id);
        world.insert_resource(context);

        let visibility = TapeGroupVisibility::from_world(&mut world);

        assert!(!visibility.accepts(standalone_id));
        assert!(!visibility.accepts(root_group_id));
        assert!(!visibility.accepts(nested_group_id));
        assert!(!visibility.accepts(child_id));
    }

    #[test]
    fn tape_hover_visibility_inside_nested_group_accepts_nested_members() {
        let (mut world, standalone_id, child_id, nested_group_id, root_group_id) =
            tape_group_visibility_world();
        let mut context = GroupEditContext::default();
        context.enter(root_group_id);
        context.enter(nested_group_id);
        world.insert_resource(context);

        let visibility = TapeGroupVisibility::from_world(&mut world);

        assert!(!visibility.accepts(standalone_id));
        assert!(!visibility.accepts(root_group_id));
        assert!(!visibility.accepts(nested_group_id));
        assert!(visibility.accepts(child_id));
    }
}
