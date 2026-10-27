//! Transient cursor guidance. The resolved point feeds existing tool commands and
//! authored edit plans; acquiring or locking a reference never edits the model.
use crate::plugins::{
    camera::OrbitCamera,
    commands::find_entity_by_element_id_readonly,
    cursor::{cursor_window_position, CursorWorldPos, DrawingPlane},
    edge_selection::{build_edge_cage, EdgeCage},
    history::{History, ModelRevision},
    identity::ElementId,
    input_ownership::InputOwnership,
    modeling::tools::polyline_tool,
    snap::{SnapKind, SnapResult, SnapSystems},
    tools::ActiveTool,
    transform::{AxisConstraint, TransformMode, TransformState},
};
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

const APERTURE: f32 = 9.0;
const RELEASE_APERTURE: f32 = 14.0;
const DWELL_SECONDS: f64 = 0.35;
const MAGENTA: Color = Color::srgb(0.85, 0.25, 0.85);
const MAX_REFERENCES: usize = 4;

pub struct InferencePlugin;
impl Plugin for InferencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InferenceEngine>()
            .add_systems(
                Update,
                update_inference
                    .after(crate::plugins::snap::update_snap_result)
                    .in_set(SnapSystems::Resolve),
            )
            .add_systems(
                Update,
                (draw_inference_visuals, draw_inference_tooltip).after(SnapSystems::Resolve),
            );
    }
}

#[derive(Debug, Clone)]
pub struct InferenceCandidate {
    pub kind: InferenceKind,
    pub direction: Option<Vec3>,
    pub distance: Option<f32>,
    pub snap_position: Option<Vec3>,
    pub reference_entity: Option<ElementId>,
    pub label: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceKind {
    Parallel,
    Perpendicular,
    OnFacePlane,
    DistanceMatch,
    Endpoint,
    Axis,
    FromPoint,
    Square,
    Extension,
}
#[derive(Debug, Clone)]
pub struct ReferenceEdge {
    pub start: Vec3,
    pub end: Vec3,
    pub entity_label: String,
    pub element_id: ElementId,
}
#[derive(Debug, Clone)]
pub struct ReferenceFacePlane {
    pub point_on_plane: Vec3,
    pub normal: Vec3,
    pub entity_label: String,
    pub element_id: ElementId,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LinearMode {
    #[default]
    All,
    References,
    Off,
}
#[derive(Clone, Debug)]
struct Reference {
    point: Vec3,
    edge: Option<(Vec3, Vec3)>,
    owner: Option<ElementId>,
}
#[derive(Clone, Debug)]
struct Guide {
    origin: Vec3,
    direction: Vec3,
    kind: InferenceKind,
    label: String,
    owner: Option<ElementId>,
}
impl Guide {
    fn project(&self, point: Vec3) -> Vec3 {
        self.origin + self.direction * (point - self.origin).dot(self.direction)
    }
}

#[derive(Resource, Default)]
pub struct InferenceEngine {
    pub candidates: Vec<InferenceCandidate>,
    pub locked: Option<InferenceCandidate>,
    pub last_distance: Option<f32>,
    pub last_mode: Option<TransformMode>,
    cage: EdgeCage,
    cached_entity: Option<Entity>,
    segments: Vec<(Vec3, Vec3)>,
    revision: Option<ModelRevision>,
    references: Vec<Reference>,
    pending: Option<(Reference, f64)>,
    origin: Option<Vec3>,
    tool: Option<ActiveTool>,
    guide: Option<Guide>,
    held_guide: Option<Guide>,
    arrow_guide: Option<Guide>,
    linear_mode: LinearMode,
    feedback: String,
    marker: Option<Vec3>,
    // Compatibility intake for capability contributors; bounded, never collected
    // scene-wide on each pointer frame.
    reference_edges: Vec<ReferenceEdge>,
    reference_planes: Vec<ReferenceFacePlane>,
    reference_distances: Vec<(f32, String)>,
}
impl InferenceEngine {
    pub fn best_snap_position(&self) -> Option<Vec3> {
        self.locked
            .as_ref()
            .or(self.candidates.first())
            .and_then(|c| c.snap_position)
    }
    pub fn best_label(&self) -> Option<&str> {
        (!self.feedback.is_empty()).then_some(self.feedback.as_str())
    }
    pub fn add_reference_edge(&mut self, edge: ReferenceEdge) {
        if self.reference_edges.len() < 256 {
            self.reference_edges.push(edge);
        }
    }
    pub fn add_reference_face_plane(&mut self, plane: ReferenceFacePlane) {
        if self.reference_planes.len() < 256 {
            self.reference_planes.push(plane);
        }
    }
    pub fn add_reference_distance(&mut self, distance: f32, label: String) {
        if self.reference_distances.len() < 256 {
            self.reference_distances.push((distance, label));
        }
    }
    fn reset_interaction(&mut self) {
        self.candidates.clear();
        self.locked = None;
        self.references.clear();
        self.pending = None;
        self.guide = None;
        self.held_guide = None;
        self.arrow_guide = None;
        self.linear_mode = LinearMode::All;
        self.feedback.clear();
        self.marker = None;
    }
    fn acquire(&mut self, reference: Option<Reference>, now: f64) {
        let Some(reference) = reference else {
            self.pending = None;
            return;
        };
        let same = |a: &Reference, b: &Reference| {
            a.owner == b.owner && a.point.distance_squared(b.point) < 1e-8 && a.edge == b.edge
        };
        if let Some((pending, since)) = &self.pending {
            if same(pending, &reference) {
                if now - since >= DWELL_SECONDS
                    && !self.references.iter().any(|r| same(r, &reference))
                {
                    if self.references.len() == MAX_REFERENCES {
                        self.references.remove(0);
                    }
                    self.references.push(reference);
                }
                return;
            }
        }
        self.pending = Some((reference, now));
    }
}

pub(crate) fn supports_tool(tool: &ActiveTool) -> bool {
    matches!(
        tool,
        ActiveTool::Tape
            | ActiveTool::PlaceGuideLine
            | ActiveTool::PlaceDimensionLine
            | ActiveTool::Select
            | ActiveTool::PlacePolyline
            | ActiveTool::PlaceBox
            | ActiveTool::PlacePlane
            | ActiveTool::PlaceSphere
            | ActiveTool::PlaceCylinder
            | ActiveTool::PlaceDraftLine
            | ActiveTool::PlaceDraftPolyline
            | ActiveTool::PlaceDraftRectangle
            | ActiveTool::PlaceDraftCircle
            | ActiveTool::PlaceDraftText
    )
}
fn tool_origin(world: &World, tool: &ActiveTool) -> Option<Vec3> {
    match tool {
        ActiveTool::Tape => crate::plugins::tape_tool::inference_origin(world),
        ActiveTool::Select => {
            let transform = world.get_resource::<TransformState>()?;
            (transform.mode == TransformMode::Moving && transform.axis == AxisConstraint::None)
                .then_some(transform.initial_cursor)
                .flatten()
        }
        ActiveTool::PlacePolyline => polyline_tool::inference_origin(world),
        ActiveTool::PlaceDraftLine
        | ActiveTool::PlaceDraftPolyline
        | ActiveTool::PlaceDraftRectangle => {
            crate::plugins::drafting::primitive_tool::inference_origin(world)
        }
        _ => None,
    }
}

#[derive(Clone)]
struct View {
    camera: Camera,
    transform: GlobalTransform,
    cursor: Vec2,
    ray: Ray3d,
}
impl View {
    fn from_world(world: &mut World) -> Option<Self> {
        let mut windows = world.query_filtered::<&Window, With<bevy::window::PrimaryWindow>>();
        let cursor = cursor_window_position(windows.single(world).ok()?)?;
        let mut cameras = world.query_filtered::<(&Camera, &GlobalTransform), With<OrbitCamera>>();
        let (camera, transform) = cameras.iter(world).find(|(c, _)| c.is_active)?;
        let ray = crate::plugins::scene_ray::pick_ray_at(camera, transform, cursor)?;
        Some(Self {
            camera: camera.clone(),
            transform: *transform,
            cursor,
            ray,
        })
    }
    fn distance(&self, point: Vec3) -> Option<f32> {
        Some(
            self.camera
                .world_to_viewport(&self.transform, point)
                .ok()?
                .distance(self.cursor),
        )
    }
}
fn near(view: Option<&View>, raw: Vec3, point: Vec3, aperture: f32) -> Option<f32> {
    if !point.is_finite() {
        return None;
    }
    let distance = view.map_or_else(|| Some(raw.distance(point) * 100.0), |v| v.distance(point))?;
    (distance <= aperture).then_some(distance)
}
/// Closest point on a segment to the camera ray, avoiding perspective-incorrect
/// interpolation of projected endpoints. No mesh or per-edge ray casting.
fn segment_point(start: Vec3, end: Vec3, raw: Vec3, view: Option<&View>) -> Option<Vec3> {
    let delta = end - start;
    let length = delta.length();
    let direction = delta.try_normalize()?;
    let t = if let Some(view) = view {
        let ray_dir = *view.ray.direction;
        let w = start - view.ray.origin;
        let dot = direction.dot(ray_dir);
        let denominator = 1.0 - dot * dot;
        if denominator < 1e-5 {
            (raw - start).dot(direction)
        } else {
            (dot * w.dot(ray_dir) - w.dot(direction)) / denominator
        }
    } else {
        (raw - start).dot(direction)
    };
    Some(start + direction * t.clamp(0.0, length))
}
fn line_point(guide: &Guide, raw: Vec3, view: Option<&View>) -> Vec3 {
    let Some(view) = view else {
        return guide.project(raw);
    };
    let ray_dir = *view.ray.direction;
    let dot = guide.direction.dot(ray_dir);
    let denominator = 1.0 - dot * dot;
    if denominator < 1e-5 {
        return guide.project(raw);
    }
    let w = guide.origin - view.ray.origin;
    guide.origin + guide.direction * ((dot * w.dot(ray_dir) - w.dot(guide.direction)) / denominator)
}

pub(crate) fn update_inference(world: &mut World) {
    let tool = world
        .get_resource::<State<ActiveTool>>()
        .map(|s| s.get().clone())
        .unwrap_or(ActiveTool::Select);
    let blocked = world
        .get_resource::<InputOwnership>()
        .is_some_and(InputOwnership::is_ui_capture);
    let raw = world.resource::<SnapResult>().raw_position;
    if blocked || !supports_tool(&tool) || raw.is_none() {
        let mut engine = world.resource_mut::<InferenceEngine>();
        engine.feedback.clear();
        engine.marker = None;
        engine.pending = None;
        if !supports_tool(&tool) {
            engine.reset_interaction();
            engine.tool = Some(tool);
        }
        return;
    }
    let raw = raw.unwrap();
    let origin = tool_origin(world, &tool);
    let plane = world
        .get_resource::<DrawingPlane>()
        .cloned()
        .unwrap_or_default();
    let planar = !matches!(
        tool,
        ActiveTool::Select
            | ActiveTool::Tape
            | ActiveTool::PlaceGuideLine
            | ActiveTool::PlaceDimensionLine
    );
    let view = View::from_world(world);
    let revision = world.get_resource::<History>().map(History::revision_token);
    let (hovered, surface, normal) = {
        let cursor = world.resource::<CursorWorldPos>();
        (
            cursor.hovered_element_id,
            cursor.surface_point,
            cursor.surface_normal,
        )
    };
    let owner = hovered.filter(|id| {
        world.get_resource::<TransformState>().is_none_or(|state| {
            !state
                .initial_snapshots
                .iter()
                .any(|(_, s)| s.element_id() == *id)
        })
    });
    let keys = world.resource::<ButtonInput<KeyCode>>().clone();
    let now = world
        .get_resource::<Time>()
        .map_or(0.0, Time::elapsed_secs_f64);
    world.resource_scope(|world, mut engine: Mut<InferenceEngine>| {
        if engine.tool.as_ref() != Some(&tool)
            || engine.origin != origin
            || engine.revision != revision
        {
            engine.reset_interaction();
            engine.origin = origin;
            engine.tool = Some(tool.clone());
        }
        if engine.revision != revision || engine.tool.is_none() {
            engine.reference_edges.clear();
            engine.reference_planes.clear();
            engine.reference_distances.clear();
            if let Some(registry) =
                world.get_resource::<crate::capability_registry::CapabilityRegistry>()
            {
                for factory in registry.factories() {
                    factory.collect_inference_geometry(world, &mut engine);
                }
            }
        }
        if engine.cage.element_id() != owner || engine.revision != revision {
            engine.cached_entity =
                owner.and_then(|id| find_entity_by_element_id_readonly(world, id));
            engine.cage = engine
                .cached_entity
                .zip(owner)
                .map(|(entity, id)| build_edge_cage(world, entity, id))
                .unwrap_or_default();
            engine.segments.clear();
            if engine.cage.edges().is_empty() && !engine.cage.declined() {
                if let Some(entity) = engine.cached_entity {
                    if let (Some(registry), Ok(entity)) = (
                        world.get_resource::<crate::capability_registry::CapabilityRegistry>(),
                        world.get_entity(entity),
                    ) {
                        if let Some(snapshot) = registry.capture_snapshot(&entity, world) {
                            engine.segments =
                                snapshot.0.snap_segments().into_iter().take(4096).collect();
                        }
                    }
                }
            }
            engine.revision = revision;
        }
        if keys.just_pressed(KeyCode::Escape) {
            engine.reset_interaction();
            return;
        }
        if keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight) {
            engine.feedback = "Inference bypassed · release Ctrl to resume".into();
            engine.marker = None;
            engine.pending = None;
            world.resource_mut::<CursorWorldPos>().snapped = Some(raw);
            return;
        }
        if origin.is_some() && keys.just_pressed(KeyCode::KeyI) {
            engine.linear_mode = match engine.linear_mode {
                LinearMode::All => LinearMode::References,
                LinearMode::References => LinearMode::Off,
                LinearMode::Off => LinearMode::All,
            };
            engine.held_guide = None;
            engine.arrow_guide = None;
        }
        let allowed = |p: Vec3| !planar || (p - plane.origin).dot(plane.normal).abs() < 1e-3;
        let mut point_hit: Option<(f32, Vec3, SnapKind, &'static str, Option<(Vec3, Vec3)>)> = None;
        let mut edge_hit: Option<(f32, Vec3, Vec3, Vec3)> = None;
        let mut offer = |point, kind, label, edge| {
            let visible =
                visible_near_surface(point, surface, normal, view.as_ref().map(|v| v.ray.origin));
            if !allowed(point) || !visible {
                return;
            }
            if let Some(distance) = near(view.as_ref(), raw, point, APERTURE) {
                if point_hit.as_ref().is_none_or(|hit| distance < hit.0) {
                    point_hit = Some((distance, point, kind, label, edge));
                }
            }
        };
        for (start, end) in engine
            .cage
            .edges()
            .iter()
            .map(|edge| (edge.start, edge.end))
            .chain(engine.segments.iter().copied())
            .chain(
                engine
                    .reference_edges
                    .iter()
                    .map(|edge| (edge.start, edge.end)),
            )
        {
            let edge = ReferenceEdge {
                start,
                end,
                entity_label: String::new(),
                element_id: owner.unwrap_or(ElementId(0)),
            };
            offer(
                edge.start,
                SnapKind::Endpoint,
                "Endpoint",
                Some((edge.start, edge.end)),
            );
            offer(
                edge.end,
                SnapKind::Endpoint,
                "Endpoint",
                Some((edge.start, edge.end)),
            );
            offer(
                (edge.start + edge.end) * 0.5,
                SnapKind::Midpoint,
                "Midpoint",
                Some((edge.start, edge.end)),
            );
            if let Some(point) =
                segment_point(edge.start, edge.end, raw, view.as_ref()).filter(|p| allowed(*p))
            {
                if let Some(distance) = near(view.as_ref(), raw, point, APERTURE) {
                    // Reuse the existing surface hit to reject back edges.
                    let visible = visible_near_surface(
                        point,
                        surface,
                        normal,
                        view.as_ref().map(|v| v.ray.origin),
                    );
                    if visible && edge_hit.as_ref().is_none_or(|hit| distance < hit.0) {
                        edge_hit = Some((distance, point, edge.start, edge.end));
                    }
                }
            }
        }
        if let Some(entity) = engine.cached_entity {
            use crate::plugins::modeling::primitives::{CylinderPrimitive, ShapeRotation};
            if let Some(cylinder) = world.get::<CylinderPrimitive>(entity) {
                let rotation = world
                    .get::<ShapeRotation>(entity)
                    .copied()
                    .unwrap_or_default()
                    .0;
                let normal = rotation * Vec3::Y;
                for sign in [-1.0, 1.0] {
                    let center = cylinder.centre + normal * (cylinder.height * 0.5 * sign);
                    offer(center, SnapKind::Center, "Center", None);
                    if let Some(origin) = origin {
                        for tangent in circle_tangents(center, normal, cylinder.radius, origin) {
                            offer(tangent, SnapKind::Control, "Tangent to circle", None);
                        }
                    }
                }
            }
        }
        // The world origin is a genuine point, never an invented object corner.
        offer(Vec3::ZERO, SnapKind::Origin, "Origin", None);
        let base_snap = world.resource::<SnapResult>().clone();
        if keys.just_pressed(KeyCode::Tab) && base_snap.candidates.len() > 1 {
            point_hit = None;
        }
        let mut resolved = base_snap.position.unwrap_or(raw);
        let mut kind = base_snap.kind;
        let mut label = match kind {
            SnapKind::Endpoint => "Endpoint",
            SnapKind::Midpoint => "Midpoint",
            SnapKind::Center => "Center",
            SnapKind::Control => "Control point",
            SnapKind::GuideAnchor => "Guide point",
            _ => "",
        }
        .to_string();
        let mut reference = None;
        if let Some((_, point, point_kind, point_label, edge)) = point_hit {
            resolved = point;
            kind = point_kind;
            label = point_label.into();
            reference = Some(Reference { point, edge, owner });
        } else if matches!(kind, SnapKind::None | SnapKind::Grid) {
            if let Some((_, point, start, end)) = edge_hit {
                resolved = point;
                kind = SnapKind::OnEdge;
                label = "On edge".into();
                reference = Some(Reference {
                    point: (start + end) * 0.5,
                    edge: Some((start, end)),
                    owner,
                });
            } else if let Some(point) = surface.filter(|p| allowed(*p) && owner.is_some()) {
                resolved = point;
                kind = SnapKind::OnFace;
                label = "On face".into();
            }
        } else if allowed(resolved) {
            reference = Some(Reference {
                point: resolved,
                edge: None,
                owner,
            });
        }
        if !allowed(resolved) {
            resolved = raw;
            kind = SnapKind::None;
            label.clear();
        }
        engine.acquire(reference, now);
        if keys.just_pressed(KeyCode::Tab)
            && base_snap.candidates.len() <= 1
            && origin.is_some()
            && tool == ActiveTool::Select
        {
            if let Some(distance) = engine
                .last_distance
                .filter(|_| engine.last_mode == Some(TransformMode::Moving))
            {
                world.resource_mut::<TransformState>().numeric_buffer =
                    Some(format!("{distance:.3}"));
            }
        }
        let exact_point = matches!(
            kind,
            SnapKind::Endpoint
                | SnapKind::Midpoint
                | SnapKind::Center
                | SnapKind::Control
                | SnapKind::GuideAnchor
                | SnapKind::Origin
        );
        let mut guides = Vec::with_capacity(32);
        if let Some(origin) = origin {
            if tool == ActiveTool::PlaceDraftRectangle && engine.linear_mode != LinearMode::Off {
                for direction in [
                    plane.tangent + plane.bitangent,
                    plane.tangent - plane.bitangent,
                ] {
                    guides.push(Guide {
                        origin,
                        direction: direction.normalize(),
                        kind: InferenceKind::Square,
                        label: "Square".into(),
                        owner: None,
                    });
                }
            }
            if engine.linear_mode == LinearMode::All {
                for (direction, name) in [
                    (Vec3::X, "X axis"),
                    (Vec3::Y, "Y axis"),
                    (Vec3::Z, "Z axis"),
                ] {
                    if !planar || direction.dot(plane.normal).abs() < 1e-3 {
                        guides.push(Guide {
                            origin,
                            direction,
                            kind: InferenceKind::Axis,
                            label: name.into(),
                            owner: None,
                        });
                    }
                }
            }
            if engine.linear_mode != LinearMode::Off {
                for reference in engine.references.iter().rev() {
                    if let Some((start, end)) = reference.edge {
                        if let Some(direction) = (end - start).try_normalize() {
                            if !planar || direction.dot(plane.normal).abs() < 1e-3 {
                                guides.push(Guide {
                                    origin: start,
                                    direction,
                                    kind: InferenceKind::Extension,
                                    label: "On edge extension".into(),
                                    owner: reference.owner,
                                });
                                guides.push(Guide {
                                    origin,
                                    direction,
                                    kind: InferenceKind::Parallel,
                                    label: "Parallel to edge".into(),
                                    owner: reference.owner,
                                });
                                // Perpendicular is defined in the active drawing plane, not
                                // an arbitrary direction from an underdetermined 3D relation.
                                if let Some(direction) =
                                    plane.normal.cross(direction).try_normalize()
                                {
                                    guides.push(Guide {
                                        origin,
                                        direction,
                                        kind: InferenceKind::Perpendicular,
                                        label: "Perpendicular to edge".into(),
                                        owner: reference.owner,
                                    });
                                }
                            }
                        }
                    }
                    if engine.linear_mode == LinearMode::All {
                        for direction in [Vec3::X, Vec3::Y, Vec3::Z] {
                            if !planar || direction.dot(plane.normal).abs() < 1e-3 {
                                guides.push(Guide {
                                    origin: reference.point,
                                    direction,
                                    kind: InferenceKind::FromPoint,
                                    label: "From point".into(),
                                    owner: reference.owner,
                                });
                            }
                        }
                    }
                }
            }
            for (key, direction, label) in [
                (KeyCode::ArrowRight, Vec3::X, "X axis"),
                (KeyCode::ArrowUp, Vec3::Y, "Y axis"),
                (KeyCode::ArrowLeft, Vec3::Z, "Z axis"),
            ] {
                if keys.just_pressed(key) && (!planar || direction.dot(plane.normal).abs() < 1e-3) {
                    engine.arrow_guide = if engine
                        .arrow_guide
                        .as_ref()
                        .is_some_and(|g| g.direction == direction)
                    {
                        None
                    } else {
                        Some(Guide {
                            origin,
                            direction,
                            kind: InferenceKind::Axis,
                            label: label.into(),
                            owner: None,
                        })
                    };
                }
            }
            if keys.just_pressed(KeyCode::ArrowDown) {
                engine.arrow_guide = if engine.arrow_guide.is_some() {
                    None
                } else {
                    engine.guide.clone().filter(|g| {
                        matches!(
                            g.kind,
                            InferenceKind::Parallel | InferenceKind::Perpendicular
                        )
                    })
                };
            }
        }
        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        if !shift {
            engine.held_guide = None;
        }
        if shift
            && (keys.just_pressed(KeyCode::ShiftLeft) || keys.just_pressed(KeyCode::ShiftRight))
        {
            engine.held_guide = engine.guide.clone();
        }
        let locked = engine
            .arrow_guide
            .clone()
            .or_else(|| engine.held_guide.clone());
        let mut best = None;
        // Compound inference: a true 3D intersection of two acquired guides.
        // At most 39 guides (three axes plus four references); never scene pairs.
        if locked.is_none() && !exact_point {
            let mut intersection = None;
            for (i, a) in guides.iter().enumerate() {
                for b in guides.iter().skip(i + 1) {
                    if a.origin.distance_squared(b.origin) < 1e-6 {
                        continue;
                    }
                    if let Some(point) = guide_intersection(a, b).filter(|p| allowed(*p)) {
                        if let Some(distance) = near(view.as_ref(), raw, point, APERTURE) {
                            if intersection.is_none_or(|(old, _)| distance < old) {
                                intersection = Some((distance, point));
                            }
                        }
                    }
                }
            }
            if let Some((_, point)) = intersection {
                resolved = point;
                kind = SnapKind::Intersection;
                label = "Intersection of guides".into();
            }
        }

        if let Some(guide) = locked.clone() {
            let point = if exact_point {
                guide.project(resolved)
            } else {
                line_point(&guide, raw, view.as_ref())
            };
            best = Some((0.0, guide, point));
        } else if !exact_point
            && kind != SnapKind::Intersection
            && origin.is_some_and(|origin| raw.distance(origin) > 0.02)
        {
            for guide in guides {
                let point = line_point(&guide, raw, view.as_ref());
                let sticky = engine.guide.as_ref().is_some_and(|old| {
                    old.kind == guide.kind
                        && old.origin == guide.origin
                        && old.direction == guide.direction
                });
                if let Some(distance) = near(
                    view.as_ref(),
                    raw,
                    point,
                    if sticky { RELEASE_APERTURE } else { APERTURE },
                ) {
                    let score = distance - if sticky { 3.0 } else { 0.0 };
                    if best
                        .as_ref()
                        .is_none_or(|(previous, _, _)| score < *previous)
                    {
                        best = Some((score, guide, point));
                    }
                }
            }
        }
        engine.candidates.clear();
        engine.locked = None;
        engine.guide = None;
        if let Some((_, guide, point)) = best.filter(|(_, _, point)| point.is_finite()) {
            resolved = point;
            kind = SnapKind::Inference;
            label = if locked.is_some() {
                format!("{} · Locked", guide.label)
            } else {
                guide.label.clone()
            };
            let candidate = InferenceCandidate {
                kind: guide.kind,
                direction: Some(guide.direction),
                distance: None,
                snap_position: Some(point),
                reference_entity: guide.owner,
                label: label.clone(),
            };
            if locked.is_some() {
                engine.locked = Some(candidate.clone());
            }
            engine.candidates.push(candidate);
            engine.guide = Some(guide);
        }
        if engine
            .references
            .last()
            .is_some_and(|r| r.point.distance(resolved) < 1e-3)
        {
            label.push_str(" · Reference acquired");
        }
        match engine.linear_mode {
            LinearMode::All => {}
            LinearMode::References => {
                if !label.is_empty() {
                    label.push_str(" · ");
                }
                label.push_str("Edge relations only");
            }
            LinearMode::Off => {
                if !label.is_empty() {
                    label.push_str(" · ");
                }
                label.push_str("Linear inference off");
            }
        }
        engine.feedback = label;
        engine.marker = (!matches!(kind, SnapKind::None | SnapKind::Grid)).then_some(resolved);
        let mut snap = world.resource_mut::<SnapResult>();
        snap.position = Some(resolved);
        snap.kind = kind;
        snap.target = engine.marker;
        // Legacy creation tools and their previews consume this published point.
        world.resource_mut::<CursorWorldPos>().snapped = Some(resolved);
    });
}

fn visible_near_surface(
    point: Vec3,
    surface: Option<Vec3>,
    normal: Option<Vec3>,
    eye: Option<Vec3>,
) -> bool {
    let (Some(surface), Some(eye)) = (surface, eye) else {
        return true;
    };
    // A nearby boundary on the hit face is visible even when it is farther
    // along the viewing ray. A fixed depth epsilon alone rejects the lower
    // edges of tall faces as soon as the pointer is a few pixels inside them.
    normal.is_some_and(|normal| (point - surface).dot(normal).abs() < 1e-3)
        || eye.distance(point) <= eye.distance(surface) + 0.02
}

fn circle_tangents(center: Vec3, normal: Vec3, radius: f32, origin: Vec3) -> Vec<Vec3> {
    let offset = origin - center;
    if !radius.is_finite() || radius <= 0.0 || offset.dot(normal).abs() > 1e-3 {
        return Vec::new();
    }
    let squared = offset.length_squared();
    if squared <= radius * radius {
        return Vec::new();
    }
    let base = center + offset * (radius * radius / squared);
    let side = normal.cross(offset) * (radius * (squared - radius * radius).sqrt() / squared);
    vec![base + side, base - side]
}

fn guide_intersection(a: &Guide, b: &Guide) -> Option<Vec3> {
    let dot = a.direction.dot(b.direction);
    let denominator = 1.0 - dot * dot;
    if denominator < 1e-5 {
        return None;
    }
    let offset = b.origin - a.origin;
    let t = (offset.dot(a.direction) - dot * offset.dot(b.direction)) / denominator;
    let point = a.origin + a.direction * t;
    (point.distance_squared(b.project(point)) < 1e-8).then_some(point)
}

fn guide_color(guide: &Guide) -> Color {
    if guide.kind == InferenceKind::Axis || guide.kind == InferenceKind::FromPoint {
        if guide.direction.x.abs() > 0.99 {
            Color::srgb(0.95, 0.3, 0.3)
        } else if guide.direction.y.abs() > 0.99 {
            Color::srgb(0.35, 0.9, 0.35)
        } else {
            Color::srgb(0.35, 0.55, 0.95)
        }
    } else {
        MAGENTA
    }
}
fn draw_inference_visuals(
    engine: Res<InferenceEngine>,
    mut gizmos: Gizmos<crate::plugins::snap::InferenceGizmos>,
) {
    let Some(point) = engine.marker else {
        return;
    };
    if let Some(guide) = &engine.guide {
        let delta = point - guide.origin;
        let color = guide_color(guide);
        if engine.locked.is_some() {
            gizmos.line(guide.origin, point, color);
        } else {
            for i in 0..16 {
                let t = i as f32 / 16.0;
                gizmos.line(
                    guide.origin + delta * t,
                    guide.origin + delta * (t + 0.035),
                    color,
                );
            }
        }
    }
    // At most four acquired references and one bounded guide. Native gizmos
    // batch these lines; there are no egui geometry overlays or scene scans.
    for reference in &engine.references {
        if let Some((start, end)) = reference.edge {
            gizmos.line(start, end, MAGENTA);
        }
    }
}
fn draw_inference_tooltip(
    engine: Res<InferenceEngine>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    ownership: Res<InputOwnership>,
    mut contexts: EguiContexts,
) {
    if engine.feedback.is_empty() || ownership.is_ui_capture() {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = cursor_window_position(window) else {
        return;
    };
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    egui::Area::new(egui::Id::new("inference_tooltip"))
        .fixed_pos(egui::pos2(cursor.x + 18.0, cursor.y + 22.0))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(egui::Label::new(&engine.feedback).wrap_mode(egui::TextWrapMode::Extend));
                if engine.origin.is_some() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("Shift hold · Arrows lock · I cycle · Ctrl bypass")
                                .small(),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                }
            });
        });
}

#[cfg(test)]
mod tests;
