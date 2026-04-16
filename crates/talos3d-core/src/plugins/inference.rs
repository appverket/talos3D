use bevy::prelude::*;

#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use crate::{
    capability_registry::CapabilityRegistry,
    plugins::{
        face_edit::PushPullContext,
        identity::ElementId,
        snap::SnapResult,
        transform::{TransformMode, TransformState},
    },
};

// --- Colors ---
const PARALLEL_COLOR: Color = Color::srgb(0.9, 0.3, 0.9); // magenta
const PERPENDICULAR_COLOR: Color = Color::srgb(0.3, 0.9, 0.9); // cyan
const ON_FACE_COLOR: Color = Color::srgb(0.9, 0.9, 0.3); // yellow
const DISTANCE_MATCH_COLOR: Color = Color::srgb(0.3, 0.9, 0.3); // green
const ENDPOINT_COLOR: Color = Color::srgb(0.3, 0.9, 0.9); // cyan
const _MIDPOINT_COLOR: Color = Color::srgb(0.6, 0.9, 0.3);
const _CENTER_COLOR: Color = Color::srgb(0.9, 0.6, 0.3);

// --- Thresholds ---
const PARALLEL_ENTER_ANGLE: f32 = 15.0_f32; // degrees
const PARALLEL_EXIT_ANGLE: f32 = 25.0_f32; // degrees
const FACE_PLANE_TOLERANCE: f32 = 0.05; // metres
const DISTANCE_MATCH_TOLERANCE: f32 = 0.02; // 2% relative
const INFERENCE_LINE_LENGTH: f32 = 20.0;
const MAX_INFERENCE_CANDIDATES: usize = 2;

pub struct InferencePlugin;

impl Plugin for InferencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InferenceEngine>()
            .add_systems(Update, (update_inference, draw_inference_visuals));
    }
}

#[derive(Debug, Clone)]
pub struct InferenceCandidate {
    pub kind: InferenceKind,
    pub direction: Option<Vec3>,
    pub distance: Option<f32>,
    pub snap_position: Option<Vec3>,
    pub reference_entity: Option<ElementId>,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceKind {
    Parallel,
    Perpendicular,
    OnFacePlane,
    DistanceMatch,
    Endpoint,
}

impl InferenceKind {
    fn priority(&self) -> u8 {
        match self {
            Self::OnFacePlane => 0,
            Self::Parallel | Self::Perpendicular => 1,
            Self::DistanceMatch => 2,
            Self::Endpoint => 3,
        }
    }
}

/// Reference edge from the scene for parallel/perpendicular inference.
#[derive(Debug, Clone)]
pub struct ReferenceEdge {
    pub start: Vec3,
    pub end: Vec3,
    pub entity_label: String,
    pub element_id: ElementId,
}

/// Reference face plane from the scene for on-face-plane inference.
#[derive(Debug, Clone)]
pub struct ReferenceFacePlane {
    pub point_on_plane: Vec3,
    pub normal: Vec3,
    pub entity_label: String,
    pub element_id: ElementId,
}

#[derive(Resource, Default)]
pub struct InferenceEngine {
    /// Currently active inference candidates, sorted by priority.
    pub candidates: Vec<InferenceCandidate>,
    /// The locked inference, if any (locked by pressing Shift during an active inference).
    pub locked: Option<InferenceCandidate>,
    /// Last committed distance for Tab-repeat.
    pub last_distance: Option<f32>,
    /// Last committed transform mode (for Tab-repeat matching).
    pub last_mode: Option<TransformMode>,
    /// Cached reference edges from the scene (invalidated when entities change).
    cached_edges: Vec<ReferenceEdge>,
    /// Cached face planes from the scene.
    cached_face_planes: Vec<ReferenceFacePlane>,
    /// Cached reference distances from the scene.
    cached_distances: Vec<(f32, String)>,
    /// Tracks active parallel/perpendicular inference for hysteresis.
    active_edge_inference: Option<(usize, InferenceKind)>,
}

impl InferenceEngine {
    /// Get the best inference snap position, if any.
    pub fn best_snap_position(&self) -> Option<Vec3> {
        if let Some(locked) = &self.locked {
            return locked.snap_position;
        }
        self.candidates.first().and_then(|c| c.snap_position)
    }

    /// Get the best inference label for the status bar.
    pub fn best_label(&self) -> Option<&str> {
        if let Some(locked) = &self.locked {
            return Some(&locked.label);
        }
        self.candidates.first().map(|c| c.label.as_str())
    }

    pub fn add_reference_edge(&mut self, edge: ReferenceEdge) {
        self.cached_edges.push(edge);
    }

    pub fn add_reference_face_plane(&mut self, plane: ReferenceFacePlane) {
        self.cached_face_planes.push(plane);
    }

    pub fn add_reference_distance(&mut self, distance: f32, label: String) {
        self.cached_distances.push((distance, label));
    }

    #[cfg(test)]
    pub(crate) fn reference_edges(&self) -> &[ReferenceEdge] {
        &self.cached_edges
    }
}

fn update_inference(world: &mut World) {
    let transform_state = world.resource::<TransformState>().clone();
    if transform_state.is_idle() {
        let mut engine = world.resource_mut::<InferenceEngine>();
        engine.candidates.clear();
        engine.active_edge_inference = None;
        return;
    }

    // Only run inference during move and push/pull for now
    if transform_state.mode != TransformMode::Moving {
        world.resource_mut::<InferenceEngine>().candidates.clear();
        return;
    }

    let snap_result = world.resource::<SnapResult>().clone();
    let Some(raw_position) = snap_result.raw_position else {
        world.resource_mut::<InferenceEngine>().candidates.clear();
        return;
    };

    let Some(initial_cursor) = transform_state.initial_cursor else {
        return;
    };

    // Rebuild reference geometry cache each frame (could optimize with dirty tracking)
    {
        let mut engine = world.resource_mut::<InferenceEngine>();
        engine.cached_edges.clear();
        engine.cached_face_planes.clear();
        engine.cached_distances.clear();
    }
    let factories: Vec<_> = world.resource::<CapabilityRegistry>().factories().to_vec();
    // Collect reference geometry from all factories into temporaries (borrow rules)
    let mut edges = Vec::new();
    let mut face_planes = Vec::new();
    let mut distances = Vec::new();
    for factory in &factories {
        let mut temp = InferenceEngine::default();
        factory.collect_inference_geometry(world, &mut temp);
        edges.extend(temp.cached_edges);
        face_planes.extend(temp.cached_face_planes);
        distances.extend(temp.cached_distances);
    }
    {
        let mut engine = world.resource_mut::<InferenceEngine>();
        engine.cached_edges = edges;
        engine.cached_face_planes = face_planes;
        engine.cached_distances = distances;
    }

    let drag_delta = raw_position - initial_cursor;
    let drag_distance = drag_delta.length();
    let push_pull_ctx = world.resource::<PushPullContext>().clone();

    // Collect candidates
    let mut candidates = Vec::new();

    // --- Edge parallel/perpendicular inference ---
    if drag_distance > 0.1 {
        let drag_dir = drag_delta.normalize();
        let engine = world.resource::<InferenceEngine>();
        let active_edge = engine.active_edge_inference;

        for (idx, edge) in engine.cached_edges.iter().enumerate() {
            let edge_dir = (edge.end - edge.start).normalize();
            if edge_dir.length_squared() < 0.01 {
                continue;
            }

            let dot = drag_dir.dot(edge_dir).abs();
            let angle_deg = dot.acos().to_degrees();

            // Check if this was the previously active inference (hysteresis)
            let was_active_parallel = active_edge
                .map(|(i, k)| i == idx && k == InferenceKind::Parallel)
                .unwrap_or(false);
            let was_active_perp = active_edge
                .map(|(i, k)| i == idx && k == InferenceKind::Perpendicular)
                .unwrap_or(false);

            let enter_threshold = PARALLEL_ENTER_ANGLE;
            let exit_threshold = PARALLEL_EXIT_ANGLE;

            // Parallel: angle near 0°
            let is_parallel = if was_active_parallel {
                angle_deg < exit_threshold
            } else {
                angle_deg < enter_threshold
            };

            // Perpendicular: angle near 90°
            let perp_angle = (90.0 - angle_deg).abs();
            let is_perp = if was_active_perp {
                perp_angle < exit_threshold
            } else {
                perp_angle < enter_threshold
            };

            if is_parallel {
                candidates.push(InferenceCandidate {
                    kind: InferenceKind::Parallel,
                    direction: Some(edge_dir),
                    distance: None,
                    snap_position: None,
                    reference_entity: Some(edge.element_id),
                    label: format!("Parallel to {}", edge.entity_label),
                });
            } else if is_perp {
                candidates.push(InferenceCandidate {
                    kind: InferenceKind::Perpendicular,
                    direction: Some(edge_dir),
                    distance: None,
                    snap_position: None,
                    reference_entity: Some(edge.element_id),
                    label: format!("Perpendicular to {}", edge.entity_label),
                });
            }
        }
    }

    // --- Face-plane inference (during push/pull) ---
    if let Some(pp) = &push_pull_ctx.active_face {
        let distance = drag_delta.dot(pp.normal);
        let engine = world.resource::<InferenceEngine>();
        let initial_snapshots = &transform_state.initial_snapshots;

        // Find the initial centroid of the face being pushed
        if let Some((_, snapshot)) = initial_snapshots.first() {
            let face_centroid = snapshot.center() + pp.normal * 0.0; // approximate
            let pushed_plane_point = face_centroid + pp.normal * distance;

            for plane in &engine.cached_face_planes {
                // Skip self
                if Some(plane.element_id) == Some(pp.element_id) {
                    continue;
                }

                // Check if the pushed face would lie on this reference plane
                let to_plane = plane.point_on_plane - pushed_plane_point;
                let plane_distance = to_plane.dot(plane.normal).abs();

                if plane_distance < FACE_PLANE_TOLERANCE {
                    // Compute the exact snap distance
                    let exact_dist = (plane.point_on_plane - face_centroid).dot(pp.normal);
                    let snap_pos = initial_cursor + pp.normal * exact_dist;
                    candidates.push(InferenceCandidate {
                        kind: InferenceKind::OnFacePlane,
                        direction: Some(plane.normal),
                        distance: Some(exact_dist),
                        snap_position: Some(snap_pos),
                        reference_entity: Some(plane.element_id),
                        label: format!("On face of {}", plane.entity_label),
                    });
                }
            }
        }
    }

    // --- Distance inference ---
    if drag_distance > 0.1 {
        let engine = world.resource::<InferenceEngine>();
        for (ref_dist, ref_label) in &engine.cached_distances {
            let ratio = (drag_distance / ref_dist - 1.0).abs();
            if ratio < DISTANCE_MATCH_TOLERANCE {
                candidates.push(InferenceCandidate {
                    kind: InferenceKind::DistanceMatch,
                    direction: None,
                    distance: Some(*ref_dist),
                    snap_position: None,
                    reference_entity: None,
                    label: format!("= {} ({:.2}m)", ref_label, ref_dist),
                });
            }
        }
    }

    // Sort by priority and truncate
    candidates.sort_by_key(|c| c.kind.priority());
    candidates.truncate(MAX_INFERENCE_CANDIDATES);

    // Update hysteresis tracking
    let new_active_edge = candidates.first().and_then(|c| match c.kind {
        InferenceKind::Parallel | InferenceKind::Perpendicular => {
            // Find the edge index that matches this candidate's reference entity
            let engine = world.resource::<InferenceEngine>();
            engine
                .cached_edges
                .iter()
                .position(|e| Some(e.element_id) == c.reference_entity)
                .map(|idx| (idx, c.kind))
        }
        _ => None,
    });

    let mut engine = world.resource_mut::<InferenceEngine>();
    engine.candidates = candidates;
    engine.active_edge_inference = new_active_edge;

    // Handle Tab for distance echo
    let keys = world.resource::<ButtonInput<KeyCode>>();
    if keys.just_pressed(KeyCode::Tab) {
        let engine = world.resource::<InferenceEngine>();
        if let Some(last_dist) = engine.last_distance {
            if let Some(last_mode) = engine.last_mode {
                if last_mode == transform_state.mode {
                    world.resource_mut::<TransformState>().numeric_buffer =
                        Some(format!("{:.3}", last_dist));
                }
            }
        }
    }

    // Handle Shift for inference lock
    let keys = world.resource::<ButtonInput<KeyCode>>();
    if keys.just_pressed(KeyCode::ShiftLeft) || keys.just_pressed(KeyCode::ShiftRight) {
        let mut engine = world.resource_mut::<InferenceEngine>();
        if engine.locked.is_some() {
            // Unlock
            engine.locked = None;
        } else if let Some(candidate) = engine.candidates.first().cloned() {
            // Lock the current best candidate
            engine.locked = Some(candidate);
        }
    }
}

fn draw_inference_visuals(
    inference: Res<InferenceEngine>,
    transform_state: Res<TransformState>,
    snap_result: Res<SnapResult>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    if transform_state.is_idle() {
        return;
    }

    let Some(cursor_pos) = snap_result.raw_position else {
        return;
    };
    let Some(initial_cursor) = transform_state.initial_cursor else {
        return;
    };

    let candidates_to_draw: Vec<&InferenceCandidate> = if let Some(locked) = &inference.locked {
        vec![locked]
    } else {
        inference.candidates.iter().collect()
    };

    for candidate in candidates_to_draw {
        let is_locked = inference.locked.is_some();
        match candidate.kind {
            InferenceKind::Parallel => {
                let dir = candidate.direction.unwrap_or(Vec3::X);
                draw_inference_line(&mut gizmos, cursor_pos, dir, PARALLEL_COLOR, is_locked);
                #[cfg(feature = "perf-stats")]
                add_gizmo_line_count(&mut perf_stats, 8);
            }
            InferenceKind::Perpendicular => {
                let dir = candidate.direction.unwrap_or(Vec3::X);
                draw_inference_line(&mut gizmos, cursor_pos, dir, PERPENDICULAR_COLOR, is_locked);
                #[cfg(feature = "perf-stats")]
                add_gizmo_line_count(&mut perf_stats, 8);
            }
            InferenceKind::OnFacePlane => {
                if let Some(snap_pos) = candidate.snap_position {
                    // Draw a small cross at the snap position
                    let size = 0.15;
                    gizmos.line(
                        snap_pos - Vec3::X * size,
                        snap_pos + Vec3::X * size,
                        ON_FACE_COLOR,
                    );
                    gizmos.line(
                        snap_pos - Vec3::Z * size,
                        snap_pos + Vec3::Z * size,
                        ON_FACE_COLOR,
                    );
                    #[cfg(feature = "perf-stats")]
                    add_gizmo_line_count(&mut perf_stats, 2);
                }
            }
            InferenceKind::DistanceMatch => {
                if let Some(distance) = candidate.distance {
                    let drag_delta = cursor_pos - initial_cursor;
                    let drag_dir = if drag_delta.length() > 0.001 {
                        drag_delta.normalize()
                    } else {
                        Vec3::X
                    };
                    let match_pos = initial_cursor + drag_dir * distance;
                    // Draw dimension line
                    gizmos.line(initial_cursor, match_pos, DISTANCE_MATCH_COLOR);
                    // Tick marks at both ends
                    let perp = Vec3::Y * 0.1;
                    gizmos.line(
                        initial_cursor - perp,
                        initial_cursor + perp,
                        DISTANCE_MATCH_COLOR,
                    );
                    gizmos.line(match_pos - perp, match_pos + perp, DISTANCE_MATCH_COLOR);
                    #[cfg(feature = "perf-stats")]
                    add_gizmo_line_count(&mut perf_stats, 3);
                }
            }
            InferenceKind::Endpoint => {
                if let Some(snap_pos) = candidate.snap_position {
                    gizmos
                        .sphere(Isometry3d::from_translation(snap_pos), 0.06, ENDPOINT_COLOR)
                        .resolution(6);
                    #[cfg(feature = "perf-stats")]
                    add_gizmo_line_count(&mut perf_stats, 1);
                }
            }
        }
    }
}

/// Draw a dashed inference line through a point along a direction.
fn draw_inference_line(
    gizmos: &mut Gizmos,
    through: Vec3,
    direction: Vec3,
    color: Color,
    solid: bool,
) {
    let half_len = INFERENCE_LINE_LENGTH * 0.5;
    if solid {
        gizmos.line(
            through - direction * half_len,
            through + direction * half_len,
            color,
        );
    } else {
        // Dashed: 8 segments
        let segments = 8;
        let seg_len = INFERENCE_LINE_LENGTH / (segments as f32 * 2.0);
        let start = through - direction * half_len;
        for i in 0..segments {
            let t0 = (i as f32 * 2.0) * seg_len;
            let t1 = t0 + seg_len;
            gizmos.line(start + direction * t0, start + direction * t1, color);
        }
    }
}
