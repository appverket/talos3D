use std::collections::{HashMap, HashSet};

use bevy::{ecs::world::EntityRef, prelude::*, window::PrimaryWindow};

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::{
    authored_entity::{EntityBounds, HandleKind},
    capability_registry::{CapabilityRegistry, SnapPoint},
    plugins::{
        commands::find_entity_by_element_id_readonly,
        cursor::{cursor_window_position, CursorSystems, CursorWorldPos},
        identity::ElementId,
        layers::entity_on_visible_layer,
        modeling::group::{GroupEditContext, GroupMembers},
        render_pipeline::RenderSettings,
        selection::Selected,
        transform::TransformState,
    },
};

const ELEMENT_SNAP_RADIUS_METRES: f32 = 0.35;
const ELEMENT_SNAP_RADIUS_SCREEN_PX: f32 = 18.0;
const SNAP_TRAIL_COLOR: Color = Color::srgb(0.4, 1.0, 0.75);
const SNAP_ENDPOINT_COLOR: Color = Color::srgb(0.38, 0.92, 1.0);
const SNAP_MIDPOINT_COLOR: Color = Color::srgb(0.55, 1.0, 0.72);
const SNAP_CONTROL_COLOR: Color = Color::srgb(1.0, 0.82, 0.28);
const SNAP_GUIDE_ANCHOR_COLOR: Color = Color::srgb(0.0, 0.86, 0.86);
const SNAP_GRID_COLOR: Color = Color::srgb(0.4, 1.0, 0.75);
const SNAP_INDICATOR_RADIUS: f32 = 0.08;
const SNAP_ENDPOINT_RING_RADIUS: f32 = 0.045;
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

    let hovered_element_id = cursor_world_pos.hovered_element_id;
    let screen_pick = snap_screen_pick_context(world);
    let xray_enabled = world
        .get_resource::<RenderSettings>()
        .is_some_and(|settings| settings.xray_enabled);
    let group_visibility = SnapGroupVisibility::from_world(world);
    let mut candidates: Vec<(SnapCandidateRank, SnapCandidate)> = Vec::new();
    let mut snap_points = Vec::new();
    let registry = world.resource::<CapabilityRegistry>();
    for factory in registry.factories() {
        factory.collect_snap_points(world, &mut snap_points);
    }
    collect_authored_handle_snap_points(
        world,
        &mut snap_points,
        hovered_element_id,
        xray_enabled,
        &group_visibility,
    );

    for snap_point in snap_points {
        if !snap_point_is_selectable(
            world,
            snap_point.element_id,
            hovered_element_id,
            xray_enabled,
            &group_visibility,
        ) {
            continue;
        }
        let Some(rank) = snap_candidate_rank(&screen_pick, raw_position, snap_point.position)
        else {
            continue;
        };
        candidates.push((
            rank,
            SnapCandidate {
                position: snap_point.position,
                kind: snap_point.kind,
                element_id: snap_point.element_id,
                label: snap_point.label,
            },
        ));
    }

    candidates.sort_by(|left, right| {
        if xray_enabled {
            left.0.cmp_screen_first(&right.0)
        } else {
            left.0.cmp_front_first(&right.0)
        }
    });

    if let Some((best_rank, best)) = candidates.first().cloned() {
        let under_cursor = candidates
            .into_iter()
            .filter(|(rank, candidate)| {
                rank.is_coincident_with(&best_rank)
                    || candidate.position.distance_squared(best.position) <= 1e-6
                    || rank.screen_distance_squared
                        <= ELEMENT_SNAP_RADIUS_SCREEN_PX * ELEMENT_SNAP_RADIUS_SCREEN_PX
            })
            .map(|(_, candidate)| candidate)
            .collect::<Vec<_>>();
        let selected_index =
            update_snap_disambiguation(world, best.position, under_cursor.len(), cycle_requested);
        let selected = under_cursor
            .get(selected_index)
            .cloned()
            .unwrap_or_else(|| best.clone());
        result.position = Some(selected.position);
        result.target = Some(selected.position);
        result.kind = selected.kind;
        result.active_candidate = selected_index;
        result.candidates = under_cursor;
    }

    *world.resource_mut::<SnapResult>() = result;
}

fn collect_authored_handle_snap_points(
    world: &World,
    out: &mut Vec<SnapPoint>,
    hovered_element_id: Option<crate::plugins::identity::ElementId>,
    xray_enabled: bool,
    group_visibility: &SnapGroupVisibility,
) {
    let transform_active = !world.resource::<TransformState>().is_idle();
    let registry = world.resource::<CapabilityRegistry>();

    if !xray_enabled {
        let Some(owner) = hovered_element_id else {
            return;
        };
        if !group_visibility.accepts(owner) {
            return;
        }
        let Some(entity) = find_entity_by_element_id_readonly(world, owner) else {
            return;
        };
        let Some(entity_ref) = world.get_entity(entity).ok() else {
            return;
        };
        collect_entity_snap_points(
            world,
            registry,
            entity_ref,
            transform_active,
            out,
            group_visibility,
        );
        return;
    }

    let mut query = world
        .try_query::<EntityRef>()
        .expect("EntityRef query should always be constructible");

    for entity_ref in query.iter(world) {
        collect_entity_snap_points(
            world,
            registry,
            entity_ref,
            transform_active,
            out,
            group_visibility,
        );
    }
}

fn collect_entity_snap_points(
    world: &World,
    registry: &CapabilityRegistry,
    entity_ref: EntityRef<'_>,
    transform_active: bool,
    out: &mut Vec<SnapPoint>,
    group_visibility: &SnapGroupVisibility,
) {
    if !entity_ref.contains::<crate::plugins::identity::ElementId>() {
        return;
    }
    if transform_active && entity_ref.contains::<Selected>() {
        return;
    }
    if !entity_ref_is_visible_snap_owner(world, entity_ref) {
        return;
    }
    let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
        return;
    };
    if !group_visibility.accepts(snapshot.element_id()) {
        return;
    }
    if snapshot.scope() != crate::authored_entity::EntityScope::AuthoredModel {
        return;
    }
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

#[derive(Debug, Clone)]
struct SnapScreenPickContext {
    camera: Camera,
    camera_transform: GlobalTransform,
    cursor_position: Vec2,
}

#[derive(Debug, Clone, Copy)]
struct SnapCandidateRank {
    screen_distance_squared: f32,
    depth: f32,
    world_distance_squared: f32,
}

impl SnapCandidateRank {
    fn cmp_screen_first(&self, other: &Self) -> std::cmp::Ordering {
        self.screen_distance_squared
            .total_cmp(&other.screen_distance_squared)
            .then_with(|| self.depth.total_cmp(&other.depth))
            .then_with(|| {
                self.world_distance_squared
                    .total_cmp(&other.world_distance_squared)
            })
    }

    fn cmp_front_first(&self, other: &Self) -> std::cmp::Ordering {
        self.depth
            .total_cmp(&other.depth)
            .then_with(|| {
                self.screen_distance_squared
                    .total_cmp(&other.screen_distance_squared)
            })
            .then_with(|| {
                self.world_distance_squared
                    .total_cmp(&other.world_distance_squared)
            })
    }

    fn is_coincident_with(&self, other: &Self) -> bool {
        (self.screen_distance_squared - other.screen_distance_squared).abs() <= 1e-3
            && (self.depth - other.depth).abs() <= 1e-3
    }
}

fn snap_screen_pick_context(world: &mut World) -> Option<SnapScreenPickContext> {
    let cursor_position = {
        let mut window_query = world.query_filtered::<&Window, With<PrimaryWindow>>();
        cursor_window_position(window_query.single(world).ok()?)?
    };
    let (camera, camera_transform) = {
        let mut camera_query =
            world.query_filtered::<(&Camera, &GlobalTransform), With<crate::plugins::camera::OrbitCamera>>();
        camera_query
            .iter(world)
            .find(|(camera, _)| camera.is_active)
            .or_else(|| camera_query.iter(world).next())
            .map(|(camera, transform)| (camera.clone(), *transform))?
    };
    Some(SnapScreenPickContext {
        camera,
        camera_transform,
        cursor_position,
    })
}

fn snap_candidate_rank(
    screen_pick: &Option<SnapScreenPickContext>,
    raw_position: Vec3,
    position: Vec3,
) -> Option<SnapCandidateRank> {
    let world_distance_squared = raw_position.distance_squared(position);
    if let Some(screen_pick) = screen_pick {
        let Ok(screen_position) = screen_pick
            .camera
            .world_to_viewport(&screen_pick.camera_transform, position)
        else {
            return None;
        };
        let screen_distance_squared = screen_pick
            .cursor_position
            .distance_squared(screen_position);
        if screen_distance_squared > ELEMENT_SNAP_RADIUS_SCREEN_PX * ELEMENT_SNAP_RADIUS_SCREEN_PX {
            return None;
        }
        let depth = screen_pick
            .camera_transform
            .translation()
            .distance(position);
        return Some(SnapCandidateRank {
            screen_distance_squared,
            depth,
            world_distance_squared,
        });
    }

    if world_distance_squared > ELEMENT_SNAP_RADIUS_METRES * ELEMENT_SNAP_RADIUS_METRES {
        return None;
    }
    Some(SnapCandidateRank {
        screen_distance_squared: world_distance_squared,
        depth: 0.0,
        world_distance_squared,
    })
}

fn snap_point_is_selectable(
    world: &World,
    element_id: Option<crate::plugins::identity::ElementId>,
    hovered_element_id: Option<crate::plugins::identity::ElementId>,
    xray_enabled: bool,
    group_visibility: &SnapGroupVisibility,
) -> bool {
    let Some(element_id) = element_id else {
        return true;
    };
    if !group_visibility.accepts(element_id) {
        return false;
    }
    if !xray_enabled && hovered_element_id != Some(element_id) {
        return false;
    }
    let Some(entity) = find_entity_by_element_id_readonly(world, element_id) else {
        return true;
    };
    entity_is_visible_snap_owner(world, entity)
}

struct SnapGroupVisibility {
    active_group: Option<ElementId>,
    group_ids: HashSet<ElementId>,
    parent_by_child: HashMap<ElementId, ElementId>,
}

impl SnapGroupVisibility {
    fn from_world(world: &mut World) -> Self {
        let active_group = world
            .get_resource::<GroupEditContext>()
            .and_then(GroupEditContext::current_group);
        let mut group_ids = HashSet::new();
        let mut parent_by_child = HashMap::new();
        if let Some(mut group_query) = world.try_query::<(&ElementId, &GroupMembers)>() {
            for (group_id, members) in group_query.iter(world) {
                group_ids.insert(*group_id);
                for member_id in &members.member_ids {
                    parent_by_child.entry(*member_id).or_insert(*group_id);
                }
            }
        }

        Self {
            active_group,
            group_ids,
            parent_by_child,
        }
    }

    fn accepts(&self, element_id: ElementId) -> bool {
        if self.group_ids.contains(&element_id) {
            return false;
        }
        match self.active_group {
            Some(active_group) => self.parent_by_child.get(&element_id) == Some(&active_group),
            None => !self.parent_by_child.contains_key(&element_id),
        }
    }
}

fn entity_ref_is_visible_snap_owner(world: &World, entity_ref: EntityRef<'_>) -> bool {
    entity_is_visible_snap_owner(world, entity_ref.id())
}

fn entity_is_visible_snap_owner(world: &World, entity: Entity) -> bool {
    if !entity_on_visible_layer(world, entity) {
        return false;
    }
    world
        .get::<Visibility>(entity)
        .is_none_or(|visibility| *visibility != Visibility::Hidden)
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
    camera_query: Query<(&Camera, &GlobalTransform)>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    let (Some(raw_position), Some(target)) = (snap_result.raw_position, snap_result.target) else {
        return;
    };

    if raw_position.distance_squared(target) > f32::EPSILON {
        gizmos.line(raw_position, target, SNAP_TRAIL_COLOR);
    }
    draw_snap_target(
        &mut gizmos,
        target,
        snap_result.kind,
        active_snap_camera(camera_query.iter()).map(|(_, transform)| transform),
    );
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

fn draw_snap_target(
    gizmos: &mut Gizmos,
    target: Vec3,
    kind: SnapKind,
    camera_transform: Option<&GlobalTransform>,
) {
    match kind {
        SnapKind::Endpoint => {
            draw_endpoint_marker(gizmos, target, camera_transform, SNAP_ENDPOINT_COLOR);
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
        SnapKind::Endpoint => 20,
        SnapKind::Midpoint => 5,
        SnapKind::Control => 8,
        SnapKind::GuideAnchor => 7,
        SnapKind::Grid | SnapKind::None => 1,
    }
}

fn active_snap_camera<'a>(
    mut cameras: impl Iterator<Item = (&'a Camera, &'a GlobalTransform)>,
) -> Option<(&'a Camera, &'a GlobalTransform)> {
    let first = cameras.next()?;
    if first.0.is_active {
        return Some(first);
    }
    cameras.find(|(camera, _)| camera.is_active).or(Some(first))
}

fn draw_endpoint_marker(
    gizmos: &mut Gizmos,
    center: Vec3,
    camera_transform: Option<&GlobalTransform>,
    color: Color,
) {
    let rotation = camera_transform
        .and_then(|transform| (transform.translation() - center).try_normalize())
        .map(|normal| Quat::from_rotation_arc(Vec3::Z, normal))
        .unwrap_or(Quat::IDENTITY);
    gizmos
        .circle(
            Isometry3d::new(center, rotation),
            SNAP_ENDPOINT_RING_RADIUS,
            color,
        )
        .resolution(20);
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
    use crate::plugins::modeling::group::{GroupEditContext, GroupFrame, GroupMembers};

    fn snap_group_visibility_world() -> (World, ElementId, ElementId, ElementId, ElementId) {
        let mut world = World::new();
        world.insert_resource(GroupEditContext::default());

        let standalone_id = ElementId(1);
        let child_id = ElementId(10);
        let nested_group_id = ElementId(20);
        let root_group_id = ElementId(30);
        world.spawn(standalone_id);
        world.spawn(child_id);
        world.spawn((
            nested_group_id,
            GroupMembers {
                name: "Nested".to_string(),
                member_ids: vec![child_id],
                frame: GroupFrame::default(),
                linked_model: None,
            },
        ));
        world.spawn((
            root_group_id,
            GroupMembers {
                name: "Root".to_string(),
                member_ids: vec![nested_group_id],
                frame: GroupFrame::default(),
                linked_model: None,
            },
        ));

        (
            world,
            standalone_id,
            child_id,
            nested_group_id,
            root_group_id,
        )
    }

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

    #[test]
    fn snap_ranking_uses_screen_first_in_xray_and_front_first_outside_xray() {
        let front = SnapCandidateRank {
            screen_distance_squared: 4.0,
            depth: 3.0,
            world_distance_squared: 10.0,
        };
        let hidden = SnapCandidateRank {
            screen_distance_squared: 4.0,
            depth: 7.0,
            world_distance_squared: 1.0,
        };
        let off_cursor = SnapCandidateRank {
            screen_distance_squared: 9.0,
            depth: 1.0,
            world_distance_squared: 1.0,
        };

        assert_eq!(front.cmp_screen_first(&hidden), std::cmp::Ordering::Less);
        assert_eq!(front.cmp_front_first(&hidden), std::cmp::Ordering::Less);
        assert_eq!(
            off_cursor.cmp_screen_first(&front),
            std::cmp::Ordering::Greater
        );
        assert_eq!(off_cursor.cmp_front_first(&front), std::cmp::Ordering::Less);
    }

    #[test]
    fn owned_snap_points_require_hovered_owner_unless_xray_is_enabled() {
        let mut world = World::new();
        let owner = Some(crate::plugins::identity::ElementId(1));
        let other = Some(crate::plugins::identity::ElementId(2));
        let visibility = SnapGroupVisibility::from_world(&mut world);

        assert!(snap_point_is_selectable(
            &world,
            None,
            None,
            false,
            &visibility
        ));
        assert!(snap_point_is_selectable(
            &world,
            owner,
            owner,
            false,
            &visibility
        ));
        assert!(!snap_point_is_selectable(
            &world,
            owner,
            None,
            false,
            &visibility
        ));
        assert!(!snap_point_is_selectable(
            &world,
            owner,
            other,
            false,
            &visibility
        ));
        assert!(snap_point_is_selectable(
            &world,
            owner,
            None,
            true,
            &visibility
        ));
    }

    #[test]
    fn snap_group_visibility_at_root_accepts_standalone_entities_only() {
        let (mut world, standalone_id, child_id, nested_group_id, root_group_id) =
            snap_group_visibility_world();

        let visibility = SnapGroupVisibility::from_world(&mut world);

        assert!(visibility.accepts(standalone_id));
        assert!(!visibility.accepts(root_group_id));
        assert!(!visibility.accepts(nested_group_id));
        assert!(!visibility.accepts(child_id));
    }

    #[test]
    fn snap_group_visibility_inside_group_waits_for_nested_drill_in() {
        let (mut world, standalone_id, child_id, nested_group_id, root_group_id) =
            snap_group_visibility_world();
        let mut context = GroupEditContext::default();
        context.enter(root_group_id);
        world.insert_resource(context);

        let visibility = SnapGroupVisibility::from_world(&mut world);

        assert!(!visibility.accepts(standalone_id));
        assert!(!visibility.accepts(root_group_id));
        assert!(!visibility.accepts(nested_group_id));
        assert!(!visibility.accepts(child_id));
    }

    #[test]
    fn snap_group_visibility_inside_nested_group_accepts_nested_members() {
        let (mut world, standalone_id, child_id, nested_group_id, root_group_id) =
            snap_group_visibility_world();
        let mut context = GroupEditContext::default();
        context.enter(root_group_id);
        context.enter(nested_group_id);
        world.insert_resource(context);

        let visibility = SnapGroupVisibility::from_world(&mut world);

        assert!(!visibility.accepts(standalone_id));
        assert!(!visibility.accepts(root_group_id));
        assert!(!visibility.accepts(nested_group_id));
        assert!(visibility.accepts(child_id));
    }
}
