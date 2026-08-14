use std::path::PathBuf;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

/// Trailing marker of the base window title. Anything after it was appended by
/// another plugin (the model-api instance annotation) rather than by us.
const TITLE_SUFFIX: &str = "\u{2014} Talos3D";

pub struct DocumentStatePlugin;

impl Plugin for DocumentStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DocumentState>()
            .add_systems(Update, sync_window_title);
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct DocumentState {
    pub current_path: Option<PathBuf>,
    pub dirty: bool,
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
            format!("{name} [modified] {TITLE_SUFFIX}")
        } else {
            format!("{name} {TITLE_SUFFIX}")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_title(title: &str, doc: DocumentState) -> App {
        let mut app = App::new();
        app.insert_resource(doc)
            .add_systems(Update, sync_window_title);
        app.world_mut().spawn((
            Window {
                title: title.to_string(),
                ..default()
            },
            PrimaryWindow,
        ));
        app
    }

    /// A document opened from the command line loads during `Startup`, before
    /// the primary window exists. The title must still arrive once it does.
    #[test]
    fn title_is_applied_on_a_later_frame_when_the_window_appears_late() {
        let doc = DocumentState {
            current_path: Some(PathBuf::from("/projects/house.talos3d")),
            dirty: false,
        };
        let mut app = App::new();
        app.insert_resource(doc)
            .add_systems(Update, sync_window_title);

        // First frame: no window yet, nothing to write to.
        app.update();
        app.world_mut().spawn((
            Window {
                title: "Untitled \u{2014} Talos3D".to_string(),
                ..default()
            },
            PrimaryWindow,
        ));
        app.update();

        let mut windows = app.world_mut().query::<&Window>();
        let window = windows.iter(app.world()).next().expect("window");
        assert_eq!(window.title, "house.talos3d \u{2014} Talos3D");
    }

    /// The model-api plugin appends " [instance @ port]"; that annotated title
    /// is current and must not be rewritten every frame.
    #[test]
    fn model_api_annotation_is_left_intact() {
        let doc = DocumentState {
            current_path: Some(PathBuf::from("/projects/house.talos3d")),
            dirty: false,
        };
        let annotated = "house.talos3d \u{2014} Talos3D [ux-audit @ 24855]";
        let mut app = app_with_title(annotated, doc);
        app.update();

        let mut windows = app.world_mut().query::<&Window>();
        let window = windows.iter(app.world()).next().expect("window");
        assert_eq!(window.title, annotated);
    }

    /// The model-api plugin's " [instance @ port]" suffix identifies which app
    /// an agent is driving; a rename must not silently drop it.
    #[test]
    fn instance_annotation_survives_a_document_rename() {
        let doc = DocumentState {
            current_path: Some(PathBuf::from("/projects/house.talos3d")),
            dirty: false,
        };
        let mut app = app_with_title("Untitled \u{2014} Talos3D [ux-audit @ 24855]", doc);
        app.update();

        let mut windows = app.world_mut().query::<&Window>();
        let window = windows.iter(app.world()).next().expect("window");
        assert_eq!(
            window.title,
            "house.talos3d \u{2014} Talos3D [ux-audit @ 24855]"
        );
    }

    #[test]
    fn dirty_document_is_marked_in_the_title() {
        let doc = DocumentState {
            current_path: Some(PathBuf::from("/projects/house.talos3d")),
            dirty: true,
        };
        let mut app = app_with_title("house.talos3d \u{2014} Talos3D", doc);
        app.update();

        let mut windows = app.world_mut().query::<&Window>();
        let window = windows.iter(app.world()).next().expect("window");
        assert_eq!(window.title, "house.talos3d [modified] \u{2014} Talos3D");
    }
}

fn sync_window_title(
    doc_state: Res<DocumentState>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let desired = doc_state.window_title();
    // Converge on the desired title rather than reacting to `DocumentState`
    // changing. A document opened from the command line is loaded during
    // `Startup`, before the primary window exists — reacting to the change once
    // dropped that title on the floor and left the app reading "Untitled" with a
    // document open.
    //
    // The model-api plugin appends " [instance @ port]" to whatever base title
    // this system sets, so an annotated title still counts as current.
    let already_current = window
        .title
        .strip_prefix(&desired)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(" ["));
    if already_current {
        return;
    }
    // Carry that annotation across a rename rather than dropping it, so the
    // instance an agent is driving stays identifiable after the document
    // changes.
    let annotation = window
        .title
        .find(TITLE_SUFFIX)
        .map(|index| &window.title[index + TITLE_SUFFIX.len()..])
        .filter(|rest| rest.starts_with(" ["))
        .unwrap_or("")
        .to_string();
    window.title = format!("{desired}{annotation}");
}
