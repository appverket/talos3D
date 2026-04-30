use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use talos3d_core::{
    authored_entity::AuthoredEntity,
    plugins::{
        camera::focus_orbit_camera_on_bounds, commands::CreateEntityCommand, tools::Preview,
        ui::StatusBarData,
    },
};

use crate::{
    components::{NeedsTerrainMesh, TerrainSurface},
    cut_fill::{CutFillAnalysisPanelState, CutFillAnalysisTarget},
    snapshots::TerrainSurfaceSnapshot,
};

const REVIEW_TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(160, 168, 182);

pub struct TerrainReviewPlugin;

impl Plugin for TerrainReviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainGenerationReviewState>()
            .init_resource::<CutFillAnalysisPanelState>()
            .add_systems(
                Update,
                (
                    sync_terrain_preview_entity,
                    draw_terrain_review_window,
                    draw_cut_fill_analysis_window,
                ),
            );
    }
}

#[derive(Resource, Debug, Default, Clone)]
pub struct TerrainGenerationReviewState {
    pub curve_count: usize,
    pub preview_surface: Option<TerrainSurfaceSnapshot>,
    pub preview_entity: Option<Entity>,
    pub frame_requested: bool,
}

fn sync_terrain_preview_entity(world: &mut World) {
    let (preview_surface, preview_entity) = {
        let review = world.resource::<TerrainGenerationReviewState>();
        (review.preview_surface.clone(), review.preview_entity)
    };

    let Some(snapshot) = preview_surface else {
        cleanup_preview_entity(world, preview_entity);
        world
            .resource_mut::<TerrainGenerationReviewState>()
            .preview_entity = None;
        return;
    };

    let next_entity = if let Some(entity) = preview_entity {
        if world.entities().contains(entity) {
            world.entity_mut(entity).insert((
                snapshot.surface.clone(),
                NeedsTerrainMesh,
                Visibility::Visible,
            ));
            entity
        } else {
            spawn_preview_entity(world, &snapshot.surface)
        }
    } else {
        spawn_preview_entity(world, &snapshot.surface)
    };

    let frame_requested = world
        .resource::<TerrainGenerationReviewState>()
        .frame_requested;
    if frame_requested {
        if let Some(bounds) = snapshot.bounds() {
            let _ = focus_orbit_camera_on_bounds(world, bounds);
        }
        world
            .resource_mut::<TerrainGenerationReviewState>()
            .frame_requested = false;
    }

    world
        .resource_mut::<TerrainGenerationReviewState>()
        .preview_entity = Some(next_entity);
}

fn spawn_preview_entity(world: &mut World, surface: &TerrainSurface) -> Entity {
    world
        .spawn((
            Preview,
            Name::new("terrain-preview"),
            surface.clone(),
            NeedsTerrainMesh,
        ))
        .id()
}

fn draw_terrain_review_window(
    mut contexts: EguiContexts,
    mut review: ResMut<TerrainGenerationReviewState>,
    mut create_entities: ResMut<Messages<CreateEntityCommand>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    let curve_count = review.curve_count;
    let Some(snapshot) = review.preview_surface.as_mut() else {
        return;
    };

    let mut cancel_requested = false;
    let mut commit_requested = false;

    egui::Window::new("Generate Terrain")
        .collapsible(false)
        .resizable(true)
        .default_width(360.0)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 360.0))
        .show(contexts.ctx_mut().unwrap(), |ui| {
            ui.label(format!("{} source curves selected", curve_count));
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut snapshot.surface.name);
            });
            ui.horizontal(|ui| {
                ui.label("Datum");
                ui.add(egui::DragValue::new(&mut snapshot.surface.datum_elevation).speed(0.1));
            });
            ui.horizontal(|ui| {
                ui.label("Contour");
                ui.add(
                    egui::DragValue::new(&mut snapshot.surface.contour_interval)
                        .speed(0.1)
                        .range(0.1..=100.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Max area");
                ui.add(
                    egui::DragValue::new(&mut snapshot.surface.max_triangle_area)
                        .speed(0.5)
                        .range(0.1..=100_000.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Min angle");
                ui.add(
                    egui::DragValue::new(&mut snapshot.surface.minimum_angle)
                        .speed(0.5)
                        .range(0.1..=89.0),
                );
            });
            if let Some((mut min, mut max)) = boundary_bounds(&snapshot.surface.boundary) {
                ui.separator();
                ui.label("Boundary");
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Min X");
                    changed |= ui
                        .add(egui::DragValue::new(&mut min.x).speed(0.5))
                        .changed();
                    ui.label("Min Z");
                    changed |= ui
                        .add(egui::DragValue::new(&mut min.y).speed(0.5))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Max X");
                    changed |= ui
                        .add(egui::DragValue::new(&mut max.x).speed(0.5))
                        .changed();
                    ui.label("Max Z");
                    changed |= ui
                        .add(egui::DragValue::new(&mut max.y).speed(0.5))
                        .changed();
                });
                if changed && min.x < max.x && min.y < max.y {
                    snapshot.surface.boundary = rectangular_boundary(min, max);
                }
            }
            ui.label(
                egui::RichText::new("Preview is clipped to the editable rectangular boundary.")
                    .small()
                    .color(REVIEW_TEXT_MUTED),
            );

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel_requested = true;
                }
                if ui.button("Generate").clicked() {
                    commit_requested = true;
                }
            });
        });

    if cancel_requested {
        review.preview_surface = None;
        status_bar_data.set_feedback("Terrain generation cancelled".to_string(), 2.0);
        return;
    }

    if commit_requested {
        create_entities.write(CreateEntityCommand {
            snapshot: snapshot.clone().into(),
        });
        review.preview_surface = None;
        status_bar_data.set_feedback("Terrain surface queued".to_string(), 2.0);
    }
}

fn draw_cut_fill_analysis_window(
    mut contexts: EguiContexts,
    mut panel: ResMut<CutFillAnalysisPanelState>,
) {
    let Some(summary) = panel.summary.clone() else {
        return;
    };
    if !panel.visible {
        return;
    }

    let mut visible = panel.visible;
    egui::Window::new("Cut/Fill")
        .resizable(false)
        .default_width(300.0)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 180.0))
        .open(&mut visible)
        .show(contexts.ctx_mut().unwrap(), |ui| {
            egui::Grid::new("cut_fill_result_grid")
                .num_columns(2)
                .spacing(egui::vec2(16.0, 6.0))
                .show(ui, |ui| {
                    ui.label("Existing");
                    ui.label(format!("#{}", summary.existing_surface_id.0));
                    ui.end_row();

                    ui.label("Comparison");
                    ui.label(cut_fill_target_label(summary.target));
                    ui.end_row();

                    ui.label("Cut");
                    ui.label(format_volume(summary.result.cut_volume));
                    ui.end_row();

                    ui.label("Fill");
                    ui.label(format_volume(summary.result.fill_volume));
                    ui.end_row();

                    ui.label("Net");
                    ui.label(format_volume(summary.result.net_volume));
                    ui.end_row();

                    ui.label("Samples");
                    ui.label(summary.result.sample_count.to_string());
                    ui.end_row();

                    ui.label("Spacing");
                    ui.label(format!("{:.2} m", summary.sample_spacing));
                    ui.end_row();

                    ui.label("Boundary");
                    ui.label(summary.boundary_vertex_count.to_string());
                    ui.end_row();
                });
        });
    panel.visible = visible;
}

fn cut_fill_target_label(target: CutFillAnalysisTarget) -> String {
    match target {
        CutFillAnalysisTarget::ProposedSurface(element_id) => {
            format!("Surface #{}", element_id.0)
        }
        CutFillAnalysisTarget::Datum(datum_y) => format!("Datum {:.2} m", datum_y),
    }
}

fn format_volume(volume: f64) -> String {
    format!("{volume:.2} m3")
}

fn cleanup_preview_entity(world: &mut World, preview_entity: Option<Entity>) {
    let Some(preview_entity) = preview_entity else {
        return;
    };
    if !world.entities().contains(preview_entity) {
        return;
    }
    let mesh_id = world
        .get_entity(preview_entity)
        .ok()
        .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh| mesh.id()));
    if let Some(mesh_id) = mesh_id {
        world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
    }
    let _ = world.despawn(preview_entity);
}

fn boundary_bounds(boundary: &[Vec2]) -> Option<(Vec2, Vec2)> {
    let mut points = boundary.iter().copied();
    let first = points.next()?;
    let mut min = first;
    let mut max = first;
    for point in points {
        min = min.min(point);
        max = max.max(point);
    }
    Some((min, max))
}

fn rectangular_boundary(min: Vec2, max: Vec2) -> Vec<Vec2> {
    vec![
        Vec2::new(min.x, min.y),
        Vec2::new(max.x, min.y),
        Vec2::new(max.x, max.y),
        Vec2::new(min.x, max.y),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangular_boundary_preserves_editable_bounds() {
        let boundary = rectangular_boundary(Vec2::new(-1.0, -2.0), Vec2::new(3.0, 4.0));
        let (min, max) = boundary_bounds(&boundary).expect("boundary has bounds");

        assert_eq!(min, Vec2::new(-1.0, -2.0));
        assert_eq!(max, Vec2::new(3.0, 4.0));
    }
}
