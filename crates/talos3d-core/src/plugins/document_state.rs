use std::path::PathBuf;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct DocumentStatePlugin;

impl Plugin for DocumentStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DocumentState>()
            .add_systems(Update, sync_window_title);
    }
}

#[derive(Resource, Debug, Clone)]
pub struct DocumentState {
    pub current_path: Option<PathBuf>,
    pub dirty: bool,
}

impl Default for DocumentState {
    fn default() -> Self {
        Self {
            current_path: None,
            dirty: false,
        }
    }
}

impl DocumentState {
    pub fn display_name(&self) -> String {
        match &self.current_path {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".to_string()),
            None => "Untitled".to_string(),
        }
    }

    pub fn window_title(&self) -> String {
        let name = self.display_name();
        if self.dirty {
            format!("{name} [modified] \u{2014} Talos3D")
        } else {
            format!("{name} \u{2014} Talos3D")
        }
    }

    pub fn mark_saved(&mut self, path: PathBuf) {
        self.current_path = Some(path);
        self.dirty = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn reset(&mut self) {
        self.current_path = None;
        self.dirty = false;
    }
}

fn sync_window_title(
    doc_state: Res<DocumentState>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !doc_state.is_changed() {
        return;
    }
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    window.title = doc_state.window_title();
}
