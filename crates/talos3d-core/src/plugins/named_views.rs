use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::camera::{apply_orbit_state, CameraProjectionMode, OrbitCamera};

/// A saved camera position with a name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedView {
    pub name: String,
    pub description: Option<String>,
    pub focus: [f32; 3],
    pub radius: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub projection_mode: CameraProjectionMode,
    pub focal_length_mm: f32,
}

impl NamedView {
    /// Capture the current orbit camera state as a named view.
    pub fn from_orbit(name: impl Into<String>, orbit: &OrbitCamera) -> Self {
        Self {
            name: name.into(),
            description: None,
            focus: orbit.focus.into(),
            radius: orbit.radius,
            yaw: orbit.yaw,
            pitch: orbit.pitch,
            projection_mode: orbit.projection_mode,
            focal_length_mm: orbit.focal_length_mm,
        }
    }

    /// Build an `OrbitCamera` that matches this view's saved state.
    pub fn to_orbit(&self) -> OrbitCamera {
        OrbitCamera {
            focus: Vec3::from(self.focus),
            radius: self.radius,
            yaw: self.yaw,
            pitch: self.pitch,
            projection_mode: self.projection_mode,
            focal_length_mm: self.focal_length_mm,
        }
    }
}

/// Resource holding all named views in insertion order.
#[derive(Resource, Default, Serialize, Deserialize, Clone, Debug)]
pub struct NamedViewRegistry {
    pub views: Vec<NamedView>,
}

impl NamedViewRegistry {
    /// Look up a view by name.
    pub fn get(&self, name: &str) -> Option<&NamedView> {
        self.views.iter().find(|v| v.name == name)
    }

    /// Look up a view by name, returning a mutable reference.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut NamedView> {
        self.views.iter_mut().find(|v| v.name == name)
    }

    /// Insert a new view. Returns an error if the name already exists.
    pub fn save(&mut self, view: NamedView) -> Result<(), String> {
        if self.views.iter().any(|v| v.name == view.name) {
            return Err(format!("A view named '{}' already exists", view.name));
        }
        self.views.push(view);
        Ok(())
    }

    /// Overwrite an existing view. Returns an error if the name is not found.
    pub fn update(&mut self, view: NamedView) -> Result<(), String> {
        let slot = self
            .views
            .iter_mut()
            .find(|v| v.name == view.name)
            .ok_or_else(|| format!("No view named '{}' exists", view.name))?;
        *slot = view;
        Ok(())
    }

    /// Insert or overwrite a view, matching on name.
    pub fn upsert(&mut self, view: NamedView) {
        if let Some(slot) = self.views.iter_mut().find(|v| v.name == view.name) {
            *slot = view;
        } else {
            self.views.push(view);
        }
    }

    /// Remove a view by name. Returns an error if not found.
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let before = self.views.len();
        self.views.retain(|v| v.name != name);
        if self.views.len() == before {
            Err(format!("No view named '{name}' exists"))
        } else {
            Ok(())
        }
    }

    /// Rename a view. Returns an error if `old_name` is not found or `new_name` is taken.
    pub fn rename(&mut self, old_name: &str, new_name: &str) -> Result<(), String> {
        if self.views.iter().any(|v| v.name == new_name) {
            return Err(format!("A view named '{new_name}' already exists"));
        }
        let slot = self
            .views
            .iter_mut()
            .find(|v| v.name == old_name)
            .ok_or_else(|| format!("No view named '{old_name}' exists"))?;
        slot.name = new_name.to_string();
        Ok(())
    }

    /// Return a slice of all views in order.
    pub fn list(&self) -> &[NamedView] {
        &self.views
    }
}

/// Message: restore the camera to a previously saved named view (instant).
#[derive(Message, Debug, Clone)]
pub struct RestoreNamedView {
    pub name: String,
}

pub struct NamedViewsPlugin;

impl Plugin for NamedViewsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NamedViewRegistry>()
            .add_message::<RestoreNamedView>()
            .add_systems(Startup, seed_default_named_views)
            .add_systems(Update, apply_restore_named_view_events);
    }
}

/// Seed four standard architectural views on startup.
fn seed_default_named_views(mut registry: ResMut<NamedViewRegistry>) {
    // Top — orthographic, looking straight down.
    registry.upsert(NamedView {
        name: "Top".to_string(),
        description: Some("Orthographic top-down view.".to_string()),
        focus: [0.0, 0.0, 0.0],
        radius: 15.0,
        yaw: 0.0,
        pitch: -std::f32::consts::FRAC_PI_2 + 0.01,
        projection_mode: CameraProjectionMode::Isometric,
        focal_length_mm: 50.0,
    });

    // Front — orthographic, looking along +Z toward origin (yaw = 0, pitch = 0).
    registry.upsert(NamedView {
        name: "Front".to_string(),
        description: Some("Orthographic front view (along Z axis).".to_string()),
        focus: [0.0, 0.0, 0.0],
        radius: 15.0,
        yaw: 0.0,
        pitch: 0.0,
        projection_mode: CameraProjectionMode::Isometric,
        focal_length_mm: 50.0,
    });

    // Right — orthographic, looking along -X toward origin (yaw = π/2, pitch = 0).
    registry.upsert(NamedView {
        name: "Right".to_string(),
        description: Some("Orthographic right-side view (along X axis).".to_string()),
        focus: [0.0, 0.0, 0.0],
        radius: 15.0,
        yaw: std::f32::consts::FRAC_PI_2,
        pitch: 0.0,
        projection_mode: CameraProjectionMode::Isometric,
        focal_length_mm: 50.0,
    });

    // Perspective — default three-quarter perspective view.
    registry.upsert(NamedView {
        name: "Perspective".to_string(),
        description: Some("Default perspective view.".to_string()),
        focus: [0.0, 0.0, 0.0],
        radius: 15.0,
        yaw: std::f32::consts::FRAC_PI_4,
        pitch: -std::f32::consts::FRAC_PI_6,
        projection_mode: CameraProjectionMode::Perspective,
        focal_length_mm: 50.0,
    });
}

/// Apply `RestoreNamedView` messages by directly updating the camera (instant, no animation).
fn apply_restore_named_view_events(
    mut events: MessageReader<RestoreNamedView>,
    registry: Res<NamedViewRegistry>,
    mut camera_query: Query<(&mut OrbitCamera, &mut Transform, &mut Projection)>,
) {
    for event in events.read() {
        let Some(view) = registry.get(&event.name) else {
            continue;
        };
        let orbit = view.to_orbit();
        let Ok((mut cam_orbit, mut transform, mut projection)) = camera_query.single_mut() else {
            continue;
        };
        *cam_orbit = orbit;
        apply_orbit_state(&cam_orbit, &mut transform, &mut projection);
    }
}
