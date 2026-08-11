//! Authoritative runtime controller for the one Drafting workspace.
//!
//! Drafting is presentation/session state. Durable drafts, planes, membership,
//! and annotations are separate authored data; this controller owns only the
//! reversible viewport transition required by ADR-026.

use bevy::prelude::*;
use serde_json::{json, Value};

use crate::plugins::{
    camera::{apply_orbit_state, CameraControlsState, CameraProjectionMode, OrbitCamera},
    command_registry::CommandResult,
    compass::CompassSettings,
    cursor::DrawingPlane,
    drafting_sheet::DrawingSceneLiveCache,
    identity::ElementId,
    render_pipeline::{apply_drafting_render_preset, drafting_surface_evidence, RenderSettings},
    ui::StatusBarData,
};

use super::visibility::DraftingVisibility;

#[derive(Debug, Clone)]
struct OrbitCameraBaseline {
    focus: Vec3,
    radius: f32,
    orthographic_scale: f32,
    yaw: f32,
    pitch: f32,
    projection_mode: CameraProjectionMode,
    focal_length_mm: f32,
}

impl From<&OrbitCamera> for OrbitCameraBaseline {
    fn from(orbit: &OrbitCamera) -> Self {
        Self {
            focus: orbit.focus,
            radius: orbit.radius,
            orthographic_scale: orbit.orthographic_scale,
            yaw: orbit.yaw,
            pitch: orbit.pitch,
            projection_mode: orbit.projection_mode,
            focal_length_mm: orbit.focal_length_mm,
        }
    }
}

impl OrbitCameraBaseline {
    fn restore(&self, orbit: &mut OrbitCamera) {
        orbit.focus = self.focus;
        orbit.radius = self.radius;
        orbit.orthographic_scale = self.orthographic_scale;
        orbit.yaw = self.yaw;
        orbit.pitch = self.pitch;
        orbit.projection_mode = self.projection_mode;
        orbit.focal_length_mm = self.focal_length_mm;
    }
}

#[derive(Debug, Clone)]
struct DraftingWorkspaceBaseline {
    render: RenderSettings,
    camera_controls: CameraControlsState,
    orbit: Option<OrbitCameraBaseline>,
    annotation_visibility: DraftingVisibility,
    compass_enabled: Option<bool>,
    drawing_plane: Option<DrawingPlane>,
}

/// The single authority for whether the main viewport is in Drafting.
#[derive(Resource, Debug, Default)]
pub struct DraftingWorkspaceState {
    baseline: Option<DraftingWorkspaceBaseline>,
    active_draft_id: Option<ElementId>,
}

impl DraftingWorkspaceState {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.baseline.is_some()
    }

    #[must_use]
    pub fn active_draft_id(&self) -> Option<ElementId> {
        self.active_draft_id
    }

    /// Select the durable Draft consumed by the one workspace controller.
    /// Returns whether the selection actually changed, preserving bounded
    /// Bevy invalidation on idempotent agent calls.
    pub(crate) fn select_draft(&mut self, draft_id: Option<ElementId>) -> bool {
        if self.active_draft_id == draft_id {
            return false;
        }
        self.active_draft_id = draft_id;
        true
    }
}

/// Command handler for `drafting.toggle`.
///
/// `enabled` is optional: omitting it toggles, while supplying it makes the
/// request idempotent for agents and remote clients.
pub(crate) fn execute_toggle_drafting_workspace(
    world: &mut World,
    params: &Value,
) -> Result<CommandResult, String> {
    let active = world
        .get_resource::<DraftingWorkspaceState>()
        .is_some_and(DraftingWorkspaceState::is_active);
    let requested = params
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(!active);

    if requested == active {
        return Ok(workspace_result(world, active));
    }

    if requested {
        enter_drafting(world)?;
        set_feedback(world, "Drafting enabled");
    } else {
        exit_drafting(world)?;
        set_feedback(world, "Drafting disabled");
    }

    Ok(workspace_result(world, requested))
}

/// Read-only command handler for agent/UI state inspection.
pub(crate) fn execute_inspect_drafting_workspace(
    world: &mut World,
    _params: &Value,
) -> Result<CommandResult, String> {
    let state = world
        .get_resource::<DraftingWorkspaceState>()
        .ok_or_else(|| "Drafting workspace state is unavailable".to_string())?;
    Ok(workspace_result(world, state.is_active()))
}

fn workspace_result(world: &World, active: bool) -> CommandResult {
    let drawing_scene = world.get_resource::<DrawingSceneLiveCache>().map(|cache| {
        let stats = cache.stats();
        json!({
            "dirty": cache.is_dirty(),
            "source_model_revision": stats.source_model_revision,
            "rebuild_count": stats.rebuild_count,
            "invalidation_count": stats.invalidation_count,
            "last_invalidation_reasons": cache.last_invalidation_reasons(),
            "last_rebuild_micros": stats.last_rebuild_micros,
            "line_count": stats.last_line_count,
            "text_count": stats.last_text_count
        })
    });
    let surface_evidence = drafting_surface_evidence(world);
    CommandResult {
        output: Some(json!({
            "active": active,
            "projection": if active { "orthographic" } else { "restored" },
            "surface": if active { "black_on_white" } else { "restored" },
            "surface_evidence": {
                "renderable_count": surface_evidence.renderable_count,
                "paper_override_count": surface_evidence.paper_override_count,
                "valid_white_override_count": surface_evidence.valid_white_override_count,
            },
            "active_draft": super::draft::inspect_active_draft(world),
            "drawing_scene": drawing_scene
        })),
        ..CommandResult::default()
    }
}

fn enter_drafting(world: &mut World) -> Result<(), String> {
    let render = world
        .get_resource::<RenderSettings>()
        .cloned()
        .ok_or_else(|| "Render settings are unavailable".to_string())?;
    let camera_controls = world
        .get_resource::<CameraControlsState>()
        .cloned()
        .ok_or_else(|| "Camera controls are unavailable".to_string())?;
    let annotation_visibility = world
        .get_resource::<DraftingVisibility>()
        .cloned()
        .ok_or_else(|| "Drafting annotation visibility is unavailable".to_string())?;
    let orbit = {
        let mut query = world.query::<&OrbitCamera>();
        query.iter(world).next().map(OrbitCameraBaseline::from)
    };
    let compass_enabled = world
        .get_resource::<CompassSettings>()
        .map(|settings| settings.enabled);
    let drawing_plane = world.get_resource::<DrawingPlane>().cloned();
    let default_draft = super::draft::first_draft_id(world);

    let mut state = world
        .get_resource_mut::<DraftingWorkspaceState>()
        .ok_or_else(|| "Drafting workspace state is unavailable".to_string())?;
    if state.is_active() {
        return Ok(());
    }
    state.baseline = Some(DraftingWorkspaceBaseline {
        render,
        camera_controls,
        orbit,
        annotation_visibility,
        compass_enabled,
        drawing_plane,
    });
    if state.active_draft_id.is_none() {
        state.active_draft_id = default_draft;
    }

    apply_active_invariants(world);
    Ok(())
}

fn exit_drafting(world: &mut World) -> Result<(), String> {
    if !world.contains_resource::<RenderSettings>() {
        return Err("Render settings are unavailable".to_string());
    }
    if !world.contains_resource::<CameraControlsState>() {
        return Err("Camera controls are unavailable".to_string());
    }
    if !world.contains_resource::<DraftingVisibility>() {
        return Err("Drafting annotation visibility is unavailable".to_string());
    }

    let baseline = world
        .get_resource_mut::<DraftingWorkspaceState>()
        .ok_or_else(|| "Drafting workspace state is unavailable".to_string())?
        .baseline
        .take();
    let Some(baseline) = baseline else {
        return Ok(());
    };

    *world
        .get_resource_mut::<RenderSettings>()
        .ok_or_else(|| "Render settings are unavailable".to_string())? = baseline.render;
    *world
        .get_resource_mut::<CameraControlsState>()
        .ok_or_else(|| "Camera controls are unavailable".to_string())? = baseline.camera_controls;
    *world
        .get_resource_mut::<DraftingVisibility>()
        .ok_or_else(|| "Drafting annotation visibility is unavailable".to_string())? =
        baseline.annotation_visibility;
    if let (Some(enabled), Some(mut compass)) = (
        baseline.compass_enabled,
        world.get_resource_mut::<CompassSettings>(),
    ) {
        compass.enabled = enabled;
    }
    if let (Some(saved_plane), Some(mut drawing_plane)) = (
        baseline.drawing_plane,
        world.get_resource_mut::<DrawingPlane>(),
    ) {
        if *drawing_plane != saved_plane {
            *drawing_plane = saved_plane;
        }
    }

    if let Some(orbit_baseline) = baseline.orbit {
        let mut query = world.query::<(&mut OrbitCamera, &mut Transform, &mut Projection)>();
        if let Some((mut orbit, mut transform, mut projection)) = query.iter_mut(world).next() {
            orbit_baseline.restore(&mut orbit);
            apply_orbit_state(&orbit, &mut transform, &mut projection);
        }
    }

    Ok(())
}

fn apply_active_invariants(world: &mut World) {
    if let Some(mut settings) = world.get_resource_mut::<RenderSettings>() {
        apply_drafting_render_preset(&mut settings);
    }
    if let Some(mut controls) = world.get_resource_mut::<CameraControlsState>() {
        controls.projection_mode = CameraProjectionMode::Isometric;
    }
    if let Some(mut visibility) = world.get_resource_mut::<DraftingVisibility>() {
        visibility.show_all = true;
    }
    if let Some(mut compass) = world.get_resource_mut::<CompassSettings>() {
        compass.enabled = false;
    }
    apply_active_draft_plane(world);
    apply_active_draft_camera(world);
}

/// Copy the selected durable Draft plane into the canonical interactive tool
/// plane. This is a semantic-state transition, not a per-frame overwrite.
pub(crate) fn apply_active_draft_plane(world: &mut World) {
    let active = world
        .get_resource::<DraftingWorkspaceState>()
        .is_some_and(DraftingWorkspaceState::is_active);
    if !active {
        return;
    }
    let Some(snapshot) = super::draft::active_draft_snapshot(world) else {
        return;
    };
    if let Some(mut drawing_plane) = world.get_resource_mut::<DrawingPlane>() {
        if *drawing_plane != snapshot.node.plane {
            *drawing_plane = snapshot.node.plane;
        }
    }
}

/// Align the existing orbit camera to the selected Draft's exact frame while
/// retaining the camera controller's focus, pan, zoom, and projection logic.
/// The camera looks along the plane normal; its screen-right and screen-up axes
/// are the plane tangent and bitangent respectively. This matches the existing
/// `DrawingPlane` handedness (`tangent × normal = bitangent`) exactly.
pub(crate) fn apply_active_draft_camera(world: &mut World) {
    let active = world
        .get_resource::<DraftingWorkspaceState>()
        .is_some_and(DraftingWorkspaceState::is_active);
    if !active {
        return;
    }
    let Some(snapshot) = super::draft::active_draft_snapshot(world) else {
        return;
    };
    let mut query = world.query::<(&mut OrbitCamera, &mut Transform, &mut Projection)>();
    let Some((orbit, transform, projection)) = query.iter_mut(world).next() else {
        return;
    };
    align_camera_to_draft_plane(orbit, transform, projection, &snapshot.node.plane);
}

fn align_camera_to_draft_plane(
    mut orbit: Mut<OrbitCamera>,
    mut transform: Mut<Transform>,
    mut projection: Mut<Projection>,
    plane: &DrawingPlane,
) {
    // Keep Bevy's change ticks semantic. Accepting `&mut T` here would mark
    // all three components changed through `Mut<T>` deref coercion even when
    // the constrained view was already exact, causing DrawingScene to rebuild
    // continuously at idle. Only take a mutable dereference in a branch that
    // actually changes controller or presentation state.
    if orbit.projection_mode != CameraProjectionMode::Isometric {
        orbit.transition_projection_mode(CameraProjectionMode::Isometric);
        apply_orbit_state(&orbit, &mut transform, &mut projection);
    } else if !matches!(*projection, Projection::Orthographic(_)) {
        apply_orbit_state(&orbit, &mut transform, &mut projection);
    }

    let distance = orbit.radius.max(0.001);
    let desired = Transform::from_translation(orbit.focus - plane.normal * distance)
        .looking_at(orbit.focus, plane.bitangent);
    let translation_matches = transform.translation.abs_diff_eq(desired.translation, 1e-5);
    let rotation_matches = transform.rotation.dot(desired.rotation).abs() >= 1.0 - 1e-5;
    if !translation_matches || !rotation_matches {
        *transform = desired;
    }
}

/// React only to a changed Draft selection or changed durable Draft. Face-edit
/// tools remain free to use the shared DrawingPlane between those transitions.
pub(crate) fn sync_active_draft_plane_on_change(
    workspace: Res<DraftingWorkspaceState>,
    drafts: Query<(&ElementId, Ref<super::draft::DraftNode>)>,
    drawing_plane: Option<ResMut<DrawingPlane>>,
) {
    if !workspace.is_active() {
        return;
    }
    let Some(active_id) = workspace.active_draft_id() else {
        return;
    };
    let Some((_, draft)) = drafts.iter().find(|(id, _)| **id == active_id) else {
        return;
    };
    if !workspace.is_changed() && !draft.is_changed() {
        return;
    }
    let Some(mut drawing_plane) = drawing_plane else {
        return;
    };
    if *drawing_plane != draft.plane {
        *drawing_plane = draft.plane.clone();
    }
}

/// Reassert exact Draft-plane camera orientation after the ordinary camera
/// controller has processed input. This prevents orbit gestures or view preset
/// commands from breaking Drafting while preserving the shared pan/zoom path.
pub(crate) fn enforce_active_draft_camera_alignment(
    workspace: Res<DraftingWorkspaceState>,
    drafts: Query<(&ElementId, &super::draft::DraftNode)>,
    mut cameras: Query<(&mut OrbitCamera, &mut Transform, &mut Projection)>,
) {
    if !workspace.is_active() {
        return;
    }
    let Some(active_id) = workspace.active_draft_id() else {
        return;
    };
    let Some((_, draft)) = drafts.iter().find(|(id, _)| **id == active_id) else {
        return;
    };
    let Some((orbit, transform, projection)) = cameras.iter_mut().next() else {
        return;
    };
    align_camera_to_draft_plane(orbit, transform, projection, &draft.plane);
}

/// Reasserts invariants if another presentation control is changed while the
/// workspace is active. Plane orientation and orthographic zoom remain free.
pub(crate) fn enforce_drafting_workspace_invariants(
    state: Res<DraftingWorkspaceState>,
    render: Option<ResMut<RenderSettings>>,
    camera: Option<ResMut<CameraControlsState>>,
    visibility: Option<ResMut<DraftingVisibility>>,
    compass: Option<ResMut<CompassSettings>>,
) {
    if !state.is_active() {
        return;
    }
    if let Some(mut render) = render {
        let mut desired = render.clone();
        apply_drafting_render_preset(&mut desired);
        if *render != desired {
            *render = desired;
        }
    }
    if let Some(mut camera) = camera {
        if camera.projection_mode != CameraProjectionMode::Isometric {
            camera.projection_mode = CameraProjectionMode::Isometric;
        }
    }
    if let Some(mut visibility) = visibility {
        if !visibility.show_all {
            visibility.show_all = true;
        }
    }
    if let Some(mut compass) = compass {
        if compass.enabled {
            compass.enabled = false;
        }
    }
}

fn set_feedback(world: &mut World, message: &str) {
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(message.to_string(), 2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::Projection;

    fn test_app() -> App {
        let mut app = App::new();
        app.init_resource::<DraftingWorkspaceState>()
            .init_resource::<DraftingVisibility>()
            .init_resource::<DrawingSceneLiveCache>()
            .init_resource::<CompassSettings>()
            .init_resource::<DrawingPlane>()
            .insert_resource(CameraControlsState::default())
            .insert_resource(RenderSettings {
                background_rgb: [0.2, 0.3, 0.4],
                grid_enabled: true,
                bloom_enabled: true,
                ..RenderSettings::default()
            });
        let orbit = OrbitCamera::default();
        app.world_mut().spawn((
            orbit,
            Transform::default(),
            Projection::Perspective(PerspectiveProjection::default()),
        ));
        app
    }

    #[test]
    fn toggle_is_reversible_for_render_camera_and_visibility() {
        let mut app = test_app();
        app.world_mut()
            .resource_mut::<DraftingVisibility>()
            .show_all = false;

        let entered =
            execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": true }))
                .expect("enter Drafting");
        assert_eq!(entered.output.unwrap()["active"], true);
        assert!(app.world().resource::<DraftingWorkspaceState>().is_active());
        assert_eq!(
            app.world().resource::<RenderSettings>().background_rgb,
            [1.0, 1.0, 1.0]
        );
        assert_eq!(
            app.world()
                .resource::<CameraControlsState>()
                .projection_mode,
            CameraProjectionMode::Isometric
        );
        assert!(app.world().resource::<DraftingVisibility>().show_all);
        assert!(!app.world().resource::<CompassSettings>().enabled);

        {
            let mut query = app.world_mut().query::<&mut OrbitCamera>();
            let mut orbit = query.single_mut(app.world_mut()).expect("orbit camera");
            orbit.focus = Vec3::new(9.0, 8.0, 7.0);
            orbit.radius = 42.0;
            orbit.yaw = 1.2;
            orbit.pitch = -0.9;
            orbit.projection_mode = CameraProjectionMode::Isometric;
        }

        execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": false }))
            .expect("exit Drafting");
        assert_eq!(
            app.world().resource::<RenderSettings>().background_rgb,
            [0.2, 0.3, 0.4]
        );
        assert_eq!(
            app.world()
                .resource::<CameraControlsState>()
                .projection_mode,
            CameraProjectionMode::Perspective
        );
        assert!(!app.world().resource::<DraftingVisibility>().show_all);
        assert!(app.world().resource::<CompassSettings>().enabled);
        let mut query = app.world_mut().query::<&OrbitCamera>();
        let orbit = query.single(app.world()).expect("restored orbit camera");
        assert_eq!(orbit.focus, Vec3::ZERO);
        assert_eq!(orbit.radius, 15.0);
        assert_eq!(orbit.projection_mode, CameraProjectionMode::Perspective);
    }

    #[test]
    fn explicit_enabled_is_idempotent_and_preserves_original_baseline() {
        let mut app = test_app();
        execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": true }))
            .expect("first enter");
        execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": true }))
            .expect("second enter");
        execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": false }))
            .expect("exit");

        assert_eq!(
            app.world().resource::<RenderSettings>().background_rgb,
            [0.2, 0.3, 0.4]
        );
    }

    #[test]
    fn selected_draft_reuses_and_reversibly_restores_tool_drawing_plane() {
        let mut app = test_app();
        let baseline = DrawingPlane::from_face(Vec3::new(1.0, 0.0, 0.0), Vec3::Y);
        *app.world_mut().resource_mut::<DrawingPlane>() = baseline.clone();

        let mut node = super::super::draft::DraftNode::new("Elevation");
        node.plane = DrawingPlane::try_from_origin_normal_tangent(
            Vec3::new(0.0, 0.0, 4.0),
            Vec3::Z,
            Vec3::X,
        )
        .unwrap();
        let expected = node.plane.clone();
        app.world_mut().spawn((ElementId(7), node));

        execute_toggle_drafting_workspace(app.world_mut(), &json!({"enabled": true})).unwrap();
        assert_eq!(
            app.world()
                .resource::<DraftingWorkspaceState>()
                .active_draft_id(),
            Some(ElementId(7))
        );
        assert_eq!(*app.world().resource::<DrawingPlane>(), expected);
        let mut camera_query = app.world_mut().query::<&Transform>();
        let camera = camera_query.single(app.world()).expect("Draft camera");
        assert!((camera.rotation * Vec3::X).dot(expected.tangent) > 0.9999);
        assert!((camera.rotation * Vec3::Y).dot(expected.bitangent) > 0.9999);
        assert!((camera.rotation * Vec3::NEG_Z).dot(expected.normal) > 0.9999);

        execute_toggle_drafting_workspace(app.world_mut(), &json!({"enabled": false})).unwrap();
        assert_eq!(*app.world().resource::<DrawingPlane>(), baseline);
    }

    #[test]
    fn exact_draft_camera_alignment_does_not_poison_change_detection_at_idle() {
        let mut app = test_app();
        app.world_mut()
            .spawn((ElementId(7), super::super::draft::DraftNode::new("Plan")));
        execute_toggle_drafting_workspace(app.world_mut(), &json!({"enabled": true})).unwrap();

        app.world_mut().clear_trackers();
        apply_active_draft_camera(app.world_mut());

        let mut query = app
            .world_mut()
            .query::<(Ref<OrbitCamera>, Ref<Transform>, Ref<Projection>)>();
        let (orbit, transform, projection) = query.single(app.world()).expect("Draft camera");
        assert!(!orbit.is_changed());
        assert!(!transform.is_changed());
        assert!(!projection.is_changed());
    }

    #[test]
    fn inspect_reports_the_same_authoritative_state_without_mutation() {
        let mut app = test_app();
        let inactive = execute_inspect_drafting_workspace(app.world_mut(), &Value::Null)
            .expect("inspect inactive")
            .output
            .unwrap();
        assert_eq!(inactive["active"], false);
        assert_eq!(inactive["drawing_scene"]["dirty"], true);

        execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": true }))
            .expect("enter Drafting");
        let active = execute_inspect_drafting_workspace(app.world_mut(), &Value::Null)
            .expect("inspect active")
            .output
            .unwrap();
        assert_eq!(active["active"], true);
        assert_eq!(active["drawing_scene"]["rebuild_count"], 0);
    }
}
