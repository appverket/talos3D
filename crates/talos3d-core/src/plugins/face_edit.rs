use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::plugins::modeling::editable_mesh::EditableMesh;
use crate::plugins::modeling::primitive_trait::Primitive;
use crate::{
    authored_entity::{PushPullAffordance, PushPullBlockReason},
    capability_registry::{
        CapabilityRegistry, FaceHitCandidate, FaceId, GeneratedEdgeRef, GeneratedFaceRef,
        SelectableSubobjectRef, SubobjectSelection,
    },
    plugins::{
        camera::OrbitCamera,
        cursor::{
            cursor_window_position as mapped_cursor_window_position, CursorWorldPos, DrawingPlane,
        },
        drafting::{
            authoring::resting_draft_plane, draft::DraftNode, workspace::DraftingWorkspaceState,
        },
        edge_selection::{
            selected_edges, EdgePickSystems, EdgePressCapture, EdgeSelectionPlugin, HoveredEdge,
        },
        egui_chrome::{ChromeInputCapture, EguiWantsInput},
        identity::ElementId,
        input_ownership::{InputOwnership, InputPhase},
        modeling::{
            csg::{csg_face_hit_test, CsgNode, CsgParentMap},
            primitives::{
                BoxPrimitive, CylinderPrimitive, PlanePrimitive, ShapeRotation, SpherePrimitive,
            },
            profile::{Profile2d, ProfileExtrusion, ProfileRevolve, ProfileSegment, ProfileSweep},
            profile_feature::{
                face_profile_feature_hit_test, feature_face_vertices, FaceProfileFeature,
            },
            semantics::semantic_push_pull_affordance,
        },
        scene_ray,
        selection::Selected,
        subobject_overlay::SubobjectOverlayPlugin,
        tools::ActiveTool,
        transform::{
            start_transform_mode_with_options, AxisConstraint, TransformMode, TransformShortcuts,
            TransformStartOptions, TransformState, TransformVisualSystems,
        },
        ui::StatusBarData,
    },
};
const FACE_HIGHLIGHT_EDGE: Color = Color::srgba(0.3, 0.7, 1.0, 0.8);
const FACE_SELECTED_EDGE: Color = Color::srgb(0.2, 1.0, 0.4);
const FACE_NORMAL_LENGTH: f32 = 0.5;
const FACE_NORMAL_COLOR: Color = Color::srgb(0.3, 0.9, 1.0);
const PUSHPULL_EDGE_COLOR: Color = Color::srgb(1.0, 0.7, 0.0);
const DRAWING_PREVIEW_COLOR: Color = Color::srgb(0.45, 0.9, 1.0);
const DRAWING_CLOSE_COLOR: Color = Color::srgb(0.2, 1.0, 0.4);
const MIN_DRAWING_SEGMENT: f32 = 0.05;
const CLOSE_THRESHOLD_PIXELS: f32 = 15.0;
const FACE_CLICK_SLOP_PX: f32 = 6.0;
const FACE_MULTI_CLICK_THRESHOLD_SECONDS: f64 = 0.4;

pub struct FaceEditPlugin;

impl Plugin for FaceEditPlugin {
    fn build(&self, app: &mut App) {
        crate::plugins::modeling::face_sketch::register(app);
        app.add_systems(
            Update,
            crate::plugins::modeling::face_sketch::select_pending_region
                .after(crate::plugins::history::HistorySet::Apply),
        );
        // Subobject picking and subobject highlighting are the two halves of
        // face editing that the mode cannot be used without, so they compose
        // here rather than needing separate registration in every app binary.
        app.add_plugins((EdgeSelectionPlugin, SubobjectOverlayPlugin))
            .init_resource::<FaceEditContext>()
            .init_resource::<HoveredFace>()
            .init_resource::<PressCapture>()
            .init_resource::<FaceClickTracker>()
            .init_resource::<PushPullContext>()
            .init_resource::<FaceDrawingContext>()
            .init_resource::<SubobjectSelection>()
            .add_systems(
                Update,
                (
                    update_hovered_face
                        .in_set(InputPhase::ToolInput)
                        .run_if(face_edit_active)
                        .run_if(not_drawing),
                    handle_face_click
                        .after(update_hovered_face)
                        .before(crate::plugins::selection::SelectionInputSystems::Click)
                        .in_set(InputPhase::ToolInput)
                        // The edge picker resolves first and flags the pointer
                        // event it owns, so a click that landed on an edge does
                        // not also move the face selection.
                        .after(EdgePickSystems)
                        .run_if(face_edit_active)
                        .run_if(not_drawing),
                    handle_face_escape
                        .after(crate::plugins::selection::SelectionInputSystems::Escape)
                        .in_set(InputPhase::ToolInput)
                        .run_if(face_edit_active)
                        .run_if(not_drawing),
                    handle_push_pull_shortcut
                        .in_set(InputPhase::ToolInput)
                        .before(TransformShortcuts)
                        .run_if(not_drawing),
                    // Face drawing sub-mode: L activates, clicks add points, close creates entity
                    handle_face_drawing_start
                        .in_set(InputPhase::ToolInput)
                        .run_if(face_edit_active),
                    handle_face_drawing_input
                        .in_set(InputPhase::ToolInput)
                        .run_if(is_drawing),
                    draw_face_highlights
                        .after(TransformVisualSystems::PreviewDraw)
                        .run_if(face_edit_active),
                    draw_face_drawing_preview.run_if(is_drawing),
                    update_face_edit_status.run_if(face_edit_active),
                    sync_drawing_plane_to_face,
                )
                    .run_if(in_state(ActiveTool::Select)),
            );
    }
}

/// Tracks the active push/pull operation so that `compute_preview` can use
/// `push_pull()` instead of `translate_by()`.
#[derive(Resource, Default, Debug, Clone)]
pub struct PushPullContext {
    /// When set, the transform is a push/pull operation on this face.
    pub active_face: Option<PushPullFace>,
}

#[derive(Debug, Clone)]
pub struct PushPullFace {
    pub entity: Entity,
    pub element_id: ElementId,
    pub face_id: FaceId,
    pub generated_face_ref: Option<GeneratedFaceRef>,
    pub normal: Vec3,
    /// For face-drawn profile features, the owning solid that receives the union/difference.
    pub feature_parent: Option<ElementId>,
    /// Stable origin on the parent face from which signed push/pull distance is measured.
    pub feature_anchor_origin: Option<Vec3>,
    /// Frozen drag plane — computed once at session start, remains fixed.
    /// Contains the face normal axis and faces the camera.
    pub drag_plane: DrawingPlane,
    /// Cursor position in viewport coordinates at push/pull start.
    pub initial_cursor_screen: Option<Vec2>,
    /// Projection of the face normal into viewport coordinates.
    pub screen_normal: Option<Vec2>,
    /// Live CSG node for real-time boolean preview during drag.
    /// Created lazily once a feature push/pull actually moves away from zero.
    pub live_csg: Option<ElementId>,
}

pub fn face_edit_active(context: Res<FaceEditContext>) -> bool {
    context.is_active()
}

/// Tracks which entity is being face-edited and which face is selected.
#[derive(Resource, Default, Debug, Clone)]
pub struct FaceEditContext {
    /// The entity whose faces are being edited.
    pub entity: Option<Entity>,
    /// The ElementId of the entity being face-edited.
    pub element_id: Option<ElementId>,
    /// The currently selected face, if any.
    pub selected_face: Option<SelectedFace>,
    /// When face-editing a CsgNode, this tracks which operand entity owns the
    /// currently selected/hovered face so push/pull targets the operand directly.
    pub csg_operand_target: Option<(Entity, ElementId)>,
}

#[derive(Debug, Clone)]
pub struct SelectedFace {
    pub face_id: FaceId,
    pub generated_face_ref: Option<GeneratedFaceRef>,
    pub normal: Vec3,
    pub centroid: Vec3,
}

impl FaceEditContext {
    pub fn is_active(&self) -> bool {
        self.entity.is_some()
    }

    pub fn enter(&mut self, entity: Entity, element_id: ElementId) {
        self.entity = Some(entity);
        self.element_id = Some(element_id);
        self.selected_face = None;
        self.csg_operand_target = None;
    }

    pub fn exit(&mut self) {
        *self = Self::default();
    }
}

fn selected_face_push_pull_affordance(
    world: &World,
    face_context: &FaceEditContext,
    selected_face: &SelectedFace,
) -> PushPullAffordance {
    let target_entity = face_context
        .csg_operand_target
        .map(|(entity, _)| entity)
        .or(face_context.entity);
    let Some(target_entity) = target_entity else {
        return PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace);
    };
    let registry = world.resource::<CapabilityRegistry>();
    let Ok(entity_ref) = world.get_entity(target_entity) else {
        return PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace);
    };
    let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
        return PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace);
    };
    let authored = snapshot.push_pull_affordance(selected_face.face_id);
    if !authored.is_allowed() {
        return authored;
    }
    semantic_push_pull_affordance(
        world,
        snapshot.element_id(),
        selected_face.generated_face_ref.as_ref(),
    )
    .unwrap_or(authored)
}

/// Drawing sub-mode within face editing.
///
/// When active, the user is drawing a polyline on the selected face.
/// Points are stored in the DrawingPlane's 2D coordinates.
/// Closing the loop creates a ProfileExtrusion and optionally starts push/pull.
#[derive(Resource, Default, Debug, Clone)]
pub struct FaceDrawingContext {
    /// Whether drawing mode is active.
    pub active: bool,
    /// Points in 3D world space (on the drawing plane).
    pub points: Vec<Vec3>,
    /// Whether each segment leading TO this point was an arc.
    /// Length matches `points` — index 0 is unused (start point has no incoming segment).
    pub segment_is_arc: Vec<bool>,
    /// Whether to use arc mode for the next segment.
    pub arc_mode: bool,
}

/// Tracks which face the cursor is currently hovering over.
#[derive(Resource, Default, Debug, Clone)]
pub struct HoveredFace {
    pub hit: Option<FaceHitCandidate>,
}

/// Captured face target from mouse press — used for click resolution.
/// This is the key to reliable small-face selection: the target is locked
/// on press and used on release, rather than re-querying hover at release time.
#[derive(Resource, Default, Debug, Clone)]
struct PressCapture {
    ignore_until_release: bool,
    face_hit: Option<FaceHitCandidate>,
    cursor_screen: Option<Vec2>,
    shift_held: bool,
}

/// A drawing completion owns both halves of its final pointer gesture.
pub(crate) fn consume_drawing_completion_click(world: &mut World) {
    let pressed = world
        .get_resource::<ButtonInput<MouseButton>>()
        .is_some_and(|buttons| buttons.pressed(MouseButton::Left));
    if let Some(mut capture) = world.get_resource_mut::<PressCapture>() {
        *capture = PressCapture {
            ignore_until_release: pressed,
            ..Default::default()
        };
    }
}

#[derive(Resource, Default, Debug, Clone)]
struct FaceClickTracker {
    last_click_time: f64,
    last_element_id: Option<ElementId>,
    last_face_id: Option<FaceId>,
    click_count: u8,
}

fn update_hovered_face(world: &mut World) {
    if world
        .resource::<ChromeInputCapture>()
        .wants_any_pointer_input()
        || world.resource::<EguiWantsInput>().wants_any_pointer_input()
    {
        world.resource_mut::<HoveredFace>().hit = None;
        return;
    }

    let Some(entity) = world.resource::<FaceEditContext>().entity else {
        world.resource_mut::<HoveredFace>().hit = None;
        return;
    };

    let Some(ray) = scene_ray::build_camera_ray(world) else {
        world.resource_mut::<HoveredFace>().hit = None;
        return;
    };

    if world.get::<FaceProfileFeature>(entity).is_some() {
        world.resource_mut::<HoveredFace>().hit = face_profile_feature_hit_test(world, entity, ray);
        return;
    }

    // If the face-edited entity is a CsgNode, delegate to csg_face_hit_test.
    let is_csg = world.get::<CsgNode>(entity).is_some();
    if is_csg {
        let hit = csg_face_hit_test(world, entity, ray);
        // Track which operand the hovered face belongs to.
        if let Some(ref h) = hit {
            world.resource_mut::<FaceEditContext>().csg_operand_target =
                Some((h.entity, h.element_id));
        } else {
            world.resource_mut::<FaceEditContext>().csg_operand_target = None;
        }
        world.resource_mut::<HoveredFace>().hit = hit;
        return;
    }

    // Find the factory for this entity and do face hit test
    let hit = {
        let registry = world.resource::<CapabilityRegistry>();
        let Ok(entity_ref) = world.get_entity(entity) else {
            world.resource_mut::<HoveredFace>().hit = None;
            return;
        };
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            world.resource_mut::<HoveredFace>().hit = None;
            return;
        };
        let Some(factory) = registry.factory_for(snapshot.type_name()) else {
            world.resource_mut::<HoveredFace>().hit = None;
            return;
        };
        factory.hit_test_face(world, entity, ray)
    };

    world.resource_mut::<HoveredFace>().hit = hit;
}

fn handle_face_click(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    hovered: Res<HoveredFace>,
    window_query: Query<&Window, With<PrimaryWindow>>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time<Real>>,
    mut press_capture: ResMut<PressCapture>,
    mut face_context: ResMut<FaceEditContext>,
    mut click_tracker: ResMut<FaceClickTracker>,
    mut subobject_selection: ResMut<SubobjectSelection>,
    ownership: Res<InputOwnership>,
    edge_press: Res<EdgePressCapture>,
) {
    if press_capture.ignore_until_release {
        if mouse_buttons.just_released(MouseButton::Left) {
            press_capture.ignore_until_release = false;
        }
        return;
    }
    if !ownership.is_idle() {
        return;
    }

    // An edge under the cursor takes the click. Faces are the fallback target,
    // exactly as in SketchUp, where edges win over the surface behind them.
    if edge_press.consumed_pointer() {
        press_capture.face_hit = None;
        press_capture.cursor_screen = None;
        press_capture.shift_held = false;
        return;
    }

    // Capture the hit target on mouse press — this is what the click will resolve to.
    if mouse_buttons.just_pressed(MouseButton::Left) {
        press_capture.face_hit = hovered.hit.clone();
        press_capture.cursor_screen = window_query.single().ok().and_then(Window::cursor_position);
        press_capture.shift_held =
            keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    }

    // Resolve the click on mouse release using the CAPTURED target, not live hover.
    // This makes small-face selection reliable: a few pixels of drift between
    // press and release don't lose the target.
    if mouse_buttons.just_released(MouseButton::Left) {
        // Drawing commits on press; its release must not clear the new region.
        if press_capture.cursor_screen.is_none() {
            return;
        }
        let release_cursor = window_query.single().ok().and_then(Window::cursor_position);
        let moved_too_far = match (press_capture.cursor_screen, release_cursor) {
            (Some(press), Some(release)) => press.distance(release) > FACE_CLICK_SLOP_PX,
            _ => false,
        };

        let shift_held = press_capture.shift_held;
        press_capture.cursor_screen = None;
        press_capture.shift_held = false;

        let captured_hit = press_capture.face_hit.take();
        if moved_too_far {
            return;
        }

        if let Some(hit) = captured_hit {
            let already_selected = face_context
                .selected_face
                .as_ref()
                .is_some_and(|selected| selected.face_id == hit.face_id);
            if shift_held && already_selected {
                face_context.selected_face = None;
                face_context.csg_operand_target = None;
                subobject_selection.refs.clear();
            } else {
                // For CSG operand hits, the hit entity/element_id is the operand.
                // The csg_operand_target is kept as set by update_hovered_face.
                let click_count =
                    face_click_count(&mut click_tracker, &hit, time.elapsed_secs_f64());
                face_context.selected_face = Some(SelectedFace {
                    face_id: hit.face_id,
                    generated_face_ref: hit.generated_face_ref.clone(),
                    normal: hit.normal,
                    centroid: hit.centroid,
                });
                subobject_selection.refs = face_selection_refs(hit.element_id, &hit, click_count);
            }
        } else if !shift_held {
            face_context.selected_face = None;
            face_context.csg_operand_target = None;
            subobject_selection.refs.clear();
        }
    }
}

fn face_click_count(tracker: &mut FaceClickTracker, hit: &FaceHitCandidate, now: f64) -> u8 {
    let same_target = tracker.last_element_id == Some(hit.element_id)
        && tracker.last_face_id == Some(hit.face_id)
        && (now - tracker.last_click_time) <= FACE_MULTI_CLICK_THRESHOLD_SECONDS;
    tracker.click_count = if same_target {
        (tracker.click_count + 1).min(3)
    } else {
        1
    };
    tracker.last_click_time = now;
    tracker.last_element_id = Some(hit.element_id);
    tracker.last_face_id = Some(hit.face_id);
    tracker.click_count
}

pub(crate) fn face_selection_refs(
    element_id: ElementId,
    hit: &FaceHitCandidate,
    click_count: u8,
) -> Vec<SelectableSubobjectRef> {
    let element_id = element_id.0;
    let Some(face_ref) = hit.generated_face_ref.clone() else {
        return vec![SelectableSubobjectRef::Entity { element_id }];
    };
    let face = SelectableSubobjectRef::Face {
        element_id,
        face: face_ref.clone(),
    };
    match click_count {
        1 => vec![face],
        2 => {
            let mut refs = vec![face.clone()];
            refs.extend(generated_face_bounding_edge_refs(element_id, &face_ref));
            refs
        }
        _ => {
            let mut refs = vec![face];
            refs.extend(generated_face_bounding_edge_refs(element_id, &face_ref));
            refs
        }
    }
}

fn generated_face_bounding_edge_refs(
    element_id: u64,
    face_ref: &GeneratedFaceRef,
) -> Vec<SelectableSubobjectRef> {
    // Viewport double-click expansion runs before the heavier evaluated-body
    // API path. It records stable generated refs for the common primitive face
    // families and lets the Model API provide full evaluated topology details.
    let count = match face_ref {
        GeneratedFaceRef::CapabilitySurface { .. } => 0,
        GeneratedFaceRef::BoxFace { .. } => 4,
        GeneratedFaceRef::PlaneFace => 4,
        GeneratedFaceRef::CylinderTop | GeneratedFaceRef::CylinderBottom => 16,
        GeneratedFaceRef::CylinderSide => 16,
        GeneratedFaceRef::ProfileTop | GeneratedFaceRef::ProfileBottom => 4,
        GeneratedFaceRef::ProfileSideSegment(_)
        | GeneratedFaceRef::ProfileSideArcSegment(_)
        | GeneratedFaceRef::ProfileSideClosingSegment
        | GeneratedFaceRef::FeatureCap
        | GeneratedFaceRef::FeatureAnchor
        | GeneratedFaceRef::FeatureSideSegment(_)
        | GeneratedFaceRef::FeatureSideArcSegment(_)
        | GeneratedFaceRef::FeatureSideClosingSegment => 1,
    };
    (0..count)
        .map(|edge_index| SelectableSubobjectRef::Edge {
            element_id,
            edge: GeneratedEdgeRef::BoundaryOfFace {
                face: face_ref.clone(),
                edge_index,
            },
        })
        .collect()
}

/// Clear presentation-only state when the edited solid's context is left.
pub(crate) fn clear_face_edit_transients(world: &mut World) {
    if let Some(mut drawing) = world.get_resource_mut::<FaceDrawingContext>() {
        *drawing = FaceDrawingContext::default();
    }
    if let Some(mut hover) = world.get_resource_mut::<HoveredFace>() {
        *hover = HoveredFace::default();
    }
    if let Some(mut press) = world.get_resource_mut::<PressCapture>() {
        *press = PressCapture::default();
    }
    if let Some(mut tracker) = world.get_resource_mut::<FaceClickTracker>() {
        *tracker = FaceClickTracker::default();
    }
}

fn handle_face_escape(world: &mut World) {
    if !world.resource::<InputOwnership>().is_idle()
        || !world
            .resource::<ButtonInput<KeyCode>>()
            .just_pressed(KeyCode::Escape)
    {
        return;
    }
    if world.resource::<FaceEditContext>().selected_face.is_some() {
        world.resource_mut::<FaceEditContext>().selected_face = None;
        world.resource_mut::<SubobjectSelection>().refs.clear();
    } else {
        crate::plugins::selection::exit_edit_context(world);
    }
}

fn draw_face_highlights(world: &World, mut gizmos: Gizmos) {
    let face_context = world.resource::<FaceEditContext>();
    let hovered = world.resource::<HoveredFace>();
    let push_pull = world.resource::<PushPullContext>();
    let transform_state = world.resource::<TransformState>();
    let is_drawing = world.resource::<FaceDrawingContext>().active;

    let entity = match face_context.entity {
        Some(e) => e,
        None => return,
    };

    // When face-editing a CsgNode, face verts come from operand entities.
    // The hit candidates returned by csg_face_hit_test already carry the operand entity,
    // so we resolve face_entity from the hit itself rather than using the csg root.
    let csg_operand_target = face_context.csg_operand_target;

    let is_transforming = !transform_state.is_idle();
    let is_push_pull = push_pull.active_face.is_some();

    // Helper: get face entity for rendering — operand entity when editing a CsgNode.
    let face_entity_for = |hit_entity: Entity| -> Entity {
        // If the hit is on an operand, use that directly; otherwise fall back to the root.
        if csg_operand_target.is_some() {
            hit_entity
        } else {
            entity
        }
    };

    // During push/pull: highlight the face being pushed/pulled in orange
    if is_push_pull {
        if let Some(pp) = &push_pull.active_face {
            // During push/pull on a CSG operand, read verts from the operand entity.
            let render_entity = if csg_operand_target.is_some() {
                pp.entity
            } else {
                entity
            };
            if let Some(verts) = face_vertices_for_entity(world, render_entity, pp.face_id) {
                draw_face_outline_for_entity(
                    world,
                    &mut gizmos,
                    render_entity,
                    &verts,
                    PUSHPULL_EDGE_COLOR,
                );
            }
        }
        return;
    }

    // Draw hovered face (blue outline + fill) — only when not transforming or drawing
    if !is_transforming && !is_drawing {
        if let Some(hit) = &hovered.hit {
            let is_selected = face_context
                .selected_face
                .as_ref()
                .is_some_and(|s| s.face_id == hit.face_id);
            if !is_selected {
                let render_entity = face_entity_for(hit.entity);
                if let Some(verts) = face_vertices_for_entity(world, render_entity, hit.face_id) {
                    draw_face_outline_for_entity(
                        world,
                        &mut gizmos,
                        render_entity,
                        &verts,
                        FACE_HIGHLIGHT_EDGE,
                    );
                }
            }
        }
    }

    // Always draw selected face outline (green) — even during drawing mode.
    // This shows which face the user is drawing on.
    if let Some(selected) = &face_context.selected_face {
        // For CSG face edit, read verts from the tracked operand entity.
        let render_entity = csg_operand_target
            .map(|(op_entity, _)| op_entity)
            .unwrap_or(entity);
        if let Some(verts) = face_vertices_for_entity(world, render_entity, selected.face_id) {
            draw_face_outline_for_entity(
                world,
                &mut gizmos,
                render_entity,
                &verts,
                FACE_SELECTED_EDGE,
            );
            if !is_drawing {
                // The face fill itself is the GPU stipple overlay in
                // `subobject_overlay`; gizmos only carry the outline and the
                // push/pull normal.
                let tip = selected.centroid + selected.normal * FACE_NORMAL_LENGTH;
                let (tangent, _) = normal_basis(selected.normal);
                let arrow_size = FACE_NORMAL_LENGTH * 0.2;
                gizmos.line(selected.centroid, tip, FACE_NORMAL_COLOR);
                gizmos.line(
                    tip,
                    tip - selected.normal * arrow_size + tangent * arrow_size,
                    FACE_NORMAL_COLOR,
                );
                gizmos.line(
                    tip,
                    tip - selected.normal * arrow_size - tangent * arrow_size,
                    FACE_NORMAL_COLOR,
                );
            }
        }
    }
}

/// Draw the outline of a face polygon.
fn draw_face_outline(gizmos: &mut Gizmos, verts: &[Vec3], color: Color) {
    let n = verts.len();
    for i in 0..n {
        gizmos.line(verts[i], verts[(i + 1) % n], color);
    }
}

// Bevy's batched, depth-tested line pass handles visibility. Do not cast a
// ray per outline edge every frame (a circular sketch has many sampled edges).
fn draw_face_outline_for_entity(
    _world: &World,
    gizmos: &mut Gizmos,
    _entity: Entity,
    verts: &[Vec3],
    color: Color,
) {
    draw_face_outline(gizmos, verts, color);
}

/// Get the world-space vertex positions of a face for any entity type.
pub(crate) fn face_vertices_for_entity(
    world: &World,
    entity: Entity,
    face_id: FaceId,
) -> Option<Vec<Vec3>> {
    let entity_ref = world.get_entity(entity).ok()?;

    if entity_ref.get::<FaceProfileFeature>().is_some() {
        return feature_face_vertices(world, entity, face_id);
    }

    if let Some(body) = entity_ref.get::<crate::plugins::modeling::planar_body::PlanarBody>() {
        return body.boundary(face_id);
    }

    // EditableMesh path
    if let Some(mesh) = entity_ref.get::<EditableMesh>() {
        let fi = face_id.0;
        if (fi as usize) >= mesh.faces.len() || mesh.faces[fi as usize].half_edge == u32::MAX {
            return None;
        }
        let vis = mesh.vertices_of_face(fi);
        let positions: Vec<Vec3> = vis.iter().map(|&vi| mesh.vertices[vi as usize]).collect();
        return if positions.len() >= 3 {
            Some(positions)
        } else {
            None
        };
    }

    // Parametric primitive path — try all known types
    let rotation = entity_ref
        .get::<ShapeRotation>()
        .copied()
        .unwrap_or_default()
        .0;
    let mesh = try_editable_mesh::<BoxPrimitive>(&entity_ref, rotation)
        .or_else(|| try_editable_mesh::<CylinderPrimitive>(&entity_ref, rotation))
        .or_else(|| try_editable_mesh::<SpherePrimitive>(&entity_ref, rotation))
        .or_else(|| try_editable_mesh::<PlanePrimitive>(&entity_ref, rotation))
        .or_else(|| try_editable_mesh::<ProfileExtrusion>(&entity_ref, rotation))
        .or_else(|| try_editable_mesh::<ProfileSweep>(&entity_ref, rotation))
        .or_else(|| try_editable_mesh::<ProfileRevolve>(&entity_ref, rotation))?;
    let fi = face_id.0;
    if (fi as usize) >= mesh.faces.len() || mesh.faces[fi as usize].half_edge == u32::MAX {
        return None;
    }
    let vis = mesh.vertices_of_face(fi);
    let positions: Vec<Vec3> = vis.iter().map(|&vi| mesh.vertices[vi as usize]).collect();
    if positions.len() >= 3 {
        Some(positions)
    } else {
        None
    }
}

fn try_editable_mesh<P: Primitive>(
    entity_ref: &bevy::ecs::world::EntityRef,
    rotation: Quat,
) -> Option<EditableMesh> {
    entity_ref.get::<P>()?.to_editable_mesh(rotation)
}

fn normal_basis(normal: Vec3) -> (Vec3, Vec3) {
    let up = if normal.y.abs() > 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let tangent = normal.cross(up).normalize();
    let bitangent = tangent.cross(normal).normalize();
    (tangent, bitangent)
}

fn build_push_pull_drag_plane(
    camera_transform: Option<Transform>,
    centroid: Vec3,
    face_normal: Vec3,
) -> DrawingPlane {
    let face_normal = face_normal.normalize_or_zero();
    let fallback_axis = if face_normal.y.abs() > 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };

    let axis_in_plane = camera_transform
        .map(|camera_transform| {
            let camera_up = camera_transform.rotation * Vec3::Y;
            let camera_right = camera_transform.rotation * Vec3::X;

            let reject_from_normal =
                |v: Vec3| (v - face_normal * v.dot(face_normal)).normalize_or_zero();

            let preferred = reject_from_normal(camera_up);
            if preferred.length_squared() > 1e-4 {
                preferred
            } else {
                let fallback = reject_from_normal(camera_right);
                if fallback.length_squared() > 1e-4 {
                    fallback
                } else {
                    face_normal.cross(fallback_axis).normalize()
                }
            }
        })
        .unwrap_or_else(|| face_normal.cross(fallback_axis).normalize());

    let plane_normal = axis_in_plane.cross(face_normal).normalize_or_zero();
    let plane_normal = if plane_normal.length_squared() > 1e-4 {
        plane_normal
    } else {
        face_normal.cross(fallback_axis).normalize()
    };

    let tangent = axis_in_plane;
    let bitangent = plane_normal.cross(tangent).normalize_or_zero();

    DrawingPlane {
        origin: centroid,
        normal: plane_normal,
        tangent,
        bitangent,
    }
}

fn active_camera(world: &World) -> Option<(Camera, GlobalTransform)> {
    let mut camera_query =
        world.try_query_filtered::<(&Camera, &GlobalTransform), With<OrbitCamera>>()?;
    camera_query
        .iter(world)
        .find(|(camera, _)| camera.is_active)
        .or_else(|| camera_query.iter(world).next())
        .map(|(camera, transform)| (camera.clone(), *transform))
}

/// Cursor position in window/render-target logical pixels — the space
/// `viewport_to_world` and `world_to_viewport` both use, so no camera-relative
/// offset is applied here.
fn viewport_cursor_position(world: &World) -> Option<Vec2> {
    let mut window_query = world.try_query_filtered::<&Window, With<PrimaryWindow>>()?;
    let window = window_query.single(world).ok()?;
    mapped_cursor_window_position(window)
}

fn projected_face_normal_screen(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    centroid: Vec3,
    face_normal: Vec3,
) -> Option<Vec2> {
    let start = camera.world_to_viewport(camera_transform, centroid).ok()?;
    let end = camera
        .world_to_viewport(
            camera_transform,
            centroid + face_normal.normalize_or_zero() * 0.25,
        )
        .ok()?;
    let delta = end - start;
    (delta.length_squared() > 1e-6).then_some(delta.normalize())
}

fn face_drawn_feature_anchor(
    world: &World,
    entity: Entity,
    element_id: ElementId,
    selected_face: &SelectedFace,
) -> Option<(ElementId, Vec3)> {
    let parent_id = world
        .resource::<CsgParentMap>()
        .parents
        .get(&element_id)
        .copied()?;
    let extrusion = world.get::<ProfileExtrusion>(entity)?;

    let anchor_origin = match selected_face.generated_face_ref.as_ref() {
        Some(GeneratedFaceRef::ProfileBottom) => selected_face.centroid,
        Some(GeneratedFaceRef::ProfileTop) => {
            selected_face.centroid - selected_face.normal * extrusion.height
        }
        _ => return None,
    };

    Some((parent_id, anchor_origin))
}

fn handle_push_pull_shortcut(world: &mut World) {
    if !world.resource::<InputOwnership>().is_idle() {
        return;
    }

    let face_context = world.resource::<FaceEditContext>().clone();
    if !face_context.is_active() {
        return;
    }
    let Some(selected_face) = &face_context.selected_face else {
        return;
    };
    let Some(entity) = face_context.entity else {
        return;
    };
    let affordance = selected_face_push_pull_affordance(world, &face_context, selected_face);
    if !affordance.is_allowed() {
        world.resource_mut::<StatusBarData>().hint = affordance.blocked_feedback().to_string();
        return;
    }

    let keys = world.resource::<ButtonInput<KeyCode>>();
    let key_pressed = keys.just_pressed(KeyCode::KeyG);
    let has_modifier = keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::AltLeft)
        || keys.pressed(KeyCode::AltRight);
    if !key_pressed || has_modifier {
        return;
    }

    // If in CSG face-edit mode, push/pull should target the operand entity.
    let csg_operand_target = face_context.csg_operand_target;
    let (pp_entity, pp_element_id) = match csg_operand_target {
        Some((op_entity, op_id)) => (op_entity, op_id),
        None => (entity, face_context.element_id.unwrap_or(ElementId(0))),
    };

    // Temporarily select the push/pull entity so collect_selected_snapshots finds it
    if let Ok(mut entity_mut) = world.get_entity_mut(pp_entity) {
        entity_mut.insert(Selected);
    }

    // The selected-face record can survive earlier push/pull edits. Re-resolve
    // its current plane once at gesture start instead of retaining the old cap.
    let face_centroid = face_vertices_for_entity(world, pp_entity, selected_face.face_id)
        .filter(|vertices| !vertices.is_empty())
        .map(|vertices| vertices.iter().copied().sum::<Vec3>() / vertices.len() as f32)
        .unwrap_or(selected_face.centroid);
    let face_normal = selected_face.normal;
    let face_id = selected_face.face_id;
    let generated_face_ref = selected_face.generated_face_ref.clone();

    // Set DrawingPlane to a plane that CONTAINS the face normal direction.
    // The face plane itself is perpendicular to the normal (cursor can't move along it).
    // The ground plane may not intersect the camera ray for vertical faces.
    // Solution: use a plane through the face centroid that faces the camera and
    // contains the push/pull axis (face normal).
    // Compute a frozen drag plane that contains the face normal and faces the camera.
    // This plane remains fixed for the entire push/pull session.
    let active_camera = active_camera(world);
    let camera_transform = active_camera
        .as_ref()
        .map(|(_, transform)| transform.compute_transform());
    let drag_plane = build_push_pull_drag_plane(camera_transform, face_centroid, face_normal);
    let initial_cursor_screen = active_camera
        .as_ref()
        .and_then(|_| viewport_cursor_position(world));
    let screen_normal = active_camera
        .as_ref()
        .and_then(|(camera, camera_transform)| {
            projected_face_normal_screen(camera, camera_transform, face_centroid, face_normal)
        });

    // Install the drag plane as the active DrawingPlane
    *world.resource_mut::<DrawingPlane>() = drag_plane.clone();

    let (feature_parent, feature_anchor_origin) = if csg_operand_target.is_none() {
        face_drawn_feature_anchor(world, pp_entity, pp_element_id, selected_face)
            .map(|(parent_id, anchor_origin)| (Some(parent_id), Some(anchor_origin)))
            .unwrap_or((None, None))
    } else {
        (None, None)
    };

    // Set up push/pull context with the frozen drag plane
    world.resource_mut::<PushPullContext>().active_face = Some(PushPullFace {
        entity: pp_entity,
        element_id: pp_element_id,
        face_id,
        generated_face_ref,
        normal: face_normal,
        feature_parent,
        feature_anchor_origin,
        drag_plane,
        initial_cursor_screen,
        screen_normal,
        live_csg: None,
    });

    let result = start_transform_mode_with_options(
        world,
        TransformMode::Moving,
        TransformStartOptions {
            axis: AxisConstraint::Custom(face_normal),
            confirm_on_release: true,
            ..default()
        },
    );

    if result.is_err() {
        // Revert if we couldn't start the transform
        world.resource_mut::<PushPullContext>().active_face = None;
        if let Ok(mut entity_mut) = world.get_entity_mut(pp_entity) {
            entity_mut.remove::<Selected>();
        }
    } else {
        // Immediately set InputOwnership so other ToolInput systems in this
        // same frame see Modal and don't process stale mouse events.
        *world.resource_mut::<InputOwnership>() =
            InputOwnership::Modal(crate::plugins::input_ownership::ModalKind::PushPull);
    }
}

/// Sync the DrawingPlane resource to the selected face.
///
/// When a face is selected, tools project onto it. When deselected, revert to
/// the plane the client is otherwise drawing on: the active Draft's plane
/// inside Drafting, and the ground plane outside it. Reverting unconditionally
/// to ground would silently overwrite the Draft plane one frame after the
/// Drafting controller installed it, leaving the cursor and every tool
/// projecting against the ground while an elevation or section Draft is active.
///
/// During push/pull, the frozen drag plane takes priority — don't overwrite it.
pub(crate) fn sync_drawing_plane_to_face(
    face_context: Res<FaceEditContext>,
    push_pull: Res<PushPullContext>,
    mut drawing_plane: ResMut<DrawingPlane>,
    drafting_workspace: Option<Res<DraftingWorkspaceState>>,
    drafts: Query<(&ElementId, &DraftNode)>,
) {
    // During push/pull, the drag plane is frozen — don't interfere.
    if let Some(pp) = &push_pull.active_face {
        // Re-apply the frozen drag plane each frame in case something else changed it.
        if (drawing_plane.origin - pp.drag_plane.origin).length_squared() > 1e-6
            || (drawing_plane.normal - pp.drag_plane.normal).length_squared() > 1e-6
        {
            *drawing_plane = pp.drag_plane.clone();
        }
        return;
    }

    if let Some(selected) = &face_context.selected_face {
        let new_plane = DrawingPlane::from_face(selected.centroid, selected.normal);
        if (drawing_plane.origin - new_plane.origin).length_squared() > 1e-6
            || (drawing_plane.normal - new_plane.normal).length_squared() > 1e-6
        {
            *drawing_plane = new_plane;
        }
    } else {
        let resting = resting_draft_plane(drafting_workspace.as_deref(), |draft_id| {
            drafts
                .iter()
                .find(|(id, _)| **id == draft_id)
                .map(|(_, draft)| draft.plane.clone())
        })
        .unwrap_or_else(DrawingPlane::ground);
        if *drawing_plane != resting {
            *drawing_plane = resting;
        }
    }
}

fn update_face_edit_status(world: &mut World) {
    let face_context = world.resource::<FaceEditContext>().clone();
    if !face_context.is_active() {
        return;
    }
    let hovered = world.resource::<HoveredFace>().clone();
    let push_pull = world.resource::<PushPullContext>().clone();
    let drawing = world.resource::<FaceDrawingContext>().clone();
    let transform_state = world.resource::<TransformState>().clone();

    // During push/pull, the transform status system handles the display
    if push_pull.active_face.is_some() && !transform_state.is_idle() {
        return;
    }

    let selected_edge_count = face_context
        .element_id
        .map(|element_id| {
            selected_edges(world.resource::<SubobjectSelection>(), element_id).count()
        })
        .unwrap_or(0);
    let hovered_edge = world.resource::<HoveredEdge>().hit.clone();

    let mut parts = Vec::new();
    parts.push("Face editing".to_string());
    if drawing.active {
        let n = drawing.points.len();
        let shape = world.resource::<crate::plugins::modeling::face_sketch::FaceSketchShape>();
        let mode = match shape {
            crate::plugins::modeling::face_sketch::FaceSketchShape::Circle => {
                "circle: centre, radius"
            }
            crate::plugins::modeling::face_sketch::FaceSketchShape::Rectangle => {
                "rectangle: opposite corners"
            }
            _ => {
                if drawing.arc_mode {
                    "arc"
                } else {
                    "line"
                }
            }
        };
        if n >= 3 {
            parts.push(format!(
                "Drawing ({mode}) {n} pts · close near start · A: arc · Enter to finish · Esc to cancel"
            ));
        } else {
            parts.push(format!(
                "Drawing ({mode}) {n} pts · click to add · A: arc · Esc to cancel"
            ));
        }
    } else if let Some(hit) = &hovered_edge {
        parts.push(format!(
            "Hover: {} · Click to select · Shift-click to add or remove",
            hit.edge.label()
        ));
    } else if selected_edge_count > 0 {
        let plural = if selected_edge_count == 1 { "" } else { "s" };
        parts.push(format!(
            "{selected_edge_count} edge{plural} selected · Shift-click an edge to add or remove"
        ));
    } else if let Some(selected) = &face_context.selected_face {
        let label = selected
            .generated_face_ref
            .as_ref()
            .map(GeneratedFaceRef::label)
            .unwrap_or_else(|| format!("{}", selected.face_id.0));
        let push_pull_hint =
            selected_face_push_pull_affordance(world, &face_context, selected).status_hint();
        parts.push(format!(
            "Face {label} selected · {push_pull_hint} · L: lines · C: circle · P: rectangle on face"
        ));
    } else if let Some(hit) = &hovered.hit {
        let label = hit
            .generated_face_ref
            .as_ref()
            .map(GeneratedFaceRef::label)
            .unwrap_or_else(|| format!("{}", hit.face_id.0));
        parts.push(format!("Hover: Face {label} · Click to select"));
    } else {
        parts.push("Click a face to select".to_string());
    }

    parts.push("Esc to exit".to_string());
    world.resource_mut::<StatusBarData>().hint = parts.join(" | ");
}

// ---------------------------------------------------------------------------
// Run conditions for drawing sub-mode
// ---------------------------------------------------------------------------

fn is_drawing(drawing: Res<FaceDrawingContext>) -> bool {
    drawing.active
}

fn not_drawing(drawing: Res<FaceDrawingContext>) -> bool {
    !drawing.active
}

// ---------------------------------------------------------------------------
// Face drawing sub-mode — draw a profile on the selected face
// ---------------------------------------------------------------------------

/// L key starts drawing on the selected face.
fn handle_face_drawing_start(
    keys: Res<ButtonInput<KeyCode>>,
    face_context: Res<FaceEditContext>,
    mut drawing: ResMut<FaceDrawingContext>,
    mut shape: ResMut<crate::plugins::modeling::face_sketch::FaceSketchShape>,
    ownership: Res<InputOwnership>,
) {
    if !ownership.is_idle() || drawing.active {
        return;
    }
    if face_context.selected_face.is_none() {
        return;
    }
    if keys.just_pressed(KeyCode::KeyL)
        || keys.just_pressed(KeyCode::KeyC)
        || keys.just_pressed(KeyCode::KeyP)
    {
        use crate::plugins::modeling::face_sketch::FaceSketchShape;
        *shape = if keys.just_pressed(KeyCode::KeyC) {
            FaceSketchShape::Circle
        } else if keys.just_pressed(KeyCode::KeyP) {
            FaceSketchShape::Rectangle
        } else {
            FaceSketchShape::Polyline
        };
        drawing.active = true;
        drawing.points.clear();
        drawing.segment_is_arc.clear();
        drawing.arc_mode = false;
    }
}

/// Handle clicks and key presses while in face drawing mode.
fn handle_face_drawing_input(world: &mut World) {
    let chrome_capture = world.resource::<ChromeInputCapture>();
    let egui = world.resource::<EguiWantsInput>();
    let chrome_keyboard = chrome_capture.wants_any_keyboard_input();
    let chrome_pointer = chrome_capture.wants_any_pointer_input();
    let keys = world.resource::<ButtonInput<KeyCode>>().clone();
    let mouse = world.resource::<ButtonInput<MouseButton>>().clone();

    // A key toggles arc mode
    if !chrome_keyboard && !egui.wants_any_keyboard_input() && keys.just_pressed(KeyCode::KeyA) {
        let mut drawing = world.resource_mut::<FaceDrawingContext>();
        drawing.arc_mode = !drawing.arc_mode;
        return;
    }

    // Escape cancels drawing
    if !chrome_keyboard && !egui.wants_any_keyboard_input() && keys.just_pressed(KeyCode::Escape) {
        let mut drawing = world.resource_mut::<FaceDrawingContext>();
        drawing.active = false;
        drawing.points.clear();
        drawing.segment_is_arc.clear();
        drawing.arc_mode = false;
        return;
    }

    // Enter closes and finishes
    if !chrome_keyboard
        && !egui.wants_any_keyboard_input()
        && (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter))
    {
        let n = world.resource::<FaceDrawingContext>().points.len();
        if n >= 2 {
            finish_face_drawing(world);
        }
        return;
    }

    // Left click adds a point
    if !chrome_pointer && !egui.wants_any_pointer_input() && mouse.just_pressed(MouseButton::Left) {
        let cursor_pos = world.resource::<CursorWorldPos>().snapped;
        let Some(cursor_pos) = cursor_pos else {
            return;
        };

        // A snap elsewhere in space must never release the support-plane constraint.
        let plane = world.resource::<DrawingPlane>().clone();
        let cursor_pos = plane.to_world(plane.project_to_2d(cursor_pos));
        let shape = *world.resource::<crate::plugins::modeling::face_sketch::FaceSketchShape>();
        let points_clone = world.resource::<FaceDrawingContext>().points.clone();
        if shape != crate::plugins::modeling::face_sketch::FaceSketchShape::Polyline {
            if let Some(first) = points_clone.first() {
                let result = crate::plugins::modeling::face_sketch::shape_profile(
                    shape,
                    plane.project_to_2d(*first),
                    plane.project_to_2d(cursor_pos),
                )
                .and_then(|profile| {
                    crate::plugins::modeling::face_sketch::commit_profile(world, profile)
                });
                if let Err(error) = result {
                    world
                        .resource_mut::<StatusBarData>()
                        .set_feedback(error, 3.);
                }
            } else {
                world
                    .resource_mut::<FaceDrawingContext>()
                    .points
                    .push(cursor_pos);
            }
            return;
        }

        // Check for close
        if points_clone.len() >= 3 {
            let start = points_clone[0];
            let threshold = face_drawing_close_threshold(world, start);
            if cursor_pos.distance(start) < threshold {
                finish_face_drawing(world);
                return;
            }
        }

        // Min distance check
        if let Some(last) = points_clone.last() {
            if cursor_pos.distance(*last) < MIN_DRAWING_SEGMENT {
                return;
            }
        }

        let is_arc = world.resource::<FaceDrawingContext>().arc_mode;
        let drawing = world.resource_mut::<FaceDrawingContext>();
        drawing.into_inner().points.push(cursor_pos);
        world
            .resource_mut::<FaceDrawingContext>()
            .segment_is_arc
            .push(is_arc);
    }
}

/// Compute close threshold scaled by camera distance.
fn face_drawing_close_threshold(world: &mut World, target: Vec3) -> f32 {
    let mut q = world.query_filtered::<(&Camera, &GlobalTransform), With<OrbitCamera>>();
    let Some((_cam, cam_tf)) = q.iter(world).next() else {
        return 0.3;
    };
    let dist = cam_tf.translation().distance(target);
    (dist * CLOSE_THRESHOLD_PIXELS / 1000.0).clamp(0.05, 2.0)
}

/// Finish face drawing: create ProfileExtrusion and start push/pull.
fn finish_face_drawing(world: &mut World) {
    let drawing = world.resource::<FaceDrawingContext>().clone();
    let plane = world.resource::<DrawingPlane>().clone();

    if drawing.points.len() < 2 {
        let mut d = world.resource_mut::<FaceDrawingContext>();
        d.active = false;
        d.points.clear();
        d.segment_is_arc.clear();
        return;
    }

    // Convert world points to plane 2D
    let points_2d: Vec<Vec2> = drawing
        .points
        .iter()
        .map(|p| plane.project_to_2d(*p))
        .collect();

    let start = points_2d[0];
    let segments: Vec<ProfileSegment> = points_2d[1..]
        .iter()
        .enumerate()
        .map(|(i, &to)| {
            // segment_is_arc[0] is for the start point (unused), so index i+1
            let is_arc = drawing.segment_is_arc.get(i + 1).copied().unwrap_or(false);
            if is_arc {
                // Default bulge for a 90° arc: tan(90/4) ≈ 0.4142
                ProfileSegment::ArcTo { to, bulge: 0.4142 }
            } else {
                ProfileSegment::LineTo { to }
            }
        })
        .collect();
    let profile = Profile2d { start, segments };

    if let Err(error) = crate::plugins::modeling::face_sketch::commit_profile(world, profile) {
        world
            .resource_mut::<StatusBarData>()
            .set_feedback(error, 3.);
    }
}

/// Draw preview lines for face drawing mode.
fn draw_face_drawing_preview(
    drawing: Res<FaceDrawingContext>,
    cursor_world_pos: Res<CursorWorldPos>,
    shape: Res<crate::plugins::modeling::face_sketch::FaceSketchShape>,
    plane: Res<DrawingPlane>,
    camera_query: Query<(&Camera, &GlobalTransform)>,
    mut gizmos: Gizmos,
) {
    if !drawing.active {
        return;
    }

    // Existing segments
    for seg in drawing.points.windows(2) {
        gizmos.line(seg[0], seg[1], DRAWING_PREVIEW_COLOR);
    }

    let Some(cursor_pos) = cursor_world_pos.snapped else {
        return;
    };

    let cursor_pos = plane.to_world(plane.project_to_2d(cursor_pos));
    if *shape != crate::plugins::modeling::face_sketch::FaceSketchShape::Polyline {
        if let Some(first) = drawing.points.first() {
            if let Ok(profile) = crate::plugins::modeling::face_sketch::shape_profile(
                *shape,
                plane.project_to_2d(*first),
                plane.project_to_2d(cursor_pos),
            ) {
                let points = profile.tessellate(64);
                for i in 0..points.len() {
                    gizmos.line(
                        plane.to_world(points[i]),
                        plane.to_world(points[(i + 1) % points.len()]),
                        DRAWING_PREVIEW_COLOR,
                    );
                }
            }
        }
        return;
    }

    // Line from last point to cursor
    if let Some(&last) = drawing.points.last() {
        gizmos.line(last, cursor_pos, DRAWING_PREVIEW_COLOR.with_alpha(0.5));
    }

    // Close preview
    if drawing.points.len() >= 3 {
        let start = drawing.points[0];
        gizmos.line(cursor_pos, start, DRAWING_PREVIEW_COLOR.with_alpha(0.3));

        // Close indicator
        let threshold = if let Some((_cam, cam_tf)) = camera_query.iter().next() {
            let dist = cam_tf.translation().distance(start);
            (dist * CLOSE_THRESHOLD_PIXELS / 1000.0).clamp(0.05, 2.0)
        } else {
            0.3
        };
        if cursor_pos.distance(start) < threshold {
            gizmos.sphere(
                Isometry3d::from_translation(start),
                threshold * 0.3,
                DRAWING_CLOSE_COLOR,
            );
        }
    }

    // Point dots
    for p in &drawing.points {
        gizmos.sphere(
            Isometry3d::from_translation(*p),
            0.04,
            DRAWING_PREVIEW_COLOR,
        );
    }
}

#[cfg(test)]
mod face_sketch_interaction_tests {
    use super::*;
    use crate::plugins::{
        commands::find_entity_by_element_id_readonly,
        history::{apply_pending_history_commands_for_test, History, PendingCommandQueue},
        identity::ElementIdAllocator,
        modeling::{
            face_sketch::{select_hit, select_pending_region, FaceSketchShape, PendingFaceRegion},
            profile_feature::{FaceProfileFeature, FaceProfileFeatureFactory},
        },
    };

    #[test]
    fn pointer_circle_projects_off_plane_snaps_and_undo_redo_preserves_authored_region() {
        let mut world = World::new();
        world.init_resource::<ChromeInputCapture>();
        world.init_resource::<EguiWantsInput>();
        world.init_resource::<ButtonInput<KeyCode>>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<FaceEditContext>();
        world.init_resource::<DrawingPlane>();
        world.init_resource::<SubobjectSelection>();
        world.init_resource::<FaceDrawingContext>();
        world.init_resource::<PendingFaceRegion>();
        world.init_resource::<PendingCommandQueue>();
        world.init_resource::<History>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<ElementIdAllocator>();
        world.init_resource::<CursorWorldPos>();
        world.init_resource::<StatusBarData>();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(FaceProfileFeatureFactory);
        world.insert_resource(registry);
        let host_id = ElementId(100);
        let host = world.spawn(host_id).id();
        let normal = Vec3::new(1., 0.4, 0.7).normalize();
        let origin = Vec3::new(10., 2., 3.);
        select_hit(
            &mut world,
            FaceHitCandidate {
                entity: host,
                element_id: host_id,
                distance: 0.,
                face_id: FaceId(0),
                generated_face_ref: None,
                normal,
                centroid: origin,
            },
        );
        let plane = world.resource::<DrawingPlane>().clone();
        world.insert_resource(FaceSketchShape::Circle);
        world.resource_mut::<FaceDrawingContext>().active = true;
        for point in [Vec2::new(0.4, 0.3), Vec2::new(0.6, 0.3)] {
            world.resource_mut::<ButtonInput<MouseButton>>().reset_all();
            world
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(MouseButton::Left);
            world.resource_mut::<CursorWorldPos>().snapped =
                Some(plane.to_world(point) + normal * 5.);
            handle_face_drawing_input(&mut world);
        }
        let id = world.resource::<PendingFaceRegion>().0.unwrap();
        apply_pending_history_commands_for_test(&mut world);
        select_pending_region(&mut world);
        let entity = find_entity_by_element_id_readonly(&world, id).unwrap();
        let feature = world.get::<FaceProfileFeature>(entity).unwrap().clone();
        assert_eq!(feature.parent, host_id);
        assert_eq!(feature.depth, 0.);
        assert!((feature.anchor_origin - plane.to_world(Vec2::new(0.4, 0.3))).length() < 1e-5);
        for p in feature.profile.tessellate(64) {
            assert!((p.length() - 0.2).abs() < 1e-5);
        }
        assert_eq!(world.resource::<FaceEditContext>().element_id, Some(id));
        assert!(world.resource::<FaceEditContext>().selected_face.is_some());
        world.resource_mut::<PendingCommandQueue>().queue_undo();
        apply_pending_history_commands_for_test(&mut world);
        assert!(find_entity_by_element_id_readonly(&world, id).is_none());
        assert!(world.get_entity(host).is_ok());
        world.resource_mut::<PendingCommandQueue>().queue_redo();
        apply_pending_history_commands_for_test(&mut world);
        let restored = find_entity_by_element_id_readonly(&world, id).unwrap();
        assert_eq!(world.get::<FaceProfileFeature>(restored), Some(&feature));
    }
}
