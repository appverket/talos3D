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
        cursor::cursor_viewport_position,
        egui_chrome::{ChromeInputCapture, EguiWantsInput},
        face_edit::{face_edit_active, FaceDrawingContext, FaceEditContext},
        identity::ElementId,
        input_ownership::{InputOwnership, InputPhase},
        modeling::subobject_topology::{
            canonical_edge_indices, edge_endpoints, evaluated_subobject_mesh,
            generated_edge_ref_for_half_edge, subobject_body_changed,
        },
        tools::ActiveTool,
        transform::TransformVisualSystems,
    },
};

/// How close, in logical pixels, the cursor must come to an edge to pick it.
const EDGE_HIT_TOLERANCE_PX: f32 = 6.0;
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
}

impl EdgeHoverContext<'_, '_> {
    fn wants_pointer(&self) -> bool {
        self.chrome.wants_any_pointer_input() || self.egui.wants_any_pointer_input()
    }

    fn cursor_and_camera(&self) -> Option<(Vec2, &Camera, &GlobalTransform)> {
        let window = self.window_query.single().ok()?;
        let (camera, camera_transform) = self.camera_query.iter().next()?;
        let cursor = cursor_viewport_position(window, camera)?;
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
            |point| camera.world_to_viewport(camera_transform, point).ok(),
        )
    } else {
        None
    };

    if hovered.hit.is_some() || hit.is_some() {
        hovered.hit = hit;
    }
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
    project: impl Fn(Vec3) -> Option<Vec2>,
) -> Option<EdgeHit> {
    let mut best: Option<(f32, f32, &PickableEdge)> = None;
    for candidate in edges {
        let Some(start) = project(candidate.start) else {
            continue;
        };
        let Some(end) = project(candidate.end) else {
            continue;
        };
        let (distance, t) = point_to_segment(cursor, start, end);
        if distance > EDGE_HIT_TOLERANCE_PX {
            continue;
        }
        let depth = candidate
            .start
            .lerp(candidate.end, t)
            .distance(camera_position);
        let bucket = (distance / EDGE_DEPTH_TIE_PX).floor();
        if best.is_none_or(|(best_bucket, best_depth, _)| {
            (bucket, depth) < (best_bucket, best_depth)
        }) {
            best = Some((bucket, depth, candidate));
        }
    }

    best.map(|(_, _, candidate)| EdgeHit {
        element_id,
        edge: candidate.edge.clone(),
        start: candidate.start,
        end: candidate.end,
    })
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
    camera_query: Query<&Camera, With<OrbitCamera>>,
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

    let cursor = window_query
        .single()
        .ok()
        .zip(camera_query.iter().next())
        .and_then(|(window, camera)| cursor_viewport_position(window, camera));

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
    selection.refs.iter().filter_map(move |reference| match reference {
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
        app.world_mut()
            .resource_mut::<FaceEditContext>()
            .element_id = Some(ElementId(11));
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

        let (distance, t) = point_to_segment(Vec2::new(-4.0, 0.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((distance - 4.0).abs() < 1e-5);
        assert_eq!(t, 0.0);

        let (distance, t) = point_to_segment(Vec2::new(14.0, 0.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
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
                project
            )
            .is_none(),
            "edges further than the tolerance are not picked"
        );
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
            app.world().resource::<EdgePressCapture>().consumed_pointer(),
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

        assert!(!app.world().resource::<EdgePressCapture>().consumed_pointer());
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
