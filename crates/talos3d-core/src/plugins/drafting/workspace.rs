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
    render_pipeline::{apply_drafting_render_preset, RenderSettings},
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
}

/// The single authority for whether the main viewport is in Drafting.
#[derive(Resource, Debug, Default)]
pub struct DraftingWorkspaceState {
    baseline: Option<DraftingWorkspaceBaseline>,
}

impl DraftingWorkspaceState {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.baseline.is_some()
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
        return Ok(workspace_result(active));
    }

    if requested {
        enter_drafting(world)?;
        set_feedback(world, "Drafting enabled");
    } else {
        exit_drafting(world)?;
        set_feedback(world, "Drafting disabled");
    }

    Ok(workspace_result(requested))
}

/// Read-only command handler for agent/UI state inspection.
pub(crate) fn execute_inspect_drafting_workspace(
    world: &mut World,
    _params: &Value,
) -> Result<CommandResult, String> {
    let state = world
        .get_resource::<DraftingWorkspaceState>()
        .ok_or_else(|| "Drafting workspace state is unavailable".to_string())?;
    Ok(workspace_result(state.is_active()))
}

fn workspace_result(active: bool) -> CommandResult {
    CommandResult {
        output: Some(json!({
            "active": active,
            "projection": if active { "orthographic" } else { "restored" },
            "surface": if active { "black_on_white" } else { "restored" }
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
    });

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
}

/// Reasserts invariants if another presentation control is changed while the
/// workspace is active. Plane orientation and orthographic zoom remain free.
pub(crate) fn enforce_drafting_workspace_invariants(
    state: Res<DraftingWorkspaceState>,
    render: Option<ResMut<RenderSettings>>,
    camera: Option<ResMut<CameraControlsState>>,
    visibility: Option<ResMut<DraftingVisibility>>,
) {
    if !state.is_active() {
        return;
    }
    if let Some(mut render) = render {
        apply_drafting_render_preset(&mut render);
    }
    if let Some(mut camera) = camera {
        camera.projection_mode = CameraProjectionMode::Isometric;
    }
    if let Some(mut visibility) = visibility {
        visibility.show_all = true;
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
    fn inspect_reports_the_same_authoritative_state_without_mutation() {
        let mut app = test_app();
        assert_eq!(
            execute_inspect_drafting_workspace(app.world_mut(), &Value::Null)
                .expect("inspect inactive")
                .output
                .unwrap()["active"],
            false
        );

        execute_toggle_drafting_workspace(app.world_mut(), &json!({ "enabled": true }))
            .expect("enter Drafting");
        assert_eq!(
            execute_inspect_drafting_workspace(app.world_mut(), &Value::Null)
                .expect("inspect active")
                .output
                .unwrap()["active"],
            true
        );
    }
}
