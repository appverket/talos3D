use bevy::{ecs::world::EntityRef, prelude::*};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::{
    authored_entity::{EntityBounds, HandleKind},
    capability_registry::{CapabilityRegistry, SnapPoint},
    plugins::{
        cursor::{CursorSystems, CursorWorldPos},
        selection::Selected,
        transform::TransformState,
    },
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

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SnapSystems {
    Resolve,
    Draw,
}

impl Plugin for SnapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SnapResult>()
            .init_resource::<SnapDisambiguationState>()
            .configure_sets(
                Update,
                (
                    SnapSystems::Resolve.after(CursorSystems::UpdateWorldPosition),
                    SnapSystems::Draw,
                )
                    .chain(),
            )
            .add_systems(Update, update_snap_result.in_set(SnapSystems::Resolve))
            .add_systems(Update, draw_snap_indicator.in_set(SnapSystems::Draw));
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct SnapResult {
    pub raw_position: Option<Vec3>,
    pub position: Option<Vec3>,
    pub target: Option<Vec3>,
    pub kind: SnapKind,
    pub candidates: Vec<SnapCandidate>,
    pub active_candidate: usize,
}

#[derive(Debug, Clone)]
pub struct SnapCandidate {
    pub position: Vec3,
    pub kind: SnapKind,
    pub element_id: Option<crate::plugins::identity::ElementId>,
    pub label: Option<String>,
}

#[derive(Resource, Debug, Clone, Default)]
struct SnapDisambiguationState {
    anchor: Option<Vec3>,
    index: usize,
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
    let cycle_requested = keys.just_pressed(KeyCode::Tab);
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
        candidates: Vec::new(),
        active_candidate: 0,
    };

    if snapping_disabled {
        *world.resource_mut::<SnapResult>() = result;
        return;
    }

    let mut candidates: Vec<(f32, SnapCandidate)> = Vec::new();
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
        candidates.push((
            distance_squared,
            SnapCandidate {
                position: snap_point.position,
                kind: snap_point.kind,
                element_id: snap_point.element_id,
                label: snap_point.label,
            },
        ));
    }

    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));

    if let Some((best_distance, best)) = candidates.first().cloned() {
        let coincident = candidates
            .into_iter()
            .filter(|(distance, candidate)| {
                (distance - best_distance).abs() <= 1e-6
                    || candidate.position.distance_squared(best.position) <= 1e-6
            })
            .map(|(_, candidate)| candidate)
            .collect::<Vec<_>>();
        let selected_index =
            update_snap_disambiguation(world, best.position, coincident.len(), cycle_requested);
        let selected = coincident
            .get(selected_index)
            .cloned()
            .unwrap_or_else(|| best.clone());
        result.position = Some(selected.position);
        result.target = Some(selected.position);
        result.kind = selected.kind;
        result.active_candidate = selected_index;
        result.candidates = coincident;
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
        let owner = entity_ref
            .get::<crate::plugins::identity::ElementId>()
            .copied();
        for handle in snapshot.handles() {
            let Some(kind) = authored_handle_snap_kind(handle.kind) else {
                continue;
            };
            out.push(SnapPoint {
                position: handle.position,
                kind,
                element_id: owner,
                label: Some(handle.id),
            });
        }
        if let Some(bounds) = snapshot.bounds() {
            collect_bounds_corner_snap_points(bounds, owner, out);
        }
    }
}

fn update_snap_disambiguation(
    world: &mut World,
    anchor: Vec3,
    candidate_count: usize,
    cycle_requested: bool,
) -> usize {
    let mut state = world.resource_mut::<SnapDisambiguationState>();
    let same_anchor = state
        .anchor
        .is_some_and(|previous| previous.distance_squared(anchor) <= 1e-6);
    if !same_anchor {
        state.anchor = Some(anchor);
        state.index = 0;
    } else if cycle_requested && candidate_count > 1 {
        state.index = (state.index + 1) % candidate_count;
    }
    state.index.min(candidate_count.saturating_sub(1))
}

fn collect_bounds_corner_snap_points(
    bounds: EntityBounds,
    owner: Option<crate::plugins::identity::ElementId>,
    out: &mut Vec<SnapPoint>,
) {
    for (index, position) in bounds.corners().into_iter().enumerate() {
        out.push(SnapPoint {
            position,
            kind: SnapKind::Endpoint,
            element_id: owner,
            label: Some(format!("bounds_corner_{index}")),
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
    for (index, candidate) in snap_result.candidates.iter().enumerate() {
        if index == snap_result.active_candidate {
            continue;
        }
        draw_diamond_marker(
            &mut gizmos,
            candidate.position,
            SNAP_HALO_RADIUS * 0.75,
            Color::srgba(1.0, 1.0, 1.0, 0.55),
        );
    }
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

        collect_bounds_corner_snap_points(
            bounds,
            Some(crate::plugins::identity::ElementId(1)),
            &mut snap_points,
        );

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
        assert!(snap_points.iter().all(
            |snap_point| snap_point.element_id == Some(crate::plugins::identity::ElementId(1))
        ));
        assert!(snap_points.iter().all(|snap_point| snap_point
            .label
            .as_deref()
            .is_some_and(|label| label.starts_with("bounds_corner_"))));
    }

    #[test]
    fn snap_disambiguation_cycles_coincident_candidates() {
        let mut world = World::new();
        world.init_resource::<SnapDisambiguationState>();
        let anchor = Vec3::new(1.0, 2.0, 3.0);

        assert_eq!(update_snap_disambiguation(&mut world, anchor, 2, false), 0);
        assert_eq!(update_snap_disambiguation(&mut world, anchor, 2, true), 1);
        assert_eq!(update_snap_disambiguation(&mut world, anchor, 2, true), 0);
        assert_eq!(
            update_snap_disambiguation(&mut world, anchor + Vec3::X, 2, false),
            0
        );
    }
}
