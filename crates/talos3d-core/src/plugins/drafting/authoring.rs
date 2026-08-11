//! The one authoring contribution Drafting makes to ordinary 3D model edits.
//!
//! Drafting does not own geometry, snapshot types, or edit plans. While the
//! workspace is active it supplies exactly two things to the existing authoring
//! pipeline:
//!
//! 1. a **local authoring frame** derived from the plane the user is drawing
//!    on, so coordinates authored by a tool, a command, or an agent land on
//!    that plane instead of the world ground plane; and
//! 2. a **membership target**, so authored model entities join the active
//!    Draft in the same atomic history step that created them.
//!
//! Both are consumed through machinery that already exists —
//! [`compose_snapshot_into_frame`](crate::plugins::modeling::group::compose_snapshot_into_frame)
//! (ADR-058 local-frame authoring) and ordinary
//! [`ApplyEntityChangesCommand`](crate::plugins::commands::ApplyEntityChangesCommand)
//! plans — so there is no second placement, movement, or membership rule set.

use bevy::prelude::*;

use crate::{
    authored_entity::BoxedEntity,
    plugins::{
        commands::find_entity_by_element_id_readonly, cursor::DrawingPlane, identity::ElementId,
        modeling::group::GroupFrame,
    },
};

use super::{
    draft::{DraftNode, DraftSnapshot},
    workspace::DraftingWorkspaceState,
};

/// The active Draft's own plane, given already-borrowed state.
///
/// This is the *derivation* used to seed and restore the shared coordinate
/// authority — the Drafting controller uses it on a selection transition and
/// face-edit uses it to decide what the cursor plane rests on. Authoring code
/// must not call it: read [`active_drafting_plane`] instead, so there is one
/// plane every surface agrees on.
pub fn resting_draft_plane(
    workspace: Option<&DraftingWorkspaceState>,
    mut plane_of: impl FnMut(ElementId) -> Option<DrawingPlane>,
) -> Option<DrawingPlane> {
    let workspace = workspace.filter(|state| state.is_active())?;
    plane_of(workspace.active_draft_id()?)
}

/// The plane authored coordinates are currently expressed in, or `None` when
/// Drafting is not active.
///
/// This is deliberately the shared [`DrawingPlane`] resource rather than a
/// second read of the durable Draft. The ratified boundary is that a Draft is
/// the membership and container authority while `DrawingPlane` is the
/// coordinate authority: the Drafting controller writes the selected Draft's
/// plane into it on every selection or Draft change, face-edit restores that
/// same resting plane whenever no face owns the cursor, and a selected face may
/// deliberately retarget it. Reading it here means 3D creation, 2D annotation
/// authoring, transforms, and the cursor all resolve against exactly one plane.
#[must_use]
pub fn active_drafting_plane(world: &World) -> Option<DrawingPlane> {
    world
        .get_resource::<DraftingWorkspaceState>()
        .filter(|state| state.is_active())?;
    world.get_resource::<DrawingPlane>().cloned()
}

/// The rigid local→world frame implied by a drawing plane.
///
/// Existing 3D authoring is expressed in the ground-plane convention: local
/// `X`/`Z` span the drawing surface and local `+Y` is the height/extrusion
/// axis. That convention is carried onto an arbitrary plane by mapping
///
/// - local `+X` → `tangent` (screen right),
/// - local `+Y` → `-normal` (out of the drawing, toward the viewer),
/// - local `+Z` → `-bitangent` (screen down).
///
/// The Draft camera looks *along* `normal`, so `-normal` is the axis a solid
/// grows out of the sheet on, and `-bitangent` keeps `+Z` pointing away from
/// the viewer's "up" exactly as `+Z` does in a top view of the ground plane.
///
/// The mapping is a proper rotation for every valid plane, because the plane
/// frame is canonically right-handed (`tangent × normal = bitangent`), and it
/// is the identity for the default plan Draft (`normal = -Y`, `tangent = X`,
/// `bitangent = -Z`) — so plan drafting leaves existing authoring untouched.
#[must_use]
pub fn plane_authoring_frame(plane: &DrawingPlane) -> GroupFrame {
    GroupFrame {
        translation: plane.origin,
        rotation: Quat::from_mat3(&Mat3::from_cols(
            plane.tangent,
            -plane.normal,
            -plane.bitangent,
        ))
        .normalize(),
    }
}

/// The authoring frame Drafting contributes right now, if any.
#[must_use]
pub fn active_drafting_frame(world: &World) -> Option<GroupFrame> {
    active_drafting_plane(world).map(|plane| plane_authoring_frame(&plane))
}

// ---------------------------------------------------------------------------
// The plane-relative transform rules.
//
// Each rule is defined exactly once here and consumed by both the viewport and
// the model API, so a drag and the equivalent agent call cannot drift apart.
// The viewport reaches them through `TransformState`'s axis constraint and
// cursor plane; the model API reaches them through the request's free value.
// ---------------------------------------------------------------------------

/// The axis an unconstrained rotation turns about: the frame's local `+Y`,
/// which is the drawing plane's view-facing normal. Reduces to world `+Y`
/// outside Drafting and on a plan Draft.
///
/// Normalized on the way out. Rotating a basis vector through the frame's
/// quaternion accumulates enough float error to leave the axis a few `1e-7`
/// off unit length, and both consumers — `Quat::from_axis_angle` and the
/// viewport's custom-axis constraint — require a genuine unit axis.
#[must_use]
pub fn authoring_rotation_axis(frame: &GroupFrame) -> Vec3 {
    (frame.rotation * Vec3::Y).normalize()
}

/// The normal of the plane an unconstrained move resolves on. A drag tracks the
/// cursor against this plane through the grab point, so the delta it produces
/// always lies in the drawing surface.
#[must_use]
pub fn authoring_drag_plane_normal(frame: &GroupFrame) -> Vec3 {
    -authoring_rotation_axis(frame)
}

/// Map a free, unconstrained translation authored in this frame to world space.
/// `[x, 0, z]` spans exactly the deltas a drag can reach; `[0, y, 0]` is the one
/// direction it cannot, out of the sheet toward the viewer.
#[must_use]
pub fn authoring_delta_to_world(frame: &GroupFrame, delta: Vec3) -> Vec3 {
    frame.rotation * delta
}

/// The Draft that newly authored model entities should join, if any.
#[must_use]
pub fn active_draft_membership_target(world: &World) -> Option<ElementId> {
    world
        .get_resource::<DraftingWorkspaceState>()
        .filter(|state| state.is_active())
        .and_then(DraftingWorkspaceState::active_draft_id)
}

/// Before/after Draft snapshots that add `new_member` to `draft_id`.
///
/// Mirrors
/// [`group_membership_add_snapshots`](crate::plugins::modeling::group::group_membership_add_snapshots):
/// membership is an ordinary authored-entity change so it travels through the
/// same command, history, and persistence path as everything else. Returns
/// `None` when the Draft is gone or already references the member, so an
/// idempotent call enqueues nothing.
#[must_use]
pub fn draft_membership_add_snapshots(
    world: &World,
    draft_id: ElementId,
    new_member: ElementId,
) -> Option<(BoxedEntity, BoxedEntity)> {
    if draft_id == new_member {
        return None;
    }
    let entity = find_entity_by_element_id_readonly(world, draft_id)?;
    let node = world.get::<DraftNode>(entity)?;
    if node.contains(new_member) {
        return None;
    }
    let before = DraftSnapshot {
        element_id: draft_id,
        node: node.clone(),
    };
    let mut after = before.clone();
    after.node.members.push(new_member);
    after.node.normalize_and_validate().ok()?;
    Some((before.into(), after.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use bevy::camera::Projection;

    use crate::{
        capability_registry::CapabilityRegistry,
        plugins::{
            camera::{CameraControlsState, OrbitCamera},
            commands::{enqueue_create_box, CommandPlugin, CreateBoxCommand},
            compass::CompassSettings,
            document_properties::DocumentProperties,
            drafting_sheet::DrawingSceneLiveCache,
            history::{History, HistoryPlugin, PendingCommandQueue},
            identity::ElementIdAllocator,
            modeling::{
                generic_factory::PrimitiveFactory,
                generic_snapshot::PrimitiveSnapshot,
                primitives::{BoxPrimitive, ShapeRotation},
            },
            render_pipeline::RenderSettings,
        },
    };

    use super::super::{
        draft::{execute_create_draft, DraftNode},
        visibility::DraftingVisibility,
        workspace::execute_toggle_drafting_workspace,
    };

    fn plane(origin: Vec3, normal: Vec3, tangent: Vec3) -> DrawingPlane {
        DrawingPlane::try_from_origin_normal_tangent(origin, normal, tangent).expect("valid plane")
    }

    /// A world with the real Drafting controller, command queue, and history, so
    /// these tests exercise the production entry points rather than a stand-in.
    fn drafting_app() -> App {
        let mut app = App::new();
        app.add_plugins((HistoryPlugin, CommandPlugin))
            .insert_resource(ElementIdAllocator::default())
            .insert_resource(CapabilityRegistry::default())
            .insert_resource(DocumentProperties::default())
            .insert_resource(CameraControlsState::default())
            .insert_resource(RenderSettings::default())
            .init_resource::<DraftingWorkspaceState>()
            .init_resource::<DraftingVisibility>()
            .init_resource::<DrawingSceneLiveCache>()
            .init_resource::<CompassSettings>()
            .init_resource::<DrawingPlane>();
        app.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        app.world_mut().spawn((
            OrbitCamera::default(),
            Transform::default(),
            Projection::Perspective(PerspectiveProjection::default()),
        ));
        app
    }

    /// Create a Draft on `plane`, then enter Drafting with it selected.
    fn enter_drafting_on(app: &mut App, plane: &DrawingPlane) -> ElementId {
        let result = execute_create_draft(
            app.world_mut(),
            &json!({
                "name": "Working Draft",
                "origin": [plane.origin.x, plane.origin.y, plane.origin.z],
                "normal": [plane.normal.x, plane.normal.y, plane.normal.z],
                "tangent": [plane.tangent.x, plane.tangent.y, plane.tangent.z],
            }),
        )
        .expect("create Draft");
        app.update();
        execute_toggle_drafting_workspace(app.world_mut(), &json!({"enabled": true}))
            .expect("enter Drafting");
        ElementId(result.created[0])
    }

    fn authored_box(world: &World, element_id: ElementId) -> (BoxPrimitive, Quat) {
        let entity = find_entity_by_element_id_readonly(world, element_id).expect("authored box");
        (
            world
                .get::<BoxPrimitive>(entity)
                .expect("box primitive")
                .clone(),
            world
                .get::<ShapeRotation>(entity)
                .copied()
                .unwrap_or_default()
                .0,
        )
    }

    fn only_authored_box(world: &mut World) -> ElementId {
        let mut query = world.query_filtered::<&ElementId, With<BoxPrimitive>>();
        let ids = query.iter(world).copied().collect::<Vec<_>>();
        assert_eq!(ids.len(), 1, "expected exactly one authored box");
        ids[0]
    }

    fn draft_members(world: &World, draft_id: ElementId) -> Vec<ElementId> {
        find_entity_by_element_id_readonly(world, draft_id)
            .and_then(|entity| world.get::<DraftNode>(entity))
            .map(|node| node.members.clone())
            .unwrap_or_default()
    }

    fn unit_box(element_id: ElementId, centre: Vec3) -> BoxedEntity {
        PrimitiveSnapshot {
            element_id,
            primitive: BoxPrimitive {
                centre,
                half_extents: Vec3::splat(0.5),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .into()
    }

    #[test]
    fn creation_outside_drafting_stays_world_space_and_unscoped() {
        let mut app = drafting_app();
        let draft_id = enter_drafting_on(
            &mut app,
            &plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X),
        );
        execute_toggle_drafting_workspace(app.world_mut(), &json!({"enabled": false}))
            .expect("leave Drafting");

        let element_id = ElementId(500);
        crate::plugins::commands::enqueue_create_boxed_entity(
            app.world_mut(),
            unit_box(element_id, Vec3::new(2.0, 1.0, 3.0)),
        );
        app.update();

        let (primitive, rotation) = authored_box(app.world(), element_id);
        assert_eq!(primitive.centre, Vec3::new(2.0, 1.0, 3.0));
        assert!(rotation.abs_diff_eq(Quat::IDENTITY, 1e-5));
        assert!(
            draft_members(app.world(), draft_id).is_empty(),
            "an inactive Drafting workspace must not scope model authoring"
        );
    }

    #[test]
    fn creation_on_a_plan_draft_leaves_ground_plane_authoring_untouched() {
        let mut app = drafting_app();
        let draft_id = enter_drafting_on(&mut app, &DraftNode::new("Plan").plane);

        let element_id = ElementId(500);
        crate::plugins::commands::enqueue_create_boxed_entity(
            app.world_mut(),
            unit_box(element_id, Vec3::new(2.0, 1.0, 3.0)),
        );
        app.update();

        let (primitive, rotation) = authored_box(app.world(), element_id);
        assert_eq!(primitive.centre, Vec3::new(2.0, 1.0, 3.0));
        assert!(rotation.abs_diff_eq(Quat::IDENTITY, 1e-5));
        assert_eq!(draft_members(app.world(), draft_id), vec![element_id]);
    }

    #[test]
    fn creation_on_an_elevation_draft_is_seated_on_the_plane_and_scoped_to_it() {
        let mut app = drafting_app();
        let elevation = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        let draft_id = enter_drafting_on(&mut app, &elevation);

        let element_id = ElementId(500);
        crate::plugins::commands::enqueue_create_boxed_entity(
            app.world_mut(),
            unit_box(element_id, Vec3::new(2.0, 1.0, 0.0)),
        );
        app.update();

        let (primitive, rotation) = authored_box(app.world(), element_id);
        // Local +X runs along the plane tangent and local +Y out of the sheet.
        assert!(
            primitive
                .centre
                .abs_diff_eq(Vec3::new(2.0, 0.0, -3.0), 1e-5),
            "authored centre {:?} was not seated on the Draft plane",
            primitive.centre
        );
        assert!(
            (rotation * Vec3::Y).abs_diff_eq(-elevation.normal, 1e-5),
            "the solid must be oriented by the same frame that placed it"
        );
        assert_eq!(draft_members(app.world(), draft_id), vec![element_id]);
        assert!(
            find_entity_by_element_id_readonly(app.world(), element_id)
                .and_then(|entity| app.world().get::<DraftNode>(entity))
                .is_none(),
            "the box must stay an ordinary authored model entity"
        );
    }

    #[test]
    fn creation_and_draft_membership_undo_and_redo_as_one_step() {
        let mut app = drafting_app();
        let draft_id = enter_drafting_on(
            &mut app,
            &plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X),
        );
        let element_id = ElementId(500);
        crate::plugins::commands::enqueue_create_boxed_entity(
            app.world_mut(),
            unit_box(element_id, Vec3::new(2.0, 1.0, 0.0)),
        );
        app.update();
        assert_eq!(draft_members(app.world(), draft_id), vec![element_id]);

        app.world_mut()
            .resource_mut::<PendingCommandQueue>()
            .queue_undo();
        app.update();
        assert!(
            find_entity_by_element_id_readonly(app.world(), element_id).is_none(),
            "one undo must remove the authored entity"
        );
        assert!(
            draft_members(app.world(), draft_id).is_empty(),
            "the same undo must drop the membership, never leave a dangling reference"
        );

        app.world_mut()
            .resource_mut::<PendingCommandQueue>()
            .queue_redo();
        app.update();
        assert!(find_entity_by_element_id_readonly(app.world(), element_id).is_some());
        assert_eq!(draft_members(app.world(), draft_id), vec![element_id]);
        assert_eq!(
            app.world().resource::<History>().undo_stack_len(),
            2,
            "Draft creation plus one grouped create-and-scope action"
        );
    }

    #[test]
    fn viewport_tool_and_agent_command_author_the_same_entity() {
        let elevation = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        let request = CreateBoxCommand {
            centre: Vec3::new(2.0, 1.0, 0.0),
            half_extents: Vec3::splat(0.5),
        };

        // The interactive tool path: a tool writes a CreateBoxCommand message.
        let mut ui = drafting_app();
        let ui_draft = enter_drafting_on(&mut ui, &elevation);
        ui.world_mut().write_message(request.clone());
        ui.update();
        let ui_id = only_authored_box(ui.world_mut());

        // The agent path: the command/model-api surface enqueues directly.
        let mut agent = drafting_app();
        let agent_draft = enter_drafting_on(&mut agent, &elevation);
        let agent_id = enqueue_create_box(agent.world_mut(), request);
        agent.update();

        assert_eq!(
            authored_box(ui.world(), ui_id),
            authored_box(agent.world(), agent_id),
            "the viewport and agent surfaces must share one placement rule"
        );
        assert_eq!(draft_members(ui.world(), ui_draft), vec![ui_id]);
        assert_eq!(draft_members(agent.world(), agent_draft), vec![agent_id]);
    }

    /// Both surfaces must resolve an unconstrained rotation to the same axis.
    /// The viewport reaches it through the transform state's axis constraint and
    /// the agent surface through the authoring frame; if those ever diverge, a
    /// drag and the equivalent `transform` call would twist different ways.
    #[cfg(feature = "model-api")]
    #[test]
    fn viewport_and_agent_rotate_about_the_same_draft_axis() {
        use crate::plugins::{
            model_api::transform_rotation,
            model_api::TransformToolRequest,
            transform::{drafting_rotation_axis, rotation_quat, AxisConstraint, TransformMode},
        };

        for surface in [
            plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X),
            plane(Vec3::new(0.0, 3.0, 0.0), Vec3::NEG_Y, Vec3::X),
            plane(Vec3::new(2.0, 0.0, 0.0), Vec3::X, Vec3::Z),
        ] {
            let mut app = drafting_app();
            enter_drafting_on(&mut app, &surface);

            let axis =
                drafting_rotation_axis(app.world(), TransformMode::Rotating, AxisConstraint::None);
            let AxisConstraint::Custom(direction) = axis else {
                panic!("Drafting must resolve an unconstrained rotation to a plane axis");
            };
            assert!(direction.abs_diff_eq(-surface.normal, 1e-5));
            assert!(
                direction.is_normalized(),
                "rotation axis must be unit length"
            );
            let viewport = rotation_quat(axis, 30f32.to_radians());

            let request = TransformToolRequest {
                element_ids: vec![1],
                operation: "rotate".to_string(),
                axis: None,
                value: json!(30.0),
                pivot: None,
            };
            let agent = transform_rotation(
                &request,
                &active_drafting_frame(app.world()).expect("Drafting is active"),
            )
            .expect("rotation");

            assert!(
                viewport.abs_diff_eq(agent, 1e-5),
                "viewport {viewport:?} and agent {agent:?} disagreed on the Draft rotation axis"
            );
        }
    }

    /// An unconstrained viewport drag resolves the cursor on a plane parallel to
    /// the drawing surface, so its delta always lies in that plane. The agent
    /// surface must be able to express exactly those deltas — and no others —
    /// with a free `[x, 0, z]` value.
    #[cfg(feature = "model-api")]
    #[test]
    fn agent_free_move_deltas_span_the_same_plane_the_viewport_drag_does() {
        use crate::plugins::{
            model_api::transform_move_delta, model_api::TransformToolRequest,
            transform::drafting_move_plane_normal,
        };

        let surface = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        let mut app = drafting_app();
        enter_drafting_on(&mut app, &surface);

        let drag_normal = drafting_move_plane_normal(app.world());
        assert_eq!(drag_normal, surface.normal);
        let frame = active_drafting_frame(app.world()).expect("Drafting is active");

        let free = |value: serde_json::Value| {
            transform_move_delta(
                &TransformToolRequest {
                    element_ids: vec![1],
                    operation: "move".to_string(),
                    axis: None,
                    value,
                    pivot: None,
                },
                &frame,
            )
            .expect("move delta")
        };

        let in_plane = free(json!([2.0, 0.0, 3.0]));
        assert!(
            in_plane.dot(drag_normal).abs() < 1e-5,
            "a free [x, 0, z] delta must stay in the plane a drag can reach"
        );
        assert!(in_plane.abs_diff_eq(surface.tangent * 2.0 - surface.bitangent * 3.0, 1e-5));
        // Local +Y is the one direction a drag cannot reach: out of the sheet.
        assert!(free(json!([0.0, 1.0, 0.0])).abs_diff_eq(-surface.normal, 1e-5));
    }

    /// A live MCP session caught this: `sync_drawing_plane_to_face` reverted the
    /// shared DrawingPlane to ground on every frame with no face selected, one
    /// frame after the Drafting controller installed the Draft plane. Authoring
    /// therefore drifted back to the world ground plane while an elevation Draft
    /// was still active, and the cursor projected against ground too.
    #[test]
    fn the_draft_plane_survives_the_face_edit_sync_and_keeps_authoring_seated() {
        use crate::plugins::face_edit::{
            sync_drawing_plane_to_face, FaceEditContext, PushPullContext,
        };

        let elevation = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        let mut app = drafting_app();
        let draft_id = enter_drafting_on(&mut app, &elevation);
        app.init_resource::<FaceEditContext>()
            .init_resource::<PushPullContext>()
            .add_systems(Update, sync_drawing_plane_to_face);

        // Several frames of ordinary idle running, exactly as the live app does.
        for _ in 0..3 {
            app.update();
        }

        assert_eq!(
            *app.world().resource::<DrawingPlane>(),
            elevation,
            "the shared drawing plane must stay on the active Draft while Drafting"
        );
        assert_eq!(
            active_drafting_plane(app.world()).as_ref(),
            Some(&elevation),
            "authoring must resolve against that same shared coordinate authority"
        );

        let element_id = ElementId(500);
        crate::plugins::commands::enqueue_create_boxed_entity(
            app.world_mut(),
            unit_box(element_id, Vec3::new(2.0, 1.0, 0.0)),
        );
        app.update();
        let (primitive, _) = authored_box(app.world(), element_id);
        assert!(
            primitive
                .centre
                .abs_diff_eq(Vec3::new(2.0, 0.0, -3.0), 1e-5),
            "authoring drifted off the Draft plane to {:?}",
            primitive.centre
        );
        assert_eq!(draft_members(app.world(), draft_id), vec![element_id]);
    }

    /// The Draft is the membership/container authority and `DrawingPlane` is the
    /// coordinate authority. A face may deliberately retarget the cursor plane;
    /// deselecting it must return to the Draft's plane, not to ground, and every
    /// authoring and transform path must follow that one resource throughout.
    #[test]
    fn a_non_plan_draft_is_the_resting_plane_across_frames_and_face_retargeting() {
        use crate::plugins::face_edit::{
            sync_drawing_plane_to_face, FaceEditContext, PushPullContext, SelectedFace,
        };
        use crate::plugins::transform::drafting_move_plane_normal;

        let elevation = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        let mut app = drafting_app();
        enter_drafting_on(&mut app, &elevation);
        app.init_resource::<FaceEditContext>()
            .init_resource::<PushPullContext>()
            .add_systems(Update, sync_drawing_plane_to_face);

        let follows_one_resource = |app: &App, expected: &DrawingPlane| {
            assert_eq!(app.world().resource::<DrawingPlane>(), expected);
            assert_eq!(active_drafting_plane(app.world()).as_ref(), Some(expected));
            assert_eq!(
                active_drafting_frame(app.world()),
                Some(plane_authoring_frame(expected))
            );
            assert!(drafting_move_plane_normal(app.world()).abs_diff_eq(expected.normal, 1e-5));
        };

        for _ in 0..3 {
            app.update();
        }
        follows_one_resource(&app, &elevation);

        // A selected face intentionally retargets the shared coordinate authority.
        let face = DrawingPlane::from_face(Vec3::new(0.0, 2.0, 0.0), Vec3::Y);
        app.world_mut()
            .resource_mut::<FaceEditContext>()
            .selected_face = Some(SelectedFace {
            face_id: crate::capability_registry::FaceId(0),
            generated_face_ref: None,
            normal: face.normal,
            centroid: face.origin,
        });
        app.update();
        follows_one_resource(&app, &face);

        // Deselecting returns to the Draft's plane, never to ground.
        app.world_mut()
            .resource_mut::<FaceEditContext>()
            .selected_face = None;
        for _ in 0..3 {
            app.update();
        }
        follows_one_resource(&app, &elevation);
    }

    #[test]
    fn leaving_drafting_returns_the_resting_plane_to_ground() {
        use crate::plugins::face_edit::{
            sync_drawing_plane_to_face, FaceEditContext, PushPullContext,
        };

        let mut app = drafting_app();
        enter_drafting_on(
            &mut app,
            &plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X),
        );
        app.init_resource::<FaceEditContext>()
            .init_resource::<PushPullContext>()
            .add_systems(Update, sync_drawing_plane_to_face);
        app.update();

        execute_toggle_drafting_workspace(app.world_mut(), &json!({"enabled": false}))
            .expect("leave Drafting");
        app.update();

        assert!(app.world().resource::<DrawingPlane>().is_ground());
        assert!(active_drafting_plane(app.world()).is_none());
    }

    #[test]
    fn drawing_metadata_is_never_reframed_or_auto_scoped() {
        let mut app = drafting_app();
        let elevation = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        let draft_id = enter_drafting_on(&mut app, &elevation);

        // A second Draft is drawing metadata: it must not be reframed by the
        // active plane, and must never become a member of another Draft.
        let second = execute_create_draft(
            app.world_mut(),
            &json!({"name": "Other", "origin": [1.0, 2.0, 3.0]}),
        )
        .expect("create second Draft");
        app.update();
        let second_id = ElementId(second.created[0]);

        let entity = find_entity_by_element_id_readonly(app.world(), second_id).expect("Draft");
        assert_eq!(
            app.world().get::<DraftNode>(entity).unwrap().plane.origin,
            Vec3::new(1.0, 2.0, 3.0),
            "drawing metadata keeps its own plane frame"
        );
        assert!(!draft_members(app.world(), draft_id).contains(&second_id));
    }

    #[test]
    fn default_plan_draft_frame_is_the_identity_authoring_frame() {
        let frame = plane_authoring_frame(&DraftNode::new("Plan").plane);
        assert!(
            frame.is_identity(),
            "a default plan Draft must not re-interpret existing ground-plane authoring"
        );
    }

    #[test]
    fn frame_is_a_proper_rotation_that_extrudes_toward_the_viewer() {
        for candidate in [
            plane(Vec3::new(0.0, 3.0, 0.0), Vec3::NEG_Y, Vec3::X),
            plane(Vec3::new(0.0, 0.0, 4.0), Vec3::Z, Vec3::X),
            plane(Vec3::new(2.0, 1.0, 0.0), Vec3::X, Vec3::Z),
            plane(
                Vec3::ZERO,
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(1.0, -1.0, 0.0),
            ),
        ] {
            let frame = plane_authoring_frame(&candidate);
            assert!(frame.rotation.is_normalized());
            let local_x = frame.rotation * Vec3::X;
            let local_y = frame.rotation * Vec3::Y;
            let local_z = frame.rotation * Vec3::Z;
            assert!(local_x.abs_diff_eq(candidate.tangent, 1e-5));
            assert!(
                local_y.abs_diff_eq(-candidate.normal, 1e-5),
                "height must grow out of the sheet toward the viewer"
            );
            assert!(local_z.abs_diff_eq(-candidate.bitangent, 1e-5));
            assert!(
                (local_x.cross(local_y).dot(local_z) - 1.0).abs() < 1e-4,
                "the authoring frame must stay right-handed, never mirrored"
            );
        }
    }

    #[test]
    fn frame_seats_local_ground_authoring_onto_the_plane() {
        // A front-elevation Draft: the drawing surface is the world XY plane at
        // z = -4, viewed from +Z, so screen right is +X and screen up is +Y.
        let elevation = plane(Vec3::new(0.0, 0.0, -4.0), Vec3::NEG_Z, Vec3::X);
        assert!(elevation.bitangent.abs_diff_eq(Vec3::Y, 1e-5));
        let frame = plane_authoring_frame(&elevation);

        // A metre "up" in the tool's local frame is a metre out of the sheet.
        assert!(frame
            .point_to_world(Vec3::Y)
            .abs_diff_eq(Vec3::new(0.0, 0.0, -3.0), 1e-5));
        // Local X/Z stay on the drawing surface: local +X is screen right and
        // local +Z is screen down, exactly as in a top view of the ground plane.
        assert!(frame
            .point_to_world(Vec3::new(2.0, 0.0, 3.0))
            .abs_diff_eq(Vec3::new(2.0, -3.0, -4.0), 1e-5));
    }
}
