use std::time::Duration;

use bevy::prelude::*;

pub struct PerfStatsPlugin;

impl Plugin for PerfStatsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PerfStats>()
            .add_systems(First, begin_perf_frame)
            .add_systems(Update, toggle_perf_overlay_visibility);
    }
}

#[derive(Resource, Debug, Clone)]
pub struct PerfStats {
    pub visible: bool,
    pub fps: f32,
    pub transform_preview_ms: f32,
    pub mesh_regen_count: usize,
    pub gizmo_line_count: usize,
}

impl Default for PerfStats {
    fn default() -> Self {
        Self {
            visible: true,
            fps: 0.0,
            transform_preview_ms: 0.0,
            mesh_regen_count: 0,
            gizmo_line_count: 0,
        }
    }
}

fn begin_perf_frame(time: Res<Time>, mut perf_stats: ResMut<PerfStats>) {
    let delta = time.delta_secs();
    perf_stats.fps = if delta > f32::EPSILON {
        1.0 / delta
    } else {
        0.0
    };
    perf_stats.transform_preview_ms = 0.0;
    perf_stats.mesh_regen_count = 0;
    perf_stats.gizmo_line_count = 0;
}

fn toggle_perf_overlay_visibility(
    keys: Res<ButtonInput<KeyCode>>,
    mut perf_stats: ResMut<PerfStats>,
) {
    if keys.just_pressed(KeyCode::F11) {
        perf_stats.visible = !perf_stats.visible;
    }
}

pub fn add_transform_preview_time(perf_stats: &mut PerfStats, elapsed: Duration) {
    perf_stats.transform_preview_ms += elapsed.as_secs_f32() * 1000.0;
}

pub fn add_mesh_regen_count(perf_stats: &mut PerfStats, count: usize) {
    perf_stats.mesh_regen_count += count;
}

pub fn add_gizmo_line_count(perf_stats: &mut PerfStats, count: usize) {
    perf_stats.gizmo_line_count += count;
}

pub fn overlay_text(perf_stats: &PerfStats) -> String {
    format!(
        "FPS: {:.1}\nTransform: {:.3} ms\nMesh regen: {}\nGizmo lines: {}",
        perf_stats.fps,
        perf_stats.transform_preview_ms,
        perf_stats.mesh_regen_count,
        perf_stats.gizmo_line_count
    )
}
