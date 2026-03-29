use bevy::{ecs::system::SystemParam, prelude::*};
use talos3d_core::plugins::{
    cursor::CursorWorldPos,
    document_properties::DocumentProperties,
    egui_chrome::EguiWantsInput,
    tools::{ActiveTool, Preview},
    ui::StatusBarData,
};

use crate::{
    components::Wall,
    create_commands::CreateWallCommand,
    mesh_generation::{wall_mesh, wall_transform},
};

const PREVIEW_COLOR: Color = Color::srgba(0.4, 0.6, 1.0, 0.4);
const START_MARKER_COLOR: Color = Color::srgb(1.0, 0.95, 0.4);
const START_MARKER_RADIUS: f32 = 0.08;
const MIN_WALL_LENGTH_METRES: f32 = 0.1;

const DEFAULT_WALL_HEIGHT: f32 = 3.0;
const DEFAULT_WALL_THICKNESS: f32 = 0.2;

fn wall_defaults(doc_props: &DocumentProperties) -> (f32, f32) {
    if let Some(arch) = doc_props.domain_defaults.get("architectural") {
        let height = arch
            .get("wall_height")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(DEFAULT_WALL_HEIGHT);
        let thickness = arch
            .get("wall_thickness")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(DEFAULT_WALL_THICKNESS);
        (height, thickness)
    } else {
        (DEFAULT_WALL_HEIGHT, DEFAULT_WALL_THICKNESS)
    }
}

pub struct WallToolPlugin;

#[derive(SystemParam)]
struct PreviewCleanupAssets<'w, 's> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    preview_query:
        Query<'w, 's, (&'static Mesh3d, &'static MeshMaterial3d<StandardMaterial>), With<Preview>>,
}

#[derive(SystemParam)]
struct PreviewMeshAssets<'w, 's> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    preview_query: Query<
        'w,
        's,
        (
            &'static Mesh3d,
            &'static MeshMaterial3d<StandardMaterial>,
            &'static mut Transform,
        ),
        With<Preview>,
    >,
}

impl Plugin for WallToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveTool::PlaceWall), initialize_wall_tool)
            .add_systems(OnExit(ActiveTool::PlaceWall), cleanup_wall_tool)
            .add_systems(
                Update,
                (
                    wall_tool_cancel,
                    wall_tool_click,
                    wall_tool_preview,
                    draw_wall_start_marker,
                )
                    .run_if(in_state(ActiveTool::PlaceWall)),
            );
    }
}

#[derive(Resource, Default)]
pub struct WallToolState {
    pub phase: WallToolPhase,
    pub start: Option<Vec3>,
    pub preview_entity: Option<Entity>,
}

#[derive(Default, Debug, Clone, PartialEq)]
pub enum WallToolPhase {
    #[default]
    WaitingForStart,
    WaitingForEnd,
}

fn initialize_wall_tool(mut commands: Commands) {
    commands.insert_resource(WallToolState::default());
}

fn cleanup_wall_tool(
    mut commands: Commands,
    mut preview_assets: PreviewCleanupAssets,
    wall_tool_state: Option<Res<WallToolState>>,
) {
    if let Some(wall_tool_state) = wall_tool_state {
        cleanup_preview_entity(
            &mut commands,
            &mut preview_assets,
            wall_tool_state.preview_entity,
        );
    }

    commands.remove_resource::<WallToolState>();
}

fn wall_tool_cancel(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut wall_tool_state: ResMut<WallToolState>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut commands: Commands,
    mut preview_assets: PreviewCleanupAssets,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    wall_tool_state.phase = WallToolPhase::WaitingForStart;
    wall_tool_state.start = None;
    cleanup_preview_entity(
        &mut commands,
        &mut preview_assets,
        wall_tool_state.preview_entity.take(),
    );
    next_active_tool.set(ActiveTool::Select);
}

#[allow(clippy::too_many_arguments)]
fn wall_tool_click(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut wall_tool_state: ResMut<WallToolState>,
    mut create_wall_commands: MessageWriter<CreateWallCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut commands: Commands,
    mut preview_assets: PreviewCleanupAssets,
    doc_props: Res<DocumentProperties>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match wall_tool_state.phase {
        WallToolPhase::WaitingForStart => {
            wall_tool_state.phase = WallToolPhase::WaitingForEnd;
            wall_tool_state.start = Some(cursor_position);
            status_bar_data.hint.clear();
            status_bar_data
                .hint
                .push_str("Click to place end point · Esc to cancel");
        }
        WallToolPhase::WaitingForEnd => {
            let Some(start_position) = wall_tool_state.start else {
                return;
            };
            let length = start_position.xz().distance(cursor_position.xz());

            if length < MIN_WALL_LENGTH_METRES {
                status_bar_data.set_feedback(
                    format!("Wall must be at least {MIN_WALL_LENGTH_METRES:.1}m long"),
                    2.0,
                );
                return;
            }

            let (wall_height, wall_thickness) = wall_defaults(&doc_props);
            create_wall_commands.write(CreateWallCommand {
                start: Vec2::new(start_position.x, start_position.z),
                end: Vec2::new(cursor_position.x, cursor_position.z),
                height: wall_height,
                thickness: wall_thickness,
            });

            wall_tool_state.phase = WallToolPhase::WaitingForStart;
            wall_tool_state.start = None;
            cleanup_preview_entity(
                &mut commands,
                &mut preview_assets,
                wall_tool_state.preview_entity.take(),
            );
            status_bar_data.hint.clear();
            status_bar_data.hint.push_str("Click to place start point");
        }
    }
}

fn wall_tool_preview(
    mut commands: Commands,
    cursor_world_pos: Res<CursorWorldPos>,
    mut wall_tool_state: ResMut<WallToolState>,
    mut preview_assets: PreviewMeshAssets,
    doc_props: Res<DocumentProperties>,
) {
    if wall_tool_state.phase != WallToolPhase::WaitingForEnd {
        return;
    }

    let (Some(start_position), Some(cursor_position)) =
        (wall_tool_state.start, cursor_world_pos.snapped)
    else {
        return;
    };

    let (wall_height, wall_thickness) = wall_defaults(&doc_props);
    let preview_wall = Wall {
        start: Vec2::new(start_position.x, start_position.z),
        end: Vec2::new(cursor_position.x, cursor_position.z),
        height: wall_height,
        thickness: wall_thickness,
    };

    if preview_wall.start.distance(preview_wall.end) < MIN_WALL_LENGTH_METRES {
        if let Some(preview_entity) = wall_tool_state.preview_entity.take() {
            if let Ok((mesh_handle, material_handle, _)) =
                preview_assets.preview_query.get_mut(preview_entity)
            {
                preview_assets.meshes.remove(mesh_handle.id());
                preview_assets.materials.remove(material_handle.id());
                commands.entity(preview_entity).despawn();
            }
        }
        return;
    }

    if let Some(preview_entity) = wall_tool_state.preview_entity {
        if let Ok((mesh_handle, _, mut transform)) =
            preview_assets.preview_query.get_mut(preview_entity)
        {
            if let Some(mesh) = preview_assets.meshes.get_mut(mesh_handle.id()) {
                *mesh = wall_mesh(&preview_wall);
            }
            *transform = wall_transform(&preview_wall);
            return;
        }
    }

    let preview_entity = commands
        .spawn((
            Preview,
            Mesh3d(preview_assets.meshes.add(wall_mesh(&preview_wall))),
            MeshMaterial3d(preview_assets.materials.add(StandardMaterial {
                base_color: PREVIEW_COLOR,
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                ..default()
            })),
            wall_transform(&preview_wall),
        ))
        .id();

    wall_tool_state.preview_entity = Some(preview_entity);
}

fn draw_wall_start_marker(wall_tool_state: Res<WallToolState>, mut gizmos: Gizmos) {
    let Some(start_position) = wall_tool_state.start else {
        return;
    };

    gizmos
        .sphere(
            Isometry3d::from_translation(start_position),
            START_MARKER_RADIUS,
            START_MARKER_COLOR,
        )
        .resolution(12);
}

fn cleanup_preview_entity(
    commands: &mut Commands,
    preview_assets: &mut PreviewCleanupAssets,
    preview_entity: Option<Entity>,
) {
    let Some(preview_entity) = preview_entity else {
        return;
    };

    if let Ok((mesh_handle, material_handle)) = preview_assets.preview_query.get(preview_entity) {
        preview_assets.meshes.remove(mesh_handle.id());
        preview_assets.materials.remove(material_handle.id());
        commands.entity(preview_entity).despawn();
    }
}
