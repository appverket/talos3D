use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::plugins::modeling::editable_mesh::EditableMesh;
use crate::plugins::modeling::primitive_trait::Primitive;
use crate::{
    authored_entity::{PushPullAffordance, PushPullBlockReason},
    capability_registry::{CapabilityRegistry, FaceHitCandidate, FaceId, GeneratedFaceRef},
    plugins::{
        commands::CreateEntityCommand,
        cursor::{CursorWorldPos, DrawingPlane},
        egui_chrome::EguiWantsInput,
        identity::{ElementId, ElementIdAllocator},
        input_ownership::{InputOwnership, InputPhase},
        modeling::{
            csg::{csg_face_hit_test, CsgNode, CsgParentMap},
            primitives::{BoxPrimitive, CylinderPrimitive, PlanePrimitive, ShapeRotation},
            profile::{Profile2d, ProfileExtrusion, ProfileRevolve, ProfileSegment, ProfileSweep},
            profile_feature::{
                face_profile_feature_hit_test, feature_face_vertices,
                make_face_profile_feature_snapshot, EvaluatedFeature, FaceProfileFeature,
            },
            semantics::semantic_push_pull_affordance,
            snapshots::ray_triangle_intersection,
        },
        scene_ray,
        selection::Selected,
        tools::ActiveTool,
        transform::{
            start_transform_mode_with_options, AxisConstraint, TransformMode, TransformShortcuts,
            TransformStartOptions, TransformState, TransformVisualSystems,
        },
        ui::StatusBarData,
    },
};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};

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

pub struct FaceEditPlugin;

impl Plugin for FaceEditPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FaceEditContext>()
            .init_resource::<HoveredFace>()
            .init_resource::<PressCapture>()
            .init_resource::<PushPullContext>()
            .init_resource::<FaceDrawingContext>()
            .add_systems(
                Update,
                (
                    update_hovered_face
                        .in_set(InputPhase::ToolInput)
                        .run_if(face_edit_active)
                        .run_if(not_drawing),
                    handle_face_click
                        .in_set(InputPhase::ToolInput)
                        .run_if(face_edit_active)
                        .run_if(not_drawing),
                    handle_face_escape
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

fn face_edit_active(context: Res<FaceEditContext>) -> bool {
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
    face_hit: Option<FaceHitCandidate>,
    cursor_screen: Option<Vec2>,
    shift_held: bool,
}

fn update_hovered_face(world: &mut World) {
    if world.resource::<EguiWantsInput>().pointer {
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
    mut press_capture: ResMut<PressCapture>,
    mut face_context: ResMut<FaceEditContext>,
    ownership: Res<InputOwnership>,
) {
    if !ownership.is_idle() {
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
            } else {
                // For CSG operand hits, the hit entity/element_id is the operand.
                // The csg_operand_target is kept as set by update_hovered_face.
                face_context.selected_face = Some(SelectedFace {
                    face_id: hit.face_id,
                    generated_face_ref: hit.generated_face_ref,
                    normal: hit.normal,
                    centroid: hit.centroid,
                });
            }
        } else if !shift_held {
            face_context.selected_face = None;
            face_context.csg_operand_target = None;
        }
    }
}

fn handle_face_escape(
    keys: Res<ButtonInput<KeyCode>>,
    mut face_context: ResMut<FaceEditContext>,
    mut commands: Commands,
    ownership: Res<InputOwnership>,
) {
    if !ownership.is_idle() {
        return;
    }

    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    if face_context.selected_face.is_some() {
        // First Escape: deselect the face
        face_context.selected_face = None;
    } else {
        // Second Escape: exit face edit mode, re-select the entity
        let entity = face_context.entity;
        face_context.exit();
        if let Some(entity) = entity {
            // The entity may have been despawned (e.g. by a line tool split).
            // Use queue_handled to avoid panicking on stale entity IDs.
            commands.queue(move |world: &mut World| {
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.insert(Selected);
                }
            });
        }
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
            if let Some(verts) = get_face_verts(world, render_entity, pp.face_id) {
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
                if let Some(verts) = get_face_verts(world, render_entity, hit.face_id) {
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
        if let Some(verts) = get_face_verts(world, render_entity, selected.face_id) {
            draw_face_outline_for_entity(
                world,
                &mut gizmos,
                render_entity,
                &verts,
                FACE_SELECTED_EDGE,
            );
            // Keep selected-face feedback clean: the hatch lines read as stray
            // gridlines on small caps and obscure the actual profile edge.
            if !is_drawing {
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

fn active_camera_position(world: &World) -> Option<Vec3> {
    let mut camera_query = world.try_query::<(&Camera, &GlobalTransform)>().unwrap();
    camera_query
        .iter(world)
        .find(|(camera, _)| camera.is_active)
        .or_else(|| camera_query.iter(world).next())
        .map(|(_, transform)| transform.translation())
}

fn feature_edge_is_visible(
    world: &World,
    entity: Entity,
    edge_midpoint: Vec3,
    camera_position: Vec3,
) -> bool {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return true;
    };
    let Some(evaluated) = entity_ref.get::<EvaluatedFeature>() else {
        return true;
    };

    let to_midpoint = edge_midpoint - camera_position;
    let max_distance = to_midpoint.length();
    if max_distance <= 1e-5 {
        return true;
    }
    let Ok(direction) = Dir3::new(to_midpoint) else {
        return true;
    };
    let ray = Ray3d::new(camera_position, direction);
    let visibility_epsilon = 0.01f32.max(max_distance * 1e-3);
    let mut nearest_hit = f32::INFINITY;

    for tri in evaluated.indices.chunks(3) {
        if tri.len() != 3 {
            continue;
        }
        let v0 = evaluated.vertices[tri[0] as usize];
        let v1 = evaluated.vertices[tri[1] as usize];
        let v2 = evaluated.vertices[tri[2] as usize];
        if let Some(distance) = ray_triangle_intersection(ray, v0, v1, v2) {
            nearest_hit = nearest_hit.min(distance);
        }
    }

    nearest_hit >= max_distance - visibility_epsilon
}

fn draw_face_outline_for_entity(
    world: &World,
    gizmos: &mut Gizmos,
    entity: Entity,
    verts: &[Vec3],
    color: Color,
) {
    let n = verts.len();
    if n < 2 {
        return;
    }

    let camera_position = world
        .get_entity(entity)
        .ok()
        .is_some_and(|entity_ref| entity_ref.get::<FaceProfileFeature>().is_some())
        .then(|| active_camera_position(world))
        .flatten();

    if camera_position.is_none() {
        draw_face_outline(gizmos, verts, color);
        return;
    }

    for i in 0..n {
        let start = verts[i];
        let end = verts[(i + 1) % n];
        let visible = camera_position.is_none_or(|camera_position| {
            feature_edge_is_visible(world, entity, (start + end) * 0.5, camera_position)
        });
        if visible {
            gizmos.line(start, end, color);
        }
    }
}

/// Get the world-space vertex positions of a face for any entity type.
fn get_face_verts(world: &World, entity: Entity, face_id: FaceId) -> Option<Vec<Vec3>> {
    let entity_ref = world.get_entity(entity).ok()?;

    if entity_ref.get::<FaceProfileFeature>().is_some() {
        return feature_face_vertices(world, entity, face_id);
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
    let mut camera_query = world.try_query::<(&Camera, &GlobalTransform)>()?;
    camera_query
        .iter(world)
        .find(|(camera, _)| camera.is_active)
        .or_else(|| camera_query.iter(world).next())
        .map(|(camera, transform)| (camera.clone(), transform.clone()))
}

fn viewport_cursor_position(world: &World, camera: &Camera) -> Option<Vec2> {
    let mut window_query = world.try_query_filtered::<&Window, With<PrimaryWindow>>()?;
    let window = window_query.single(world).ok()?;
    let cursor_position = window.cursor_position()?;
    Some(match camera.logical_viewport_rect() {
        Some(rect) => cursor_position - rect.min,
        None => cursor_position,
    })
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
    let drag_plane =
        build_push_pull_drag_plane(camera_transform, selected_face.centroid, face_normal);
    let initial_cursor_screen = active_camera
        .as_ref()
        .and_then(|(camera, _)| viewport_cursor_position(world, camera));
    let screen_normal = active_camera
        .as_ref()
        .and_then(|(camera, camera_transform)| {
            projected_face_normal_screen(
                camera,
                camera_transform,
                selected_face.centroid,
                face_normal,
            )
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
/// When a face is selected, tools project onto it. When deselected, revert to ground.
/// During push/pull, the frozen drag plane takes priority — don't overwrite it.
fn sync_drawing_plane_to_face(
    face_context: Res<FaceEditContext>,
    push_pull: Res<PushPullContext>,
    mut drawing_plane: ResMut<DrawingPlane>,
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
    } else if !drawing_plane.is_ground() {
        *drawing_plane = DrawingPlane::ground();
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

    let mut parts = Vec::new();
    parts.push("Face editing".to_string());
    if drawing.active {
        let n = drawing.points.len();
        let mode = if drawing.arc_mode { "arc" } else { "line" };
        if n >= 3 {
            parts.push(format!(
                "Drawing ({mode}) {n} pts · close near start · A: arc · Enter to finish · Esc to cancel"
            ));
        } else {
            parts.push(format!(
                "Drawing ({mode}) {n} pts · click to add · A: arc · Esc to cancel"
            ));
        }
    } else if let Some(selected) = &face_context.selected_face {
        let label = selected
            .generated_face_ref
            .as_ref()
            .map(GeneratedFaceRef::label)
            .unwrap_or_else(|| format!("{}", selected.face_id.0));
        let push_pull_hint =
            selected_face_push_pull_affordance(world, &face_context, selected).status_hint();
        parts.push(format!(
            "Face {label} selected · {push_pull_hint} · L: draw on face"
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
    ownership: Res<InputOwnership>,
) {
    if !ownership.is_idle() || drawing.active {
        return;
    }
    if face_context.selected_face.is_none() {
        return;
    }
    if keys.just_pressed(KeyCode::KeyL) {
        drawing.active = true;
        drawing.points.clear();
        drawing.segment_is_arc.clear();
        drawing.arc_mode = false;
    }
}

/// Handle clicks and key presses while in face drawing mode.
fn handle_face_drawing_input(world: &mut World) {
    let egui = world.resource::<EguiWantsInput>().clone();
    let keys = world.resource::<ButtonInput<KeyCode>>().clone();
    let mouse = world.resource::<ButtonInput<MouseButton>>().clone();

    // A key toggles arc mode
    if !egui.keyboard && keys.just_pressed(KeyCode::KeyA) {
        let mut drawing = world.resource_mut::<FaceDrawingContext>();
        drawing.arc_mode = !drawing.arc_mode;
        return;
    }

    // Escape cancels drawing
    if !egui.keyboard && keys.just_pressed(KeyCode::Escape) {
        let mut drawing = world.resource_mut::<FaceDrawingContext>();
        drawing.active = false;
        drawing.points.clear();
        drawing.segment_is_arc.clear();
        drawing.arc_mode = false;
        return;
    }

    // Enter closes and finishes
    if !egui.keyboard
        && (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter))
    {
        let n = world.resource::<FaceDrawingContext>().points.len();
        if n >= 3 {
            finish_face_drawing(world);
        }
        return;
    }

    // Left click adds a point
    if !egui.pointer && mouse.just_pressed(MouseButton::Left) {
        let cursor_pos = world.resource::<CursorWorldPos>().snapped;
        let Some(cursor_pos) = cursor_pos else {
            return;
        };

        let points_clone = world.resource::<FaceDrawingContext>().points.clone();

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
    let mut q = world.query::<(&Camera, &GlobalTransform)>();
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

    if drawing.points.len() < 3 {
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

    let (pmin, pmax) = profile.bounds_2d();
    let mid_2d = (pmin + pmax) * 0.5;
    let centred_profile = profile.translated(-mid_2d);

    let centre_on_plane = plane.to_world(mid_2d);

    // Build rotation from the drawing plane's tangent frame.
    // local X → plane.tangent, local Y → plane.normal, local Z → plane.bitangent
    let rotation = Quat::from_mat3(&Mat3::from_cols(
        plane.tangent,
        plane.normal,
        plane.bitangent,
    ));

    let element_id = world.resource::<ElementIdAllocator>().next_id();
    let parent_element_id = world
        .resource::<FaceEditContext>()
        .element_id
        .unwrap_or(ElementId(0));
    let support_face = world
        .resource::<FaceEditContext>()
        .selected_face
        .as_ref()
        .and_then(|face| face.generated_face_ref.clone());
    let snapshot = make_face_profile_feature_snapshot(
        element_id,
        parent_element_id,
        centred_profile,
        centre_on_plane,
        rotation,
        support_face,
    );

    // Apply immediately so the entity exists this frame
    use crate::authored_entity::AuthoredEntity;
    snapshot.apply_to(world);

    // Find the new entity
    let new_entity = {
        let mut q = world.query::<(Entity, &ElementId)>();
        q.iter(world)
            .find(|(_, eid)| **eid == element_id)
            .map(|(e, _)| e)
    };

    // Send command for undo/redo
    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: snapshot.into(),
        });

    // Clear drawing state
    {
        let mut d = world.resource_mut::<FaceDrawingContext>();
        d.active = false;
        d.points.clear();
        d.segment_is_arc.clear();
        d.arc_mode = false;
    }

    // Exit face-edit on the original entity, select the new one.
    // The user can double-click it to face-edit and push/pull to adjust height.
    world.resource_mut::<FaceEditContext>().exit();

    if let Some(new_entity) = new_entity {
        if let Ok(mut e) = world.get_entity_mut(new_entity) {
            e.insert(Selected);
        }
    }
}

/// Draw preview lines for face drawing mode.
fn draw_face_drawing_preview(
    drawing: Res<FaceDrawingContext>,
    cursor_world_pos: Res<CursorWorldPos>,
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
