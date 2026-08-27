//! Viewport selection of generated edges.
//!
//! Edges are subobjects, not authored elements. The Model API already names
//! them with [`GeneratedEdgeRef`] and already carries a selection of them in
//! [`SubobjectSelection`]; what was missing was the viewport half — until now
//! an agent could address an edge over MCP but a user could not point at one.
//! This module supplies the pointing, and writes into the same selection
//! resource with the same references, so "edge 3 of this box" means one thing
//! whether it was named by a click or by a tool call. No second edge identity,
//! no second selection store.
//!
//! # Cost
//!
//! Picking is screen-space against a cached edge set for the single entity
//! currently being subobject-edited — never a scene-wide or per-frame per-edge
//! ray-cast. The cache is rebuilt only when the target entity changes or its
//! authored body changes, so orbiting, hovering and idling do no topology work
//! at all. Bodies with more than [`MAX_PICKABLE_EDGES`] edges are declined
//! rather than drawn and picked slowly.

use bevy::{
    ecs::system::SystemParam,
    gizmos::config::{GizmoConfigGroup, GizmoConfigStore},
    prelude::*,
    window::PrimaryWindow,
};
use std::collections::HashSet;

use crate::{
    capability_registry::{
        GeneratedEdgeRef, SelectableSubobjectRef, SubobjectDisplayOverrides, SubobjectSelection,
    },
    plugins::{
        camera::OrbitCamera,
        cursor::cursor_window_position,
        egui_chrome::{ChromeInputCapture, EguiWantsInput},
        face_edit::{face_edit_active, FaceDrawingContext, FaceEditContext},
        identity::ElementId,
        input_ownership::{InputOwnership, InputPhase},
        modeling::subobject_topology::{
            canonical_edge_indices, edge_endpoints, evaluated_subobject_mesh,
            generated_edge_ref_for_half_edge, subobject_body_changed,
        },
        scene_ray::PickApertures,
        tools::ActiveTool,
        transform::TransformVisualSystems,
    },
};

/// Fraction of a narrow strip's width that each of its two bounding edges may
/// claim. The remaining third in the middle is left to the face, so a face
/// never becomes unclickable however thin it is on screen.
const STRIP_EDGE_SHARE: f32 = 1.0 / 3.0;
/// How parallel two edges must be before they count as bounding a strip rather
/// than merely converging, as the two edges at a box corner do.
const STRIP_PARALLEL_DOT: f32 = 0.9;
/// How far past the aperture to look for the far side of a strip. Three
/// apertures is enough to see the partner edge of any strip narrow enough to
/// matter, and keeps the scan bounded.
const STRIP_SEARCH_FACTOR: f32 = 3.0;
/// Screen distances within this many pixels of each other count as a tie, and
/// the tie is settled by depth so the front edge of a solid wins over the back.
const EDGE_DEPTH_TIE_PX: f32 = 2.0;
/// Press and release must land within this distance to count as a click rather
/// than a drag. Matches the face picker's slop.
const EDGE_CLICK_SLOP_PX: f32 = 6.0;
/// Upper bound on the pickable edge set for one body. Beyond this the feature
/// declines rather than degrading the frame.
pub const MAX_PICKABLE_EDGES: usize = 4096;

/// Near-black, the way SketchUp draws edges: a lit surface is always lighter
/// than this, so the pickable edges read against any material.
const EDGE_CAGE_COLOR: Color = Color::srgba(0.06, 0.07, 0.09, 0.85);
const EDGE_HOVER_COLOR: Color = Color::srgb(0.3, 0.7, 1.0);
/// SketchUp's selected-edge blue.
const EDGE_SELECTED_COLOR: Color = Color::srgb(0.25, 0.45, 1.0);

/// Thin linework: the pickable edge cage and the hover highlight.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct EdgeCageGizmos;

/// Selected edges, drawn heavier and in front of the surface so a selection on
/// the far side of a solid stays legible.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct EdgeSelectionGizmos;

/// One pickable edge of the subobject-edit target, in world space.
#[derive(Debug, Clone)]
pub struct PickableEdge {
    pub edge: GeneratedEdgeRef,
    pub start: Vec3,
    pub end: Vec3,
}

/// The pickable edge set of the entity currently being subobject-edited.
#[derive(Resource, Default, Debug)]
pub struct EdgeCage {
    entity: Option<Entity>,
    element_id: Option<ElementId>,
    edges: Vec<PickableEdge>,
    /// Set when the body has more edges than the picker will handle.
    declined: bool,
    /// How many times the set has been derived from topology. The picker's
    /// cost claim is that this does not advance while the body is unchanged.
    builds: u64,
}

impl EdgeCage {
    pub fn edges(&self) -> &[PickableEdge] {
        &self.edges
    }

    pub fn element_id(&self) -> Option<ElementId> {
        self.element_id
    }

    pub fn declined(&self) -> bool {
        self.declined
    }

    pub fn builds(&self) -> u64 {
        self.builds
    }

    fn clear(&mut self) {
        let builds = self.builds;
        *self = Self {
            builds,
            ..Self::default()
        };
    }
}

#[derive(Debug, Clone)]
pub struct EdgeHit {
    pub element_id: ElementId,
    pub edge: GeneratedEdgeRef,
    pub start: Vec3,
    pub end: Vec3,
}

impl EdgeHit {
    pub fn reference(&self) -> SelectableSubobjectRef {
        SelectableSubobjectRef::Edge {
            element_id: self.element_id.0,
            edge: self.edge.clone(),
        }
    }
}

#[derive(Resource, Default, Debug)]
pub struct HoveredEdge {
    pub hit: Option<EdgeHit>,
}

/// Press/release bookkeeping, plus the flag that tells the face picker this
/// click already belongs to an edge.
#[derive(Resource, Default, Debug)]
pub struct EdgePressCapture {
    hit: Option<EdgeHit>,
    cursor: Option<Vec2>,
    shift_held: bool,
    /// True for the remainder of the frame when the edge picker has taken
    /// ownership of the current press or release.
    consumed_pointer: bool,
}

impl EdgePressCapture {
    /// Whether the edge picker owns this frame's pointer event.
    pub fn consumed_pointer(&self) -> bool {
        self.consumed_pointer
    }
}

/// Ordering anchor: the edge picker resolves a click before the face picker
/// sees it, so a click that lands on an edge never also moves the face
/// selection.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgePickSystems;

pub struct EdgeSelectionPlugin;

impl Plugin for EdgeSelectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EdgeCage>()
            .init_resource::<HoveredEdge>()
            .init_resource::<EdgePressCapture>()
            .init_resource::<PickApertures>()
            .init_gizmo_group::<EdgeCageGizmos>()
            .init_gizmo_group::<EdgeSelectionGizmos>()
            .add_systems(Startup, configure_edge_gizmos)
            .add_systems(
                Update,
                (sync_edge_cage, update_hovered_edge, handle_edge_click)
                    .chain()
                    .in_set(EdgePickSystems)
                    .in_set(InputPhase::ToolInput)
                    .run_if(in_state(ActiveTool::Select)),
            )
            .add_systems(
                Update,
                draw_edge_overlays
                    .after(TransformVisualSystems::PreviewDraw)
                    .run_if(face_edit_active)
                    .run_if(in_state(ActiveTool::Select)),
            );
    }
}

fn configure_edge_gizmos(mut config_store: ResMut<GizmoConfigStore>) {
    let (cage, _) = config_store.config_mut::<EdgeCageGizmos>();
    cage.line.width = 1.0;
    // No depth bias: the cage must be occluded by the solid it belongs to.
    // Biasing it forward pulls the far edges through the near faces, and a
    // box then reads as a wireframe box rather than a solid one.
    cage.depth_bias = 0.0;

    let (selection, _) = config_store.config_mut::<EdgeSelectionGizmos>();
    selection.line.width = 3.0;
    // Selected edges read through the surface, the way the manipulator gizmo
    // does, so a selection you made from the other side is still visible.
    selection.depth_bias = -0.2;
}

/// Rebuilds the pickable edge set when — and only when — the target body changes.
fn sync_edge_cage(world: &mut World) {
    let Some((entity, element_id)) = subobject_edit_target(world) else {
        clear_edge_cage(world);
        return;
    };

    let Ok(entity_ref) = world.get_entity(entity) else {
        clear_edge_cage(world);
        return;
    };

    let body_changed = subobject_body_changed(&entity_ref);
    let display_changed = entity_ref
        .get_ref::<SubobjectDisplayOverrides>()
        .is_some_and(|overrides| overrides.is_changed());
    if world.resource::<EdgeCage>().entity == Some(entity) && !body_changed && !display_changed {
        return;
    }

    let mut cage = build_edge_cage(world, entity, element_id);
    cage.builds = world.resource::<EdgeCage>().builds + 1;
    *world.resource_mut::<EdgeCage>() = cage;
    if world.resource::<HoveredEdge>().hit.is_some() {
        world.resource_mut::<HoveredEdge>().hit = None;
    }
}

/// Drops the cache without waking change detection when it is already empty —
/// this runs every frame the Select tool is active, face editing or not.
fn clear_edge_cage(world: &mut World) {
    if world.resource::<EdgeCage>().entity.is_some() {
        world.resource_mut::<EdgeCage>().clear();
    }
    if world.resource::<HoveredEdge>().hit.is_some() {
        world.resource_mut::<HoveredEdge>().hit = None;
    }
}

fn build_edge_cage(world: &World, entity: Entity, element_id: ElementId) -> EdgeCage {
    let mut cage = EdgeCage {
        entity: Some(entity),
        element_id: Some(element_id),
        ..EdgeCage::default()
    };
    let Ok(entity_ref) = world.get_entity(entity) else {
        return cage;
    };
    let Some(mesh) = evaluated_subobject_mesh(&entity_ref) else {
        return cage;
    };

    let canonical_edges = canonical_edge_indices(&mesh);
    if canonical_edges.len() > MAX_PICKABLE_EDGES {
        cage.declined = true;
        return cage;
    }

    let overrides = entity_ref.get::<SubobjectDisplayOverrides>();
    cage.edges.reserve(canonical_edges.len());
    for canonical in canonical_edges {
        let edge = generated_edge_ref_for_half_edge(&mesh, &entity_ref, canonical);
        // Hidden edges stay addressable by reference but are not pickable, as
        // the Model API's subobject listing promises.
        if overrides.is_some_and(|overrides| overrides.is_edge_hidden(&edge)) {
            continue;
        }
        let Some((start, end)) = edge_endpoints(&mesh, canonical) else {
            continue;
        };
        cage.edges.push(PickableEdge { edge, start, end });
    }
    cage
}

/// The entity whose subobjects are currently pickable.
fn subobject_edit_target(world: &World) -> Option<(Entity, ElementId)> {
    let face_context = world.resource::<FaceEditContext>();
    if !face_context.is_active() || world.resource::<FaceDrawingContext>().active {
        return None;
    }
    if let Some((entity, element_id)) = face_context.csg_operand_target {
        return Some((entity, element_id));
    }
    Some((face_context.entity?, face_context.element_id?))
}

#[derive(SystemParam)]
struct EdgeHoverContext<'w, 's> {
    window_query: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    camera_query: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<OrbitCamera>>,
    ownership: Res<'w, InputOwnership>,
    chrome: Res<'w, ChromeInputCapture>,
    egui: Res<'w, EguiWantsInput>,
    apertures: Res<'w, PickApertures>,
}

impl EdgeHoverContext<'_, '_> {
    fn wants_pointer(&self) -> bool {
        self.chrome.wants_any_pointer_input() || self.egui.wants_any_pointer_input()
    }

    /// Cursor and camera in one space. `world_to_viewport` already maps through
    /// `logical_viewport_rect()`, so the cursor stays in window space and no
    /// viewport offset is applied on either side of the comparison — the
    /// convention `scene_ray::pick_ray_at` records for rays.
    fn cursor_and_camera(&self) -> Option<(Vec2, &Camera, &GlobalTransform)> {
        let window = self.window_query.single().ok()?;
        let (camera, camera_transform) = self.camera_query.iter().next()?;
        let cursor = cursor_window_position(window)?;
        Some((cursor, camera, camera_transform))
    }
}

fn update_hovered_edge(
    cage: Res<EdgeCage>,
    context: EdgeHoverContext,
    mut hovered: ResMut<HoveredEdge>,
    mut press: ResMut<EdgePressCapture>,
) {
    // One reset point per frame, before anything can claim the pointer.
    press.consumed_pointer = false;

    let hit = if !context.ownership.is_idle() || context.wants_pointer() {
        None
    } else if let (Some(element_id), Some((cursor, camera, camera_transform))) =
        (cage.element_id, context.cursor_and_camera())
    {
        nearest_edge(
            cage.edges(),
            element_id,
            cursor,
            camera_transform.translation(),
            context.apertures.edge_px,
            |point| camera.world_to_viewport(camera_transform, point).ok(),
        )
    } else {
        None
    };

    if hovered.hit.is_some() || hit.is_some() {
        hovered.hit = hit;
    }
}

/// One projected edge, with everything the arbitration needs about it.
struct ProjectedEdge<'a> {
    edge: &'a PickableEdge,
    /// Screen distance from the cursor to the segment, in logical pixels.
    distance: f32,
    /// Which side of the edge's line the cursor falls on.
    side: f32,
    /// Screen-space direction of the edge, normalised.
    direction: Vec2,
    /// Distance from the camera to the closest point along the edge.
    depth: f32,
}

/// Nearest pickable edge to the cursor in screen space.
///
/// `project` maps world space to viewport pixels and returns `None` for points
/// the camera cannot see; an edge with an unprojectable end — one crossing
/// behind the camera — is skipped rather than picked at a bogus position.
fn nearest_edge(
    edges: &[PickableEdge],
    element_id: ElementId,
    cursor: Vec2,
    camera_position: Vec3,
    aperture: f32,
    project: impl Fn(Vec3) -> Option<Vec2>,
) -> Option<EdgeHit> {
    // Reach past the aperture far enough to find the edge on the other side of
    // a narrow strip, which is what tells us the strip is narrow.
    let search = aperture * STRIP_SEARCH_FACTOR;
    let mut candidates: Vec<ProjectedEdge> = Vec::new();
    for edge in edges {
        let (Some(start), Some(end)) = (project(edge.start), project(edge.end)) else {
            continue;
        };
        let (distance, t) = point_to_segment(cursor, start, end);
        if distance > search {
            continue;
        }
        let span = end - start;
        candidates.push(ProjectedEdge {
            edge,
            distance,
            side: span.perp_dot(cursor - start),
            direction: span.normalize_or_zero(),
            depth: edge.start.lerp(edge.end, t).distance(camera_position),
        });
    }

    let best = candidates
        .iter()
        .filter(|candidate| candidate.distance <= aperture)
        .min_by(|left, right| {
            let bucket =
                |candidate: &ProjectedEdge| (candidate.distance / EDGE_DEPTH_TIE_PX).floor();
            bucket(left)
                .total_cmp(&bucket(right))
                .then(left.depth.total_cmp(&right.depth))
        })?;

    if best.distance > yielding_aperture(best, &candidates, aperture) {
        return None;
    }

    Some(EdgeHit {
        element_id,
        edge: best.edge.edge.clone(),
        start: best.edge.start,
        end: best.edge.end,
    })
}

/// The aperture, narrowed so it can never swallow a thin face whole.
///
/// A fixed aperture is right in principle but wrong at the point where a face
/// is narrower on screen than the bands its own edges project. A 20 mm seam rib
/// a few pixels across has no interior left: every pixel of it is within reach
/// of an edge, so the face cannot be clicked at all, at any zoom, and the user
/// sees selection simply stop working.
///
/// So when two roughly parallel edges bracket the cursor, they bound a strip
/// that is the face between them, and the aperture may claim at most
/// [`STRIP_EDGE_SHARE`] of it from each side — the middle always belongs to the
/// face. Edges that merely converge nearby, such as the two meeting at a box
/// corner, do not bound a strip and do not narrow anything.
fn yielding_aperture(best: &ProjectedEdge, candidates: &[ProjectedEdge], aperture: f32) -> f32 {
    candidates
        .iter()
        .filter(|other| {
            // Parallel, and on the far side of the cursor: together with `best`
            // they enclose it.
            best.direction.dot(other.direction).abs() >= STRIP_PARALLEL_DOT
                && best.side * other.side < 0.0
        })
        .map(|other| (best.distance + other.distance) * STRIP_EDGE_SHARE)
        .fold(aperture, f32::min)
}

/// Distance from `point` to segment `a`–`b`, with the parameter of the closest
/// point along the segment.
fn point_to_segment(point: Vec2, a: Vec2, b: Vec2) -> (f32, f32) {
    let segment = b - a;
    let length_squared = segment.length_squared();
    if length_squared < f32::EPSILON {
        return (point.distance(a), 0.0);
    }
    let t = ((point - a).dot(segment) / length_squared).clamp(0.0, 1.0);
    (point.distance(a + segment * t), t)
}

fn handle_edge_click(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window_query: Query<&Window, With<PrimaryWindow>>,
    hovered: Res<HoveredEdge>,
    ownership: Res<InputOwnership>,
    mut press: ResMut<EdgePressCapture>,
    mut selection: ResMut<SubobjectSelection>,
    mut face_context: ResMut<FaceEditContext>,
) {
    if !ownership.is_idle() {
        press.hit = None;
        return;
    }

    let cursor = window_query.single().ok().and_then(cursor_window_position);

    if mouse_buttons.just_pressed(MouseButton::Left) {
        press.hit = hovered.hit.clone();
        press.cursor = cursor;
        press.shift_held = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        // Claim the press so the face picker leaves this click alone.
        press.consumed_pointer = press.hit.is_some();
    }

    if !mouse_buttons.just_released(MouseButton::Left) {
        return;
    }

    let Some(hit) = press.hit.take() else {
        return;
    };
    press.consumed_pointer = true;
    let shift_held = std::mem::take(&mut press.shift_held);
    let press_cursor = press.cursor.take();

    if let (Some(press_cursor), Some(cursor)) = (press_cursor, cursor) {
        if press_cursor.distance(cursor) > EDGE_CLICK_SLOP_PX {
            return;
        }
    }

    apply_edge_click(&mut selection, &mut face_context, &hit, shift_held);
}

/// Click semantics: plain click replaces the subobject selection with this
/// edge, shift-click toggles it, so an edge can be deselected the same way it
/// was selected.
fn apply_edge_click(
    selection: &mut SubobjectSelection,
    face_context: &mut FaceEditContext,
    hit: &EdgeHit,
    shift_held: bool,
) {
    let reference = hit.reference();
    if shift_held {
        if let Some(index) = selection
            .refs
            .iter()
            .position(|existing| existing == &reference)
        {
            selection.refs.remove(index);
        } else {
            selection.refs.push(reference);
        }
        return;
    }

    selection.refs = vec![reference];
    // A plain edge click takes over from any face selection, so the face
    // highlight does not linger over an unrelated edge selection.
    face_context.selected_face = None;
}

/// The edges of the subobject selection that belong to the cage's entity.
pub fn selected_edges(
    selection: &SubobjectSelection,
    element_id: ElementId,
) -> impl Iterator<Item = &GeneratedEdgeRef> {
    selection
        .refs
        .iter()
        .filter_map(move |reference| match reference {
            SelectableSubobjectRef::Edge {
                element_id: owner,
                edge,
            } if *owner == element_id.0 => Some(edge),
            _ => None,
        })
}

fn draw_edge_overlays(
    cage: Res<EdgeCage>,
    hovered: Res<HoveredEdge>,
    selection: Res<SubobjectSelection>,
    mut cage_gizmos: Gizmos<EdgeCageGizmos>,
    mut selection_gizmos: Gizmos<EdgeSelectionGizmos>,
) {
    let Some(element_id) = cage.element_id else {
        return;
    };

    let selected: HashSet<&GeneratedEdgeRef> = selected_edges(&selection, element_id).collect();
    let hovered_edge = hovered.hit.as_ref().map(|hit| &hit.edge);

    for candidate in &cage.edges {
        if selected.contains(&candidate.edge) {
            selection_gizmos.line(candidate.start, candidate.end, EDGE_SELECTED_COLOR);
        } else if hovered_edge == Some(&candidate.edge) {
            cage_gizmos.line(candidate.start, candidate.end, EDGE_HOVER_COLOR);
        } else {
            cage_gizmos.line(candidate.start, candidate.end, EDGE_CAGE_COLOR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_registry::GeneratedFaceRef,
        plugins::modeling::primitives::{BoxPrimitive, ShapeRotation},
    };

    /// Face-edit target: a 2×2×2 box centred on the origin.
    fn cage_app() -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<EdgeCage>()
            .init_resource::<HoveredEdge>()
            .init_resource::<FaceEditContext>()
            .init_resource::<FaceDrawingContext>()
            .add_systems(Update, sync_edge_cage);

        let entity = app
            .world_mut()
            .spawn((
                BoxPrimitive {
                    centre: Vec3::ZERO,
                    half_extents: Vec3::splat(1.0),
                },
                ShapeRotation::default(),
            ))
            .id();
        app.world_mut().resource_mut::<FaceEditContext>().entity = Some(entity);
        app.world_mut().resource_mut::<FaceEditContext>().element_id = Some(ElementId(11));
        (app, entity)
    }

    fn edge_ref(index: u32) -> GeneratedEdgeRef {
        GeneratedEdgeRef::BoundaryOfFace {
            face: GeneratedFaceRef::BoxFace {
                axis: (index % 3) as u8,
                positive: index.is_multiple_of(2),
            },
            edge_index: index,
        }
    }

    fn hit(index: u32) -> EdgeHit {
        EdgeHit {
            element_id: ElementId(7),
            edge: edge_ref(index),
            start: Vec3::ZERO,
            end: Vec3::X,
        }
    }

    #[test]
    fn point_to_segment_measures_perpendicular_distance_and_clamps_to_ends() {
        let (distance, t) = point_to_segment(Vec2::new(5.0, 3.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((distance - 3.0).abs() < 1e-5);
        assert!((t - 0.5).abs() < 1e-5);

        let (distance, t) =
            point_to_segment(Vec2::new(-4.0, 0.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((distance - 4.0).abs() < 1e-5);
        assert_eq!(t, 0.0);

        let (distance, t) =
            point_to_segment(Vec2::new(14.0, 0.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((distance - 4.0).abs() < 1e-5);
        assert_eq!(t, 1.0);
    }

    #[test]
    fn plain_click_replaces_the_subobject_selection() {
        let mut selection = SubobjectSelection {
            refs: vec![SelectableSubobjectRef::Entity { element_id: 7 }],
        };
        let mut face_context = FaceEditContext::default();

        apply_edge_click(&mut selection, &mut face_context, &hit(1), false);

        assert_eq!(selection.refs, vec![hit(1).reference()]);
    }

    #[test]
    fn shift_click_adds_then_removes_the_same_edge() {
        let mut selection = SubobjectSelection::default();
        let mut face_context = FaceEditContext::default();

        apply_edge_click(&mut selection, &mut face_context, &hit(1), true);
        apply_edge_click(&mut selection, &mut face_context, &hit(2), true);
        assert_eq!(selection.refs.len(), 2);

        apply_edge_click(&mut selection, &mut face_context, &hit(1), true);
        assert_eq!(
            selection.refs,
            vec![hit(2).reference()],
            "shift-clicking a selected edge deselects exactly that edge"
        );
    }

    #[test]
    fn entering_face_edit_makes_every_edge_of_the_body_pickable() {
        let (mut app, _) = cage_app();
        app.update();

        let cage = app.world().resource::<EdgeCage>();
        assert_eq!(cage.edges().len(), 12, "a box offers twelve pickable edges");
        assert_eq!(cage.element_id(), Some(ElementId(11)));
        assert!(!cage.declined());
    }

    #[test]
    fn the_cage_is_not_rebuilt_while_the_body_is_unchanged() {
        let (mut app, _) = cage_app();
        app.update();
        assert_eq!(
            app.world().resource::<EdgeCage>().builds(),
            1,
            "the first frame in face-edit mode must build the cage"
        );

        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<EdgeCage>().builds(),
            1,
            "hovering and orbiting must not re-derive topology"
        );
    }

    #[test]
    fn editing_the_body_rebuilds_the_cage() {
        let (mut app, entity) = cage_app();
        app.update();

        app.world_mut()
            .entity_mut(entity)
            .get_mut::<BoxPrimitive>()
            .expect("box primitive")
            .half_extents = Vec3::splat(2.0);
        app.update();

        assert_eq!(
            app.world().resource::<EdgeCage>().builds(),
            2,
            "a push/pull or property edit must refresh the pickable edges"
        );
        let cage = app.world().resource::<EdgeCage>();
        let longest = cage
            .edges()
            .iter()
            .map(|edge| edge.start.distance(edge.end))
            .fold(0.0_f32, f32::max);
        assert!(
            (longest - 4.0).abs() < 1e-4,
            "cage must follow the resized body, longest edge was {longest}"
        );
    }

    #[test]
    fn hidden_edges_are_not_pickable() {
        let (mut app, entity) = cage_app();
        app.update();

        let hidden = app.world().resource::<EdgeCage>().edges()[0].edge.clone();
        let mut overrides = SubobjectDisplayOverrides::default();
        overrides.set_edge_hidden(hidden.clone(), true);
        app.world_mut().entity_mut(entity).insert(overrides);
        app.update();

        let cage = app.world().resource::<EdgeCage>();
        assert_eq!(cage.edges().len(), 11);
        assert!(
            !cage.edges().iter().any(|edge| edge.edge == hidden),
            "a hidden edge stays addressable by reference but must not be pickable"
        );
    }

    #[test]
    fn leaving_face_edit_drops_the_cage() {
        let (mut app, _) = cage_app();
        app.update();
        assert_eq!(app.world().resource::<EdgeCage>().edges().len(), 12);

        app.world_mut().resource_mut::<FaceEditContext>().exit();
        app.update();

        assert!(app.world().resource::<EdgeCage>().edges().is_empty());
    }

    #[test]
    fn picking_prefers_the_edge_nearest_the_cursor_then_the_nearest_to_camera() {
        // Two edges projecting to the same screen line, one 5 m in front of the
        // other — the classic front/back pair on a box seen square-on.
        let edges = vec![
            PickableEdge {
                edge: edge_ref(1),
                start: Vec3::new(0.0, 0.0, 0.0),
                end: Vec3::new(10.0, 0.0, 0.0),
            },
            PickableEdge {
                edge: edge_ref(2),
                start: Vec3::new(0.0, 0.0, 5.0),
                end: Vec3::new(10.0, 0.0, 5.0),
            },
            PickableEdge {
                edge: edge_ref(3),
                start: Vec3::new(0.0, 40.0, 0.0),
                end: Vec3::new(10.0, 40.0, 0.0),
            },
        ];
        // Orthographic-style projection down +Z: x/y map straight to pixels.
        let project = |point: Vec3| Some(Vec2::new(point.x, point.y));
        let camera_position = Vec3::new(0.0, 0.0, 100.0);

        let hit = nearest_edge(
            &edges,
            ElementId(11),
            Vec2::new(5.0, 1.0),
            camera_position,
            6.0,
            project,
        )
        .expect("cursor is within tolerance of an edge");
        assert_eq!(
            hit.edge,
            edge_ref(2),
            "of two edges under the cursor the nearer one to the camera wins"
        );

        assert!(
            nearest_edge(
                &edges,
                ElementId(11),
                Vec2::new(5.0, 20.0),
                camera_position,
                6.0,
                project
            )
            .is_none(),
            "edges further than the tolerance are not picked"
        );
    }

    /// Two parallel edges `width` px apart, seen straight on so screen pixels
    /// and model units coincide — a strip of face between two edges.
    fn strip(width: f32) -> Vec<PickableEdge> {
        vec![
            PickableEdge {
                edge: edge_ref(1),
                start: Vec3::new(0.0, 0.0, 0.0),
                end: Vec3::new(100.0, 0.0, 0.0),
            },
            PickableEdge {
                edge: edge_ref(2),
                start: Vec3::new(0.0, width, 0.0),
                end: Vec3::new(100.0, width, 0.0),
            },
        ]
    }

    fn pick(edges: &[PickableEdge], cursor: Vec2, aperture: f32) -> Option<GeneratedEdgeRef> {
        nearest_edge(
            edges,
            ElementId(11),
            cursor,
            Vec3::Z * 1000.0,
            aperture,
            |point| Some(Vec2::new(point.x, point.y)),
        )
        .map(|hit| hit.edge)
    }

    #[test]
    fn a_face_too_thin_for_the_aperture_keeps_a_clickable_middle() {
        // 9 px across: both 6 px bands cover every pixel of it, so a fixed
        // aperture leaves the face unreachable at any zoom.
        let edges = strip(9.0);

        assert_eq!(
            pick(&edges, Vec2::new(50.0, 4.5), 6.0),
            None,
            "the middle of a thin strip must fall through to the face"
        );
        assert_eq!(
            pick(&edges, Vec2::new(50.0, 0.5), 6.0),
            Some(edge_ref(1)),
            "aiming at an edge of the strip must still pick that edge"
        );
        assert_eq!(
            pick(&edges, Vec2::new(50.0, 8.5), 6.0),
            Some(edge_ref(2)),
            "and the same on the strip's other side"
        );
    }

    #[test]
    fn a_wide_face_gives_the_edge_its_whole_aperture() {
        let edges = strip(400.0);
        assert_eq!(
            pick(&edges, Vec2::new(50.0, 5.5), 6.0),
            Some(edge_ref(1)),
            "with room to spare the aperture is not narrowed at all"
        );
        assert_eq!(
            pick(&edges, Vec2::new(50.0, 6.5), 6.0),
            None,
            "and it still ends where the aperture ends"
        );
    }

    #[test]
    fn converging_edges_do_not_narrow_the_aperture() {
        // The two edges meeting at a box corner are not a strip: they enclose
        // no face between them, so neither may narrow the other.
        let edges = vec![
            PickableEdge {
                edge: edge_ref(1),
                start: Vec3::new(0.0, 0.0, 0.0),
                end: Vec3::new(100.0, 0.0, 0.0),
            },
            PickableEdge {
                edge: edge_ref(2),
                start: Vec3::new(0.0, -50.0, 0.0),
                end: Vec3::new(0.0, 50.0, 0.0),
            },
        ];
        // Equidistant from both at 5 px. They do fall on opposite sides of
        // each other, so without the parallel test they would be mistaken for
        // a 10 px strip, which would narrow the aperture to 3.3 px and reject
        // this pick. Which of the two wins does not matter; that one does.
        assert!(
            pick(&edges, Vec2::new(5.0, 5.0), 6.0).is_some(),
            "a corner must stay as pickable as a lone edge"
        );
    }

    #[test]
    fn the_aperture_is_a_setting() {
        let edges = strip(400.0);
        assert_eq!(pick(&edges, Vec2::new(50.0, 9.0), 6.0), None);
        assert_eq!(
            pick(&edges, Vec2::new(50.0, 9.0), 12.0),
            Some(edge_ref(1)),
            "a wider aperture must reach further, so the setting has an effect"
        );
    }

    #[test]
    fn the_aperture_is_worth_less_in_model_units_as_the_view_zooms_in() {
        // Same 6 px aperture, two zoom levels: 10 px per metre, then 100.
        let edges = vec![PickableEdge {
            edge: edge_ref(1),
            start: Vec3::ZERO,
            end: Vec3::new(10.0, 0.0, 0.0),
        }];
        let cursor_at = |metres_off: f32, px_per_m: f32| {
            nearest_edge(
                &edges,
                ElementId(11),
                Vec2::new(5.0 * px_per_m, metres_off * px_per_m),
                Vec3::Z * 1000.0,
                6.0,
                move |point: Vec3| Some(Vec2::new(point.x, point.y) * px_per_m),
            )
            .is_some()
        };

        // 0.4 m off the edge: within 6 px when a metre is 10 px, outside it
        // when the same metre is 100 px.
        assert!(cursor_at(0.4, 10.0), "0.4 m is 4 px zoomed out — picked");
        assert!(!cursor_at(0.4, 100.0), "0.4 m is 40 px zoomed in — missed");
    }

    #[test]
    fn edges_the_camera_cannot_project_are_skipped() {
        let edges = vec![PickableEdge {
            edge: edge_ref(1),
            start: Vec3::ZERO,
            end: Vec3::new(10.0, 0.0, 0.0),
        }];
        let hit = nearest_edge(
            &edges,
            ElementId(11),
            Vec2::new(5.0, 0.0),
            Vec3::Z * 100.0,
            6.0,
            |_| None,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn a_press_on_an_edge_claims_the_pointer_from_the_face_picker() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<HoveredEdge>()
            .init_resource::<EdgePressCapture>()
            .init_resource::<SubobjectSelection>()
            .init_resource::<FaceEditContext>()
            .init_resource::<InputOwnership>()
            .add_systems(Update, handle_edge_click);

        app.world_mut().resource_mut::<HoveredEdge>().hit = Some(hit(1));
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        app.update();

        assert!(
            app.world()
                .resource::<EdgePressCapture>()
                .consumed_pointer(),
            "the face picker must be told this press already belongs to an edge"
        );
        assert!(
            app.world().resource::<SubobjectSelection>().refs.is_empty(),
            "selection changes on release, not on press"
        );

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(MouseButton::Left);
        app.update();

        assert_eq!(
            app.world().resource::<SubobjectSelection>().refs,
            vec![hit(1).reference()],
            "releasing over the pressed edge selects it"
        );
    }

    #[test]
    fn a_press_on_nothing_leaves_the_pointer_to_the_face_picker() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<HoveredEdge>()
            .init_resource::<EdgePressCapture>()
            .init_resource::<SubobjectSelection>()
            .init_resource::<FaceEditContext>()
            .init_resource::<InputOwnership>()
            .add_systems(Update, handle_edge_click);

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        app.update();

        assert!(!app
            .world()
            .resource::<EdgePressCapture>()
            .consumed_pointer());
    }

    #[test]
    fn selected_edges_ignores_other_entities_and_other_subobject_kinds() {
        let selection = SubobjectSelection {
            refs: vec![
                SelectableSubobjectRef::Entity { element_id: 7 },
                SelectableSubobjectRef::Edge {
                    element_id: 7,
                    edge: edge_ref(1),
                },
                SelectableSubobjectRef::Edge {
                    element_id: 99,
                    edge: edge_ref(2),
                },
            ],
        };

        let edges: Vec<_> = selected_edges(&selection, ElementId(7)).collect();
        assert_eq!(edges, vec![&edge_ref(1)]);
    }
}
