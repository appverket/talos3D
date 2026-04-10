use bevy::{ecs::world::EntityRef, prelude::*};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::{
    authored_entity::{EntityBounds, HandleKind},
    capability_registry::{CapabilityRegistry, SnapPoint},
    plugins::{cursor::CursorWorldPos, selection::Selected, transform::TransformState},
};

const ELEMENT_SNAP_RADIUS_METRES: f32 = 0.35;
const SNAP_TRAIL_COLOR: Color = Color::srgb(0.4, 1.0, 0.75);
const SNAP_ENDPOINT_COLOR: Color = Color::srgb(0.38, 0.92, 1.0);
const SNAP_MIDPOINT_COLOR: Color = Color::srgb(0.55, 1.0, 0.72);
const SNAP_CONTROL_COLOR: Color = Color::srgb(1.0, 0.82, 0.28);
const SNAP_GUIDE_ANCHOR_COLOR: Color = Color::srgb(0.0, 0.86, 0.86);
const SNAP_GRID_COLOR: Color = Color::srgb(0.4, 1.0, 0.75);
const SNAP_INDICATOR_RADIUS: f32 = 0.08;
const SNAP_HALO_RADIUS: f32 = 0.11;

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
    Control,
    GuideAnchor,
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
    collect_authored_handle_snap_points(world, &mut snap_points);

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

fn collect_authored_handle_snap_points(world: &World, out: &mut Vec<SnapPoint>) {
    let transform_active = !world.resource::<TransformState>().is_idle();
    let registry = world.resource::<CapabilityRegistry>();
    let mut query = world
        .try_query::<EntityRef>()
        .expect("EntityRef query should always be constructible");

    for entity_ref in query.iter(world) {
        if !entity_ref.contains::<crate::plugins::identity::ElementId>() {
            continue;
        }
        if transform_active && entity_ref.contains::<Selected>() {
            continue;
        }
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        for handle in snapshot.handles() {
            let Some(kind) = authored_handle_snap_kind(handle.kind) else {
                continue;
            };
            out.push(SnapPoint {
                position: handle.position,
                kind,
            });
        }
        if let Some(bounds) = snapshot.bounds() {
            collect_bounds_corner_snap_points(bounds, out);
        }
    }
}

fn collect_bounds_corner_snap_points(bounds: EntityBounds, out: &mut Vec<SnapPoint>) {
    for position in bounds.corners() {
        out.push(SnapPoint {
            position,
            kind: SnapKind::Endpoint,
        });
    }
}

fn authored_handle_snap_kind(kind: HandleKind) -> Option<SnapKind> {
    match kind {
        HandleKind::Vertex => Some(SnapKind::Endpoint),
        HandleKind::Center => Some(SnapKind::Midpoint),
        HandleKind::Control | HandleKind::Parameter => Some(SnapKind::Control),
    }
}

fn draw_snap_indicator(
    snap_result: Res<SnapResult>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    let (Some(raw_position), Some(target)) = (snap_result.raw_position, snap_result.target) else {
        return;
    };

    if raw_position.distance_squared(target) > f32::EPSILON {
        gizmos.line(raw_position, target, SNAP_TRAIL_COLOR);
    }
    draw_snap_target(&mut gizmos, target, snap_result.kind);
    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, snap_indicator_line_count(snap_result.kind));
}

fn draw_snap_target(gizmos: &mut Gizmos, target: Vec3, kind: SnapKind) {
    match kind {
        SnapKind::Endpoint => {
            draw_cube_marker(gizmos, target, SNAP_INDICATOR_RADIUS, SNAP_ENDPOINT_COLOR);
            draw_diamond_marker(gizmos, target, SNAP_HALO_RADIUS, SNAP_ENDPOINT_COLOR);
        }
        SnapKind::Midpoint => {
            draw_sphere_marker(gizmos, target, SNAP_INDICATOR_RADIUS, SNAP_MIDPOINT_COLOR);
            draw_diamond_marker(gizmos, target, SNAP_HALO_RADIUS, SNAP_MIDPOINT_COLOR);
        }
        SnapKind::Control => {
            draw_diamond_marker(gizmos, target, SNAP_HALO_RADIUS, SNAP_CONTROL_COLOR);
            draw_cross_marker(gizmos, target, SNAP_INDICATOR_RADIUS, SNAP_CONTROL_COLOR);
        }
        SnapKind::GuideAnchor => {
            draw_diamond_marker(gizmos, target, SNAP_HALO_RADIUS, SNAP_GUIDE_ANCHOR_COLOR);
            draw_cross_marker(
                gizmos,
                target,
                SNAP_INDICATOR_RADIUS,
                SNAP_GUIDE_ANCHOR_COLOR,
            );
        }
        SnapKind::Grid | SnapKind::None => {
            draw_sphere_marker(gizmos, target, SNAP_INDICATOR_RADIUS, SNAP_GRID_COLOR);
        }
    }
}

#[cfg(feature = "perf-stats")]
fn snap_indicator_line_count(kind: SnapKind) -> usize {
    match kind {
        SnapKind::Endpoint => 5,
        SnapKind::Midpoint => 5,
        SnapKind::Control => 8,
        SnapKind::GuideAnchor => 7,
        SnapKind::Grid | SnapKind::None => 1,
    }
}

fn draw_cube_marker(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    gizmos.cube(
        Transform::from_translation(center).with_scale(Vec3::splat(radius * 2.0)),
        color,
    );
}

fn draw_sphere_marker(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    gizmos
        .sphere(Isometry3d::from_translation(center), radius, color)
        .resolution(10);
}

fn draw_diamond_marker(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    let half = radius;
    let corners = [
        center + Vec3::new(0.0, 0.0, -half),
        center + Vec3::new(-half, 0.0, 0.0),
        center + Vec3::new(0.0, 0.0, half),
        center + Vec3::new(half, 0.0, 0.0),
    ];
    draw_loop(gizmos, corners, color);
}

fn draw_cross_marker(gizmos: &mut Gizmos, center: Vec3, radius: f32, color: Color) {
    gizmos.line(
        center + Vec3::new(-radius, 0.0, 0.0),
        center + Vec3::new(radius, 0.0, 0.0),
        color,
    );
    gizmos.line(
        center + Vec3::new(0.0, -radius, 0.0),
        center + Vec3::new(0.0, radius, 0.0),
        color,
    );
    gizmos.line(
        center + Vec3::new(0.0, 0.0, -radius),
        center + Vec3::new(0.0, 0.0, radius),
        color,
    );
}

fn draw_loop(gizmos: &mut Gizmos, corners: [Vec3; 4], color: Color) {
    for index in 0..corners.len() {
        let next = (index + 1) % corners.len();
        gizmos.line(corners[index], corners[next], color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_handle_kind_maps_to_expected_snap_kind() {
        assert_eq!(
            authored_handle_snap_kind(HandleKind::Vertex),
            Some(SnapKind::Endpoint)
        );
        assert_eq!(
            authored_handle_snap_kind(HandleKind::Center),
            Some(SnapKind::Midpoint)
        );
        assert_eq!(
            authored_handle_snap_kind(HandleKind::Control),
            Some(SnapKind::Control)
        );
        assert_eq!(
            authored_handle_snap_kind(HandleKind::Parameter),
            Some(SnapKind::Control)
        );
    }

    #[test]
    fn bounds_corners_are_exposed_as_endpoint_snap_points() {
        let bounds = EntityBounds {
            min: Vec3::new(-1.0, 0.0, -2.0),
            max: Vec3::new(3.0, 4.0, 5.0),
        };
        let mut snap_points = Vec::new();

        collect_bounds_corner_snap_points(bounds, &mut snap_points);

        assert_eq!(snap_points.len(), 8);
        assert!(snap_points
            .iter()
            .all(|snap_point| snap_point.kind == SnapKind::Endpoint));
        assert!(snap_points
            .iter()
            .any(|snap_point| snap_point.position == Vec3::new(-1.0, 0.0, -2.0)));
        assert!(snap_points
            .iter()
            .any(|snap_point| snap_point.position == Vec3::new(3.0, 4.0, 5.0)));
    }
}
