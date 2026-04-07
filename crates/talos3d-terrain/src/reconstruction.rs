use std::collections::{BTreeMap, BTreeSet, HashMap};

use bevy::prelude::*;
use delaunator::{triangulate, Point};

use crate::components::ElevationCurve;

const REPAIR_EPSILON: f32 = 1.0e-4;
const DEFAULT_MIN_ALIGNMENT_DOT: f32 = 0.35;
const DEFAULT_IDW_NEIGHBOR_COUNT: usize = 8;
const MAX_ALPHA_RADIUS_FACTOR: f32 = 8.0;
const RELAXED_ALPHA_RADIUS_FACTOR: f32 = 20.0;
const MAX_BRIDGE_LENGTH_FACTOR: f32 = 6.0;
const MIN_BRIDGE_DIRECTION_DOT: f32 = 0.5;
const MAX_BRIDGE_LATERAL_FACTOR: f32 = 1.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContourRepairSettings {
    pub join_tolerance: f32,
    pub min_alignment_dot: f32,
}

impl Default for ContourRepairSettings {
    fn default() -> Self {
        Self {
            join_tolerance: crate::components::DEFAULT_TERRAIN_CONTOUR_JOIN_TOLERANCE,
            min_alignment_dot: DEFAULT_MIN_ALIGNMENT_DOT,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepairedContour {
    pub curve: ElevationCurve,
    pub inserted_bridge_count: usize,
    pub source_fragment_count: usize,
}

#[derive(Debug, Clone)]
struct ContourFragment {
    points: Vec<Vec3>,
    elevation: f32,
    source_layer: String,
    curve_type: crate::components::ElevationCurveType,
    survey_source_id: Option<String>,
    inserted_bridge_count: usize,
    source_fragment_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct EndpointJoinCandidate {
    left_index: usize,
    right_index: usize,
    left_end: EndpointKind,
    right_end: EndpointKind,
    score: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EndpointKind {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FragmentEndpoint {
    fragment_index: usize,
    endpoint: EndpointKind,
}

pub fn repair_elevation_curves(
    curves: &[ElevationCurve],
    settings: ContourRepairSettings,
) -> Vec<RepairedContour> {
    let mut groups = BTreeMap::<(i32, String), Vec<ContourFragment>>::new();
    for curve in curves {
        let points = normalize_points(&curve.points);
        if points.len() < 2 {
            continue;
        }
        groups
            .entry((
                quantize_elevation(curve.elevation),
                curve.source_layer.clone(),
            ))
            .or_default()
            .push(ContourFragment {
                points,
                elevation: curve.elevation,
                source_layer: curve.source_layer.clone(),
                curve_type: curve.curve_type,
                survey_source_id: curve.survey_source_id.clone(),
                inserted_bridge_count: 0,
                source_fragment_count: 1,
            });
    }

    let mut repaired = Vec::new();
    for (_, mut fragments) in groups {
        while let Some(candidate) = best_join_candidate(&fragments, settings) {
            let (keep, remove) = if candidate.left_index < candidate.right_index {
                (candidate.left_index, candidate.right_index)
            } else {
                (candidate.right_index, candidate.left_index)
            };
            let right = fragments.remove(remove);
            let left = fragments.remove(keep);
            fragments.push(merge_fragments(left, right, candidate));
        }

        for fragment in fragments {
            let mut points = fragment.points;
            close_fragment_if_needed(&mut points, settings);
            if points.len() < 2 {
                continue;
            }
            repaired.push(RepairedContour {
                curve: ElevationCurve {
                    points,
                    elevation: fragment.elevation,
                    source_layer: fragment.source_layer,
                    curve_type: fragment.curve_type,
                    survey_source_id: fragment.survey_source_id,
                },
                inserted_bridge_count: fragment.inserted_bridge_count,
                source_fragment_count: fragment.source_fragment_count,
            });
        }
    }

    repaired
}

pub fn sample_curve_points(points: &[Vec3], spacing: f32) -> Vec<Vec3> {
    let points = normalize_points(points);
    if points.len() < 2 {
        return points;
    }

    let spacing = spacing.max(REPAIR_EPSILON);
    let mut sampled = Vec::with_capacity(points.len());
    if let Some(first) = points.first() {
        sampled.push(*first);
    }
    for segment in points.windows(2) {
        let start = segment[0];
        let end = segment[1];
        let length = start.distance(end);
        if length <= REPAIR_EPSILON {
            continue;
        }
        let subdivisions = (length / spacing).floor() as usize;
        for step in 1..=subdivisions {
            let distance = step as f32 * spacing;
            if distance >= length {
                break;
            }
            sampled.push(start.lerp(end, distance / length));
        }
        sampled.push(end);
    }
    normalize_points(&sampled)
}

pub fn sample_boundary_support_points(
    boundary: &[Vec2],
    spacing: f32,
    contour_points: &[Vec3],
) -> Vec<Vec3> {
    if boundary.len() < 3 || contour_points.is_empty() {
        return Vec::new();
    }

    let edge_points = sample_boundary_edge_points(boundary, spacing);
    edge_points
        .into_iter()
        .map(|point| {
            Vec3::new(
                point.x,
                interpolate_height_idw(point, contour_points),
                point.y,
            )
        })
        .collect()
}

pub fn sample_interior_support_points(
    boundary: &[Vec2],
    spacing: f32,
    contour_points: &[Vec3],
) -> Vec<Vec3> {
    if boundary.len() < 3 || contour_points.is_empty() {
        return Vec::new();
    }

    let Some((min, max)) = planar_bounds_2d(boundary) else {
        return Vec::new();
    };
    let spacing = spacing.max(REPAIR_EPSILON);
    let clearance_squared = (spacing * 0.35).powi(2);
    let mut samples = Vec::new();
    let mut z = min.y + spacing * 0.5;
    while z < max.y {
        let mut x = min.x + spacing * 0.5;
        while x < max.x {
            let point = Vec2::new(x, z);
            if point_in_polygon_2d(point, boundary)
                && contour_points.iter().all(|sample| {
                    Vec2::new(sample.x, sample.z).distance_squared(point) > clearance_squared
                })
            {
                samples.push(Vec3::new(
                    point.x,
                    interpolate_height_idw(point, contour_points),
                    point.y,
                ));
            }
            x += spacing;
        }
        z += spacing;
    }
    samples
}

pub fn estimate_terrain_boundary(curves: &[ElevationCurve], spacing: f32) -> Vec<Vec2> {
    let spacing = spacing.max(REPAIR_EPSILON);
    let dilation_amount = if curves.len() >= 10 {
        spacing * 1.5
    } else {
        0.0
    };
    let all_samples = dedupe_vec2(
        &curves
            .iter()
            .flat_map(|curve| sample_curve_points(&curve.points, spacing))
            .map(|point| Vec2::new(point.x, point.z))
            .collect::<Vec<_>>(),
    );
    let boundary = estimate_boundary_from_samples(&all_samples, spacing, None);
    if should_fallback_to_terminus_boundary(&boundary, curves.len()) {
        let relaxed_boundary = estimate_boundary_from_samples(
            &all_samples,
            spacing,
            Some(spacing * RELAXED_ALPHA_RADIUS_FACTOR),
        );
        if relaxed_boundary.len() >= 3 && relaxed_boundary.len() < boundary.len() {
            return relaxed_boundary;
        }
    }

    let terminus_samples = contour_terminus_samples(curves, spacing);
    if should_fallback_to_terminus_boundary(&boundary, curves.len())
        && terminus_samples.len() >= 6
        && terminus_samples_cover_model(&terminus_samples, &all_samples)
    {
        let terminus_boundary =
            estimate_boundary_from_samples(&terminus_samples, spacing * 2.0, None);
        if terminus_boundary.len() >= 3 {
            return dilate_boundary(&terminus_boundary, dilation_amount);
        }
    }
    dilate_boundary(&boundary, dilation_amount)
}

fn estimate_boundary_from_samples(
    samples: &[Vec2],
    spacing: f32,
    alpha_radius_override: Option<f32>,
) -> Vec<Vec2> {
    if samples.len() < 3 {
        return convex_hull(samples);
    }
    let triangulation = triangulate(
        &samples
            .iter()
            .map(|point| Point {
                x: f64::from(point.x),
                y: f64::from(point.y),
            })
            .collect::<Vec<_>>(),
    );
    if triangulation.triangles.is_empty() {
        return convex_hull(&samples);
    }

    let alpha_radius = alpha_radius_override
        .unwrap_or_else(|| estimate_alpha_radius(&samples, &triangulation.triangles, spacing));
    let boundary_edges = alpha_boundary_edges(&samples, &triangulation.triangles, alpha_radius);
    let loops = trace_boundary_loops(&samples, &boundary_edges);
    loops
        .into_iter()
        .max_by(|left, right| {
            polygon_area(left)
                .abs()
                .total_cmp(&polygon_area(right).abs())
        })
        .filter(|loop_points| loop_points.len() >= 3)
        .unwrap_or_else(|| convex_hull(samples))
}

fn should_fallback_to_terminus_boundary(boundary: &[Vec2], curve_count: usize) -> bool {
    boundary.len() > curve_count.saturating_mul(10).max(256)
}

fn terminus_samples_cover_model(terminus_samples: &[Vec2], all_samples: &[Vec2]) -> bool {
    let Some((terminus_min, terminus_max)) = planar_bounds_2d(terminus_samples) else {
        return false;
    };
    let Some((all_min, all_max)) = planar_bounds_2d(all_samples) else {
        return false;
    };
    let terminus_extent = (terminus_max - terminus_min).max(Vec2::splat(REPAIR_EPSILON));
    let all_extent = (all_max - all_min).max(Vec2::splat(REPAIR_EPSILON));
    terminus_extent.x >= all_extent.x * 0.7 && terminus_extent.y >= all_extent.y * 0.7
}

fn planar_bounds_2d(points: &[Vec2]) -> Option<(Vec2, Vec2)> {
    let mut iter = points.iter().copied();
    let first = iter.next()?;
    let mut min = first;
    let mut max = first;
    for point in iter {
        min = min.min(point);
        max = max.max(point);
    }
    Some((min, max))
}

fn contour_terminus_samples(curves: &[ElevationCurve], spacing: f32) -> Vec<Vec2> {
    let mut samples = Vec::new();
    let closure_tolerance = spacing.max(REPAIR_EPSILON) * 2.0;
    for curve in curves {
        let points = normalize_points(&curve.points);
        if points.len() < 2 {
            continue;
        }
        let first = points[0];
        let second = points.get(1).copied().unwrap_or(first);
        let penultimate = points
            .get(points.len().saturating_sub(2))
            .copied()
            .unwrap_or(first);
        let last = *points.last().unwrap_or(&first);
        if first.distance(last) <= closure_tolerance {
            continue;
        }
        samples.push(first.xz());
        samples.push(second.xz());
        samples.push(penultimate.xz());
        samples.push(last.xz());
    }
    dedupe_vec2(&samples)
}

fn dilate_boundary(boundary: &[Vec2], amount: f32) -> Vec<Vec2> {
    if boundary.len() < 3 || amount <= REPAIR_EPSILON {
        return boundary.to_vec();
    }
    let centroid = boundary
        .iter()
        .copied()
        .reduce(|sum, point| sum + point)
        .map(|sum| sum / boundary.len() as f32)
        .unwrap_or(Vec2::ZERO);
    boundary
        .iter()
        .copied()
        .map(|point| {
            let direction = (point - centroid).normalize_or_zero();
            if direction.length_squared() <= REPAIR_EPSILON {
                point
            } else {
                point + direction * amount
            }
        })
        .collect()
}

pub fn planar_bounds_center(points: impl Iterator<Item = Vec3>) -> Option<Vec2> {
    let mut points = points.peekable();
    let first = points.peek().copied()?;
    let mut min = first.xz();
    let mut max = first.xz();
    for point in points {
        min = min.min(point.xz());
        max = max.max(point.xz());
    }
    Some((min + max) * 0.5)
}

fn sample_boundary_edge_points(boundary: &[Vec2], spacing: f32) -> Vec<Vec2> {
    let mut samples = Vec::new();
    let spacing = spacing.max(REPAIR_EPSILON);
    for index in 0..boundary.len() {
        let start = boundary[index];
        let end = boundary[(index + 1) % boundary.len()];
        if samples.last().copied() != Some(start) {
            samples.push(start);
        }
        let length = start.distance(end);
        if length <= REPAIR_EPSILON {
            continue;
        }
        let subdivisions = (length / spacing).floor() as usize;
        for step in 1..=subdivisions {
            let distance = step as f32 * spacing;
            if distance >= length {
                break;
            }
            samples.push(start.lerp(end, distance / length));
        }
        samples.push(end);
    }
    dedupe_vec2(&samples)
}

fn alpha_boundary_edges(
    points: &[Vec2],
    triangles: &[usize],
    alpha_radius: f32,
) -> Vec<(usize, usize)> {
    let mut edge_counts = HashMap::<(usize, usize), usize>::new();
    for triangle in triangles.chunks_exact(3) {
        let a = points[triangle[0]];
        let b = points[triangle[1]];
        let c = points[triangle[2]];
        if triangle_area(a, b, c) <= REPAIR_EPSILON {
            continue;
        }
        let circumradius = triangle_circumradius(a, b, c);
        if !circumradius.is_finite() || circumradius > alpha_radius {
            continue;
        }
        for (u, v) in [
            ordered_edge(triangle[0], triangle[1]),
            ordered_edge(triangle[1], triangle[2]),
            ordered_edge(triangle[2], triangle[0]),
        ] {
            *edge_counts.entry((u, v)).or_insert(0) += 1;
        }
    }
    edge_counts
        .into_iter()
        .filter_map(|(edge, count)| (count == 1).then_some(edge))
        .collect()
}

fn trace_boundary_loops(points: &[Vec2], edges: &[(usize, usize)]) -> Vec<Vec<Vec2>> {
    if edges.is_empty() {
        return Vec::new();
    }

    let mut adjacency = HashMap::<usize, Vec<usize>>::new();
    for &(u, v) in edges {
        adjacency.entry(u).or_default().push(v);
        adjacency.entry(v).or_default().push(u);
    }

    let mut remaining = edges
        .iter()
        .map(|&(u, v)| ordered_edge(u, v))
        .collect::<BTreeSet<_>>();
    let mut loops = Vec::new();

    while let Some(&(start_u, start_v)) = remaining.iter().next() {
        let mut loop_indices = vec![start_u];
        let mut previous = start_u;
        let mut current = start_v;
        remaining.remove(&ordered_edge(start_u, start_v));
        let mut guard = 0usize;

        while guard < edges.len() * 2 {
            guard += 1;
            loop_indices.push(current);
            let Some(neighbors) = adjacency.get(&current) else {
                break;
            };
            let next = neighbors
                .iter()
                .copied()
                .find(|neighbor| {
                    *neighbor != previous && remaining.contains(&ordered_edge(current, *neighbor))
                })
                .or_else(|| {
                    neighbors
                        .iter()
                        .copied()
                        .find(|neighbor| *neighbor == loop_indices[0])
                });
            let Some(next) = next else {
                break;
            };
            remaining.remove(&ordered_edge(current, next));
            previous = current;
            current = next;
            if current == loop_indices[0] {
                break;
            }
        }

        if loop_indices.len() >= 3 && current == loop_indices[0] {
            loop_indices.pop();
            loops.push(
                loop_indices
                    .into_iter()
                    .map(|index| points[index])
                    .collect(),
            );
        }
    }

    loops
}

fn estimate_alpha_radius(points: &[Vec2], triangles: &[usize], spacing: f32) -> f32 {
    let mut radii = triangles
        .chunks_exact(3)
        .filter_map(|triangle| {
            let a = points[triangle[0]];
            let b = points[triangle[1]];
            let c = points[triangle[2]];
            (triangle_area(a, b, c) > REPAIR_EPSILON).then_some(triangle_circumradius(a, b, c))
        })
        .filter(|radius| radius.is_finite())
        .collect::<Vec<_>>();
    if radii.is_empty() {
        return spacing * MAX_ALPHA_RADIUS_FACTOR;
    }
    radii.sort_by(|left, right| left.total_cmp(right));
    let median = radii[radii.len() / 2];
    median.clamp(spacing * 2.0, spacing * MAX_ALPHA_RADIUS_FACTOR)
}

fn triangle_area(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() * 0.5
}

fn point_in_polygon_2d(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    for index in 0..polygon.len() {
        let current = polygon[index];
        let next = polygon[(index + 1) % polygon.len()];
        let denominator = next.y - current.y;
        let intersects = (current.y > point.y) != (next.y > point.y)
            && point.x < (next.x - current.x) * (point.y - current.y) / denominator + current.x;
        if intersects {
            inside = !inside;
        }
    }
    inside
}

fn triangle_circumradius(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    let ab = a.distance(b);
    let bc = b.distance(c);
    let ca = c.distance(a);
    let area = triangle_area(a, b, c);
    if area <= REPAIR_EPSILON {
        return f32::INFINITY;
    }
    (ab * bc * ca) / (4.0 * area)
}

fn ordered_edge(u: usize, v: usize) -> (usize, usize) {
    if u < v {
        (u, v)
    } else {
        (v, u)
    }
}

fn polygon_area(points: &[Vec2]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for index in 0..points.len() {
        let current = points[index];
        let next = points[(index + 1) % points.len()];
        area += current.x * next.y - next.x * current.y;
    }
    area * 0.5
}

fn convex_hull(points: &[Vec2]) -> Vec<Vec2> {
    if points.len() <= 1 {
        return points.to_vec();
    }
    let mut points = points.to_vec();
    points.sort_by(|left, right| left.x.total_cmp(&right.x).then(left.y.total_cmp(&right.y)));

    let mut lower = Vec::new();
    for point in &points {
        while lower.len() >= 2
            && cross(lower[lower.len() - 2], lower[lower.len() - 1], *point) <= 0.0
        {
            lower.pop();
        }
        lower.push(*point);
    }

    let mut upper = Vec::new();
    for point in points.iter().rev() {
        while upper.len() >= 2
            && cross(upper[upper.len() - 2], upper[upper.len() - 1], *point) <= 0.0
        {
            upper.pop();
        }
        upper.push(*point);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn cross(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn interpolate_height_idw(point: Vec2, contour_points: &[Vec3]) -> f32 {
    let mut distances = contour_points
        .iter()
        .map(|sample| {
            (
                Vec2::new(sample.x, sample.z).distance_squared(point),
                sample.y,
            )
        })
        .collect::<Vec<_>>();
    distances.sort_by(|left, right| left.0.total_cmp(&right.0));

    if let Some((distance_squared, elevation)) = distances.first().copied() {
        if distance_squared <= REPAIR_EPSILON {
            return elevation;
        }
    }

    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for (distance_squared, elevation) in distances.into_iter().take(DEFAULT_IDW_NEIGHBOR_COUNT) {
        let weight = 1.0 / distance_squared.max(REPAIR_EPSILON);
        weighted_sum += elevation * weight;
        weight_sum += weight;
    }
    if weight_sum <= REPAIR_EPSILON {
        0.0
    } else {
        weighted_sum / weight_sum
    }
}

fn best_join_candidate(
    fragments: &[ContourFragment],
    settings: ContourRepairSettings,
) -> Option<EndpointJoinCandidate> {
    let mut candidates = Vec::new();
    let mut best_per_endpoint = HashMap::<FragmentEndpoint, EndpointJoinCandidate>::new();
    for left_index in 0..fragments.len() {
        for right_index in (left_index + 1)..fragments.len() {
            let left = &fragments[left_index];
            let right = &fragments[right_index];
            for left_end in [EndpointKind::Start, EndpointKind::End] {
                for right_end in [EndpointKind::Start, EndpointKind::End] {
                    let Some(score) = join_score(left, right, left_end, right_end, settings) else {
                        continue;
                    };
                    let candidate = EndpointJoinCandidate {
                        left_index,
                        right_index,
                        left_end,
                        right_end,
                        score,
                    };
                    candidates.push(candidate);
                    update_endpoint_best(&mut best_per_endpoint, left_index, left_end, candidate);
                    update_endpoint_best(&mut best_per_endpoint, right_index, right_end, candidate);
                }
            }
        }
    }

    candidates
        .into_iter()
        .filter(|candidate| candidate_is_mutual_best(*candidate, &best_per_endpoint))
        .min_by(|left, right| left.score.total_cmp(&right.score))
}

fn join_score(
    left: &ContourFragment,
    right: &ContourFragment,
    left_end: EndpointKind,
    right_end: EndpointKind,
    settings: ContourRepairSettings,
) -> Option<f32> {
    let left_points = oriented_points(&left.points, left_end, true);
    let right_points = oriented_points(&right.points, right_end, false);
    let left_end_point = *left_points.last()?;
    let right_start_point = *right_points.first()?;
    let gap = right_start_point - left_end_point;
    let distance = gap.length();
    let bridge_limit = settings.join_tolerance.max(REPAIR_EPSILON) * MAX_BRIDGE_LENGTH_FACTOR;
    if distance > bridge_limit {
        return None;
    }

    let left_direction = terminal_direction(&left_points, true);
    let right_direction = terminal_direction(&right_points, false);
    let alignment = match (left_direction, right_direction) {
        (Some(left_direction), Some(right_direction)) => left_direction.dot(right_direction),
        _ => 1.0,
    };
    if alignment < settings.min_alignment_dot {
        return None;
    }

    if distance <= REPAIR_EPSILON {
        return Some((1.0 - alignment.clamp(-1.0, 1.0)) * settings.join_tolerance);
    }

    let gap_direction = gap / distance;
    let left_forward = left_direction.map_or(1.0, |direction| direction.dot(gap_direction));
    let right_forward = right_direction.map_or(1.0, |direction| direction.dot(gap_direction));
    if left_forward < MIN_BRIDGE_DIRECTION_DOT || right_forward < MIN_BRIDGE_DIRECTION_DOT {
        return None;
    }

    let lateral_limit = settings.join_tolerance.max(REPAIR_EPSILON) * MAX_BRIDGE_LATERAL_FACTOR;
    let left_lateral_error = line_deviation(
        left_end_point,
        left_direction.unwrap_or(gap_direction),
        right_start_point,
    );
    let right_lateral_error = line_deviation(
        right_start_point,
        right_direction.unwrap_or(gap_direction),
        left_end_point,
    );
    if left_lateral_error > lateral_limit || right_lateral_error > lateral_limit {
        return None;
    }

    let score = distance
        + (1.0 - alignment.clamp(-1.0, 1.0)) * settings.join_tolerance
        + (1.0 - left_forward.clamp(-1.0, 1.0)) * settings.join_tolerance
        + (1.0 - right_forward.clamp(-1.0, 1.0)) * settings.join_tolerance
        + left_lateral_error
        + right_lateral_error;
    Some(score)
}

fn update_endpoint_best(
    best_per_endpoint: &mut HashMap<FragmentEndpoint, EndpointJoinCandidate>,
    fragment_index: usize,
    endpoint: EndpointKind,
    candidate: EndpointJoinCandidate,
) {
    let endpoint = FragmentEndpoint {
        fragment_index,
        endpoint,
    };
    let current_best = best_per_endpoint.get(&endpoint).copied();
    if current_best.is_none_or(|current| candidate.score < current.score) {
        best_per_endpoint.insert(endpoint, candidate);
    }
}

fn candidate_is_mutual_best(
    candidate: EndpointJoinCandidate,
    best_per_endpoint: &HashMap<FragmentEndpoint, EndpointJoinCandidate>,
) -> bool {
    let left_endpoint = FragmentEndpoint {
        fragment_index: candidate.left_index,
        endpoint: candidate.left_end,
    };
    let right_endpoint = FragmentEndpoint {
        fragment_index: candidate.right_index,
        endpoint: candidate.right_end,
    };
    best_per_endpoint
        .get(&left_endpoint)
        .zip(best_per_endpoint.get(&right_endpoint))
        .is_some_and(|(left_best, right_best)| {
            same_join_endpoints(*left_best, candidate)
                && same_join_endpoints(*right_best, candidate)
        })
}

fn same_join_endpoints(left: EndpointJoinCandidate, right: EndpointJoinCandidate) -> bool {
    left.left_index == right.left_index
        && left.right_index == right.right_index
        && left.left_end == right.left_end
        && left.right_end == right.right_end
}

fn line_deviation(origin: Vec3, direction: Vec3, point: Vec3) -> f32 {
    let direction = direction.normalize_or_zero();
    if direction.length_squared() <= REPAIR_EPSILON {
        return origin.distance(point);
    }
    let delta = point - origin;
    let along = delta.dot(direction);
    let projection = origin + direction * along;
    point.distance(projection)
}

fn merge_fragments(
    left: ContourFragment,
    right: ContourFragment,
    candidate: EndpointJoinCandidate,
) -> ContourFragment {
    let mut left_points = oriented_points(&left.points, candidate.left_end, true);
    let right_points = oriented_points(&right.points, candidate.right_end, false);
    let join_distance = left_points
        .last()
        .copied()
        .zip(right_points.first().copied())
        .map_or(0.0, |(left_end, right_start)| {
            left_end.distance(right_start)
        });

    let mut inserted_bridge_count = left.inserted_bridge_count + right.inserted_bridge_count;
    if join_distance > REPAIR_EPSILON {
        inserted_bridge_count += 1;
        left_points.push(right_points[0]);
        left_points.extend(right_points.iter().copied().skip(1));
    } else {
        left_points.extend(right_points.iter().copied().skip(1));
    }

    ContourFragment {
        points: normalize_points(&left_points),
        elevation: left.elevation,
        source_layer: left.source_layer,
        curve_type: strongest_curve_type(left.curve_type, right.curve_type),
        survey_source_id: merge_survey_source_id(
            left.survey_source_id.as_deref(),
            right.survey_source_id.as_deref(),
        ),
        inserted_bridge_count,
        source_fragment_count: left.source_fragment_count + right.source_fragment_count,
    }
}

fn close_fragment_if_needed(points: &mut Vec<Vec3>, settings: ContourRepairSettings) {
    if points.len() < 3 {
        return;
    }
    let first = points.first().copied().unwrap_or(Vec3::ZERO);
    let last = points.last().copied().unwrap_or(Vec3::ZERO);
    if first.distance(last) > settings.join_tolerance {
        return;
    }

    let start_direction = terminal_direction(points, false);
    let end_direction = terminal_direction(points, true);
    let alignment = match (start_direction, end_direction) {
        (Some(start_direction), Some(end_direction)) => start_direction.dot(end_direction),
        _ => 1.0,
    };
    if alignment < settings.min_alignment_dot {
        return;
    }

    let join = first.lerp(last, 0.5);
    if let Some(first_mut) = points.first_mut() {
        *first_mut = join;
    }
    if let Some(last_mut) = points.last_mut() {
        *last_mut = join;
    }
}

fn oriented_points(
    points: &[Vec3],
    endpoint: EndpointKind,
    endpoint_should_be_end: bool,
) -> Vec<Vec3> {
    let mut oriented = points.to_vec();
    let should_reverse = matches!(
        (endpoint, endpoint_should_be_end),
        (EndpointKind::Start, true) | (EndpointKind::End, false)
    );
    if should_reverse {
        oriented.reverse();
    }
    oriented
}

fn terminal_direction(points: &[Vec3], at_end: bool) -> Option<Vec3> {
    if points.len() < 2 {
        return None;
    }
    let (start, end) = if at_end {
        (points[points.len() - 2], points[points.len() - 1])
    } else {
        (points[0], points[1])
    };
    let direction = (end - start).normalize_or_zero();
    (direction.length_squared() > REPAIR_EPSILON).then_some(direction)
}

fn normalize_points(points: &[Vec3]) -> Vec<Vec3> {
    let mut normalized = Vec::with_capacity(points.len());
    for point in points {
        if normalized
            .last()
            .is_some_and(|existing: &Vec3| existing.distance_squared(*point) <= REPAIR_EPSILON)
        {
            continue;
        }
        normalized.push(*point);
    }
    normalized
}

fn dedupe_vec2(points: &[Vec2]) -> Vec<Vec2> {
    let mut normalized = Vec::with_capacity(points.len());
    for point in points {
        if normalized
            .last()
            .is_some_and(|existing: &Vec2| existing.distance_squared(*point) <= REPAIR_EPSILON)
        {
            continue;
        }
        normalized.push(*point);
    }
    normalized
}

fn quantize_elevation(elevation: f32) -> i32 {
    (elevation * 1000.0).round() as i32
}

fn strongest_curve_type(
    left: crate::components::ElevationCurveType,
    right: crate::components::ElevationCurveType,
) -> crate::components::ElevationCurveType {
    if curve_type_rank(left) >= curve_type_rank(right) {
        left
    } else {
        right
    }
}

fn curve_type_rank(curve_type: crate::components::ElevationCurveType) -> usize {
    match curve_type {
        crate::components::ElevationCurveType::Index => 3,
        crate::components::ElevationCurveType::Major => 2,
        crate::components::ElevationCurveType::Minor => 1,
        crate::components::ElevationCurveType::Supplementary => 0,
    }
}

fn merge_survey_source_id(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), None) => Some(left.to_string()),
        (None, Some(right)) => Some(right.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::ElevationCurveType;

    fn curve(points: &[[f32; 3]], elevation: f32) -> ElevationCurve {
        ElevationCurve {
            points: points
                .iter()
                .map(|point| Vec3::new(point[0], point[1], point[2]))
                .collect(),
            elevation,
            source_layer: format!("Contour_{elevation:.1}"),
            curve_type: ElevationCurveType::Major,
            survey_source_id: Some("survey-1".to_string()),
        }
    }

    #[test]
    fn repairs_broken_contours_by_joining_aligned_endpoints() {
        let repaired = repair_elevation_curves(
            &[
                curve(&[[0.0, 10.0, 0.0], [5.0, 10.0, 0.0]], 10.0),
                curve(&[[6.0, 10.0, 0.0], [10.0, 10.0, 0.0]], 10.0),
            ],
            ContourRepairSettings {
                join_tolerance: 1.5,
                min_alignment_dot: 0.0,
            },
        );

        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].source_fragment_count, 2);
        assert_eq!(repaired[0].inserted_bridge_count, 1);
        assert_eq!(
            repaired[0].curve.points.first(),
            Some(&Vec3::new(0.0, 10.0, 0.0))
        );
        assert_eq!(
            repaired[0].curve.points.last(),
            Some(&Vec3::new(10.0, 10.0, 0.0))
        );
    }

    #[test]
    fn repairs_short_building_gap_when_fragment_directions_stay_aligned() {
        let repaired = repair_elevation_curves(
            &[
                curve(&[[0.0, 10.0, 0.0], [4.0, 10.0, 0.2]], 10.0),
                curve(&[[8.0, 10.0, 0.3], [12.0, 10.0, 0.5]], 10.0),
            ],
            ContourRepairSettings::default(),
        );

        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].source_fragment_count, 2);
        assert_eq!(repaired[0].inserted_bridge_count, 1);
    }

    #[test]
    fn does_not_join_crossing_candidates_that_are_not_mutual_best() {
        let repaired = repair_elevation_curves(
            &[
                curve(&[[0.0, 10.0, 0.0], [4.0, 10.0, 0.0]], 10.0),
                curve(&[[8.0, 10.0, 0.0], [12.0, 10.0, 0.0]], 10.0),
                curve(&[[4.2, 10.0, 2.0], [8.2, 10.0, 2.0]], 10.0),
            ],
            ContourRepairSettings::default(),
        );

        assert_eq!(repaired.len(), 2);
        assert!(repaired
            .iter()
            .any(|curve| curve.source_fragment_count == 2));
    }

    #[test]
    fn does_not_join_contours_with_different_elevations() {
        let repaired = repair_elevation_curves(
            &[
                curve(&[[0.0, 10.0, 0.0], [5.0, 10.0, 0.0]], 10.0),
                curve(&[[5.2, 11.0, 0.0], [10.0, 11.0, 0.0]], 11.0),
            ],
            ContourRepairSettings {
                join_tolerance: 1.0,
                min_alignment_dot: 0.0,
            },
        );

        assert_eq!(repaired.len(), 2);
    }

    #[test]
    fn samples_boundary_support_points_with_interpolated_heights() {
        let support_points = sample_boundary_support_points(
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            5.0,
            &[
                Vec3::new(2.0, 8.0, 2.0),
                Vec3::new(8.0, 12.0, 2.0),
                Vec3::new(2.0, 8.0, 8.0),
                Vec3::new(8.0, 12.0, 8.0),
            ],
        );

        assert!(support_points.len() >= 8);
        assert!(support_points.iter().all(|point| point.y.is_finite()));
    }

    #[test]
    fn samples_interior_support_points_inside_boundary() {
        let support_points = sample_interior_support_points(
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(12.0, 0.0),
                Vec2::new(12.0, 12.0),
                Vec2::new(0.0, 12.0),
            ],
            4.0,
            &[
                Vec3::new(0.0, 5.0, 0.0),
                Vec3::new(12.0, 7.0, 0.0),
                Vec3::new(0.0, 6.0, 12.0),
                Vec3::new(12.0, 8.0, 12.0),
            ],
        );

        assert!(!support_points.is_empty());
        assert!(support_points.iter().all(|point| {
            point.x > 0.0
                && point.x < 12.0
                && point.z > 0.0
                && point.z < 12.0
                && point.y.is_finite()
        }));
    }

    #[test]
    fn estimates_boundary_without_falling_back_to_bounding_box() {
        let curves = vec![
            curve(&[[0.0, 10.0, 0.0], [20.0, 10.0, 0.0]], 10.0),
            curve(&[[0.0, 10.0, 10.0], [8.0, 10.0, 10.0]], 10.0),
            curve(&[[0.0, 10.0, 20.0], [20.0, 10.0, 20.0]], 10.0),
        ];
        let boundary = estimate_terrain_boundary(&curves, 2.0);

        assert!(boundary.len() >= 3);
        let max_x = boundary
            .iter()
            .map(|point| point.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let min_x = boundary
            .iter()
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min);
        let max_y = boundary
            .iter()
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let min_y = boundary
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        assert!(min_x <= 0.0);
        assert!(max_x >= 10.0);
        assert!(min_y <= 0.0);
        assert!(max_y >= 10.0);
        assert!(boundary
            .iter()
            .all(|point| !(point.x > 10.0 && point.y > 10.0)));
    }
}
