use bevy::prelude::*;

use super::document_properties::DocumentProperties;
#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};

const GRID_EXTENT: f32 = 50.0;
const GRID_MINOR_COLOR: Color = Color::srgba(0.5, 0.5, 0.5, 0.3);
const GRID_MAJOR_COLOR: Color = Color::srgba(0.7, 0.7, 0.7, 0.5);
const GRID_X_AXIS_COLOR: Color = Color::srgb(1.0, 0.2, 0.2);
const GRID_Z_AXIS_COLOR: Color = Color::srgb(0.2, 0.4, 1.0);

pub struct GridPlugin;

impl Plugin for GridPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, draw_grid);
    }
}

fn draw_grid(
    mut gizmos: Gizmos,
    doc_props: Res<DocumentProperties>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    let minor_spacing = doc_props.grid_minor_spacing;
    let major_spacing = doc_props.grid_major_spacing;
    #[cfg(feature = "perf-stats")]
    let mut line_count = 0usize;

    let mut offset = -GRID_EXTENT;
    while offset <= GRID_EXTENT {
        if offset.abs() < f32::EPSILON {
            offset += minor_spacing;
            continue;
        }

        let color = if is_major_line(offset, major_spacing) {
            GRID_MAJOR_COLOR
        } else {
            GRID_MINOR_COLOR
        };

        gizmos.line(
            Vec3::new(offset, 0.0, -GRID_EXTENT),
            Vec3::new(offset, 0.0, GRID_EXTENT),
            color,
        );
        gizmos.line(
            Vec3::new(-GRID_EXTENT, 0.0, offset),
            Vec3::new(GRID_EXTENT, 0.0, offset),
            color,
        );
        #[cfg(feature = "perf-stats")]
        {
            line_count += 2;
        }

        offset += minor_spacing;
    }

    gizmos.line(
        Vec3::new(-GRID_EXTENT, 0.0, 0.0),
        Vec3::new(GRID_EXTENT, 0.0, 0.0),
        GRID_X_AXIS_COLOR,
    );
    gizmos.line(
        Vec3::new(0.0, 0.0, -GRID_EXTENT),
        Vec3::new(0.0, 0.0, GRID_EXTENT),
        GRID_Z_AXIS_COLOR,
    );
    #[cfg(feature = "perf-stats")]
    {
        line_count += 2;
        add_gizmo_line_count(&mut perf_stats, line_count);
    }
}

fn is_major_line(offset: f32, major_spacing: f32) -> bool {
    let major_step = (offset / major_spacing).round();
    (offset - major_step * major_spacing).abs() < f32::EPSILON
}
