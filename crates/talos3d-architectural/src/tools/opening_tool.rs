use bevy::{ecs::system::SystemParam, picking::prelude::*, prelude::*, window::PrimaryWindow};
use talos3d_core::plugins::{
    document_properties::DocumentProperties,
    egui_chrome::EguiWantsInput,
    identity::ElementId,
    math::{draw_loop, rectangle_corners, snap_to_increment},
    tools::ActiveTool,
    ui::StatusBarData,
};

use crate::{
    components::{OpeningKind, Wall},
    create_commands::CreateOpeningCommand,
};

const PREVIEW_COLOR: Color = Color::srgb(0.35, 0.9, 1.0);
const PREVIEW_FACE_OFFSET_METRES: f32 = 0.01;
const POSITION_SNAP_INCREMENT_METRES: f32 = 0.1;

const DEFAULT_OPENING_WIDTH: f32 = 1.2;
const DEFAULT_OPENING_HEIGHT: f32 = 1.5;
const DEFAULT_SILL_HEIGHT: f32 = 0.9;

#[derive(Clone, Copy)]
struct OpeningDefaults {
    width: f32,
    height: f32,
    sill_height: f32,
}

fn opening_defaults(doc_props: &DocumentProperties) -> OpeningDefaults {
    if let Some(arch) = doc_props.domain_defaults.get("architectural") {
        OpeningDefaults {
            width: arch
                .get("opening_width")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or(DEFAULT_OPENING_WIDTH),
            height: arch
                .get("opening_height")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or(DEFAULT_OPENING_HEIGHT),
            sill_height: arch
                .get("sill_height")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or(DEFAULT_SILL_HEIGHT),
        }
    } else {
        OpeningDefaults {
            width: DEFAULT_OPENING_WIDTH,
            height: DEFAULT_OPENING_HEIGHT,
            sill_height: DEFAULT_SILL_HEIGHT,
        }
    }
}

pub struct OpeningToolPlugin;

impl Plugin for OpeningToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                cancel_opening_tool,
                commit_opening_on_click,
                draw_opening_preview,
            )
                .run_if(in_state(ActiveTool::PlaceOpening)),
        );
    }
}

#[derive(SystemParam)]
struct OpeningHitTest<'w, 's> {
    window_query: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    camera_query: Query<'w, 's, (&'static Camera, &'static GlobalTransform)>,
    ray_cast: MeshRayCast<'w, 's>,
    walls: Query<'w, 's, (&'static ElementId, &'static Wall)>,
}

#[derive(Debug, Clone)]
struct OpeningPreview {
    wall_element_id: ElementId,
    wall: Wall,
    position_along_wall: f32,
}

fn cancel_opening_tool(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn commit_opening_on_click(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut hit_test: OpeningHitTest,
    mut create_opening_commands: MessageWriter<CreateOpeningCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    doc_props: Res<DocumentProperties>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let defaults = opening_defaults(&doc_props);
    let Some(preview) = hit_test.preview(defaults) else {
        return;
    };

    create_opening_commands.write(CreateOpeningCommand {
        parent_wall_element_id: preview.wall_element_id,
        width: defaults.width,
        height: defaults.height,
        sill_height: defaults.sill_height,
        kind: OpeningKind::Window,
        position_along_wall: preview.position_along_wall,
    });

    status_bar_data.hint = "Hover a wall and click to place opening".to_string();
}

fn draw_opening_preview(
    mut hit_test: OpeningHitTest,
    mut gizmos: Gizmos,
    doc_props: Res<DocumentProperties>,
) {
    let defaults = opening_defaults(&doc_props);
    let Some(preview) = hit_test.preview(defaults) else {
        return;
    };

    let Some(corners) =
        opening_preview_corners(&preview.wall, preview.position_along_wall, defaults)
    else {
        return;
    };

    draw_loop(&mut gizmos, corners.front, PREVIEW_COLOR);
    draw_loop(&mut gizmos, corners.back, PREVIEW_COLOR);
    for index in 0..corners.front.len() {
        gizmos.line(corners.front[index], corners.back[index], PREVIEW_COLOR);
    }
}

impl OpeningHitTest<'_, '_> {
    fn preview(&mut self, defaults: OpeningDefaults) -> Option<OpeningPreview> {
        let cursor_position = self.cursor_position()?;
        let (camera, camera_transform) = self.camera_query.iter().next()?;
        let viewport_cursor = match camera.logical_viewport_rect() {
            Some(rect) => cursor_position - rect.min,
            None => cursor_position,
        };
        let ray = camera
            .viewport_to_world(camera_transform, viewport_cursor)
            .ok()?;
        let hit = self
            .ray_cast
            .cast_ray(
                ray,
                &MeshRayCastSettings::default().with_filter(&|entity| self.walls.contains(entity)),
            )
            .first()?;
        let (wall_element_id, wall) = self.walls.get(hit.0).ok()?;
        let length = wall.length();
        let min_center_distance = defaults.width * 0.5;
        let max_center_distance = length - min_center_distance;

        if length < defaults.width
            || wall.height < defaults.sill_height + defaults.height
            || max_center_distance < min_center_distance
        {
            return None;
        }

        let direction = wall.direction()?;
        let projected_distance = (hit.1.point.xz() - wall.start).dot(direction);
        let snapped_distance =
            snap_to_increment(projected_distance, POSITION_SNAP_INCREMENT_METRES)
                .clamp(min_center_distance, max_center_distance);

        Some(OpeningPreview {
            wall_element_id: *wall_element_id,
            wall: wall.clone(),
            position_along_wall: snapped_distance / length,
        })
    }

    fn cursor_position(&self) -> Option<Vec2> {
        let window = self.window_query.single().ok()?;
        window.cursor_position()
    }
}

struct PreviewCorners {
    front: [Vec3; 4],
    back: [Vec3; 4],
}

fn opening_preview_corners(
    wall: &Wall,
    position_along_wall: f32,
    defaults: OpeningDefaults,
) -> Option<PreviewCorners> {
    let direction = wall.direction()?;
    let normal = Vec3::new(-direction.y, 0.0, direction.x);
    let center_distance = position_along_wall.clamp(0.0, 1.0) * wall.length();
    let center = wall.start + direction * center_distance;
    let opening_center = Vec3::new(
        center.x,
        defaults.sill_height + defaults.height * 0.5,
        center.y,
    );
    let horizontal_offset = Vec3::new(direction.x, 0.0, direction.y) * (defaults.width * 0.5);
    let vertical_offset = Vec3::Y * (defaults.height * 0.5);
    let face_offset = normal * (wall.thickness * 0.5 + PREVIEW_FACE_OFFSET_METRES);

    Some(PreviewCorners {
        front: rectangle_corners(
            opening_center + face_offset,
            horizontal_offset,
            vertical_offset,
        ),
        back: rectangle_corners(
            opening_center - face_offset,
            horizontal_offset,
            vertical_offset,
        ),
    })
}
