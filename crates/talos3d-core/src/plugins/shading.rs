use bevy::prelude::*;

pub struct ShadingPlugin;

/// Toggles between PBR shading and flat (unlit) display.
/// Switch with Ctrl + Option + S.
#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShadingMode {
    #[default]
    Shaded,
    Flat,
}

impl Plugin for ShadingPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<ShadingMode>()
            .add_systems(
                Update,
                (toggle_shading_mode, apply_shading_to_new_materials),
            )
            .add_systems(OnEnter(ShadingMode::Flat), set_all_unlit::<true>)
            .add_systems(OnEnter(ShadingMode::Shaded), set_all_unlit::<false>);
    }
}

fn toggle_shading_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<State<ShadingMode>>,
    mut next: ResMut<NextState<ShadingMode>>,
) {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    if ctrl && alt && keys.just_pressed(KeyCode::KeyS) {
        next.set(match mode.get() {
            ShadingMode::Shaded => ShadingMode::Flat,
            ShadingMode::Flat => ShadingMode::Shaded,
        });
    }
}

/// Applies the current mode to any material that was just added (e.g. newly spawned walls).
fn apply_shading_to_new_materials(
    mode: Res<State<ShadingMode>>,
    query: Query<&MeshMaterial3d<StandardMaterial>, Added<MeshMaterial3d<StandardMaterial>>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if matches!(mode.get(), ShadingMode::Flat) {
        for handle in &query {
            if let Some(mat) = materials.get_mut(handle) {
                mat.unlit = true;
            }
        }
    }
}

/// Flips `unlit` on every `StandardMaterial` in the scene.
fn set_all_unlit<const UNLIT: bool>(
    query: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for handle in &query {
        if let Some(mat) = materials.get_mut(handle) {
            mat.unlit = UNLIT;
        }
    }
}
