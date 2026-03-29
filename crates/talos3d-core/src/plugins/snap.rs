use bevy::prelude::*;

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::{capability_registry::CapabilityRegistry, plugins::cursor::CursorWorldPos};

const ELEMENT_SNAP_RADIUS_METRES: f32 = 0.35;
const SNAP_INDICATOR_COLOR: Color = Color::srgb(0.4, 1.0, 0.75);
const SNAP_INDICATOR_RADIUS: f32 = 0.08;

pub struct SnapPlugin;

impl Plugin for SnapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SnapResult>()
            .add_systems(Update, (update_snap_result, draw_snap_indicator));
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct SnapResult {
    pub raw_position: Option<Vec3>,
    pub position: Option<Vec3>,
    pub target: Option<Vec3>,
    pub kind: SnapKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapKind {
    #[default]
    None,
    Grid,
    Endpoint,
    Midpoint,
}

fn update_snap_result(world: &mut World) {
    let raw_position = world.resource::<CursorWorldPos>().raw;
    let Some(raw_position) = raw_position else {
        *world.resource_mut::<SnapResult>() = SnapResult::default();
        return;
    };

    let keys = world.resource::<ButtonInput<KeyCode>>();
    let snapping_disabled = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let cursor_world_pos = world.resource::<CursorWorldPos>();
    let mut result = SnapResult {
        raw_position: Some(raw_position),
        position: if snapping_disabled {
            Some(raw_position)
        } else {
            cursor_world_pos.snapped
        },
        target: None,
        kind: if snapping_disabled {
            SnapKind::None
        } else {
            SnapKind::Grid
        },
    };

    if snapping_disabled {
        *world.resource_mut::<SnapResult>() = result;
        return;
    }

    let mut best_candidate: Option<(f32, Vec3, SnapKind)> = None;
    let mut snap_points = Vec::new();
    let registry = world.resource::<CapabilityRegistry>();
    for factory in registry.factories() {
        factory.collect_snap_points(world, &mut snap_points);
    }

    for snap_point in snap_points {
        let distance_squared = raw_position.distance_squared(snap_point.position);
        if distance_squared > ELEMENT_SNAP_RADIUS_METRES * ELEMENT_SNAP_RADIUS_METRES {
            continue;
        }

        let replace_candidate = best_candidate
            .map(|(best_distance, _, _)| distance_squared < best_distance)
            .unwrap_or(true);
        if replace_candidate {
            best_candidate = Some((distance_squared, snap_point.position, snap_point.kind));
        }
    }

    if let Some((_, target, kind)) = best_candidate {
        result.position = Some(target);
        result.target = Some(target);
        result.kind = kind;
    }

    *world.resource_mut::<SnapResult>() = result;
}

fn draw_snap_indicator(
    snap_result: Res<SnapResult>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    let (Some(raw_position), Some(target)) = (snap_result.raw_position, snap_result.target) else {
        return;
    };

    gizmos.line(raw_position, target, SNAP_INDICATOR_COLOR);
    gizmos
        .sphere(
            Isometry3d::from_translation(target),
            SNAP_INDICATOR_RADIUS,
            SNAP_INDICATOR_COLOR,
        )
        .resolution(10);
    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, 1);
}
