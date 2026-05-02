//! Geometry health checks for evaluated authored geometry.
//!
//! This module is the first ADR-048 substrate slice. It does not try to be a
//! full B-rep validator yet; it provides deterministic checks that can reject
//! rendered output with obvious CAE failures such as duplicate coplanar faces
//! before visual QA has to catch z-fighting.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::primitives::TriangleMesh;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeometryHealthOptions {
    pub distance_tolerance: f32,
    pub normal_dot_tolerance: f32,
    pub area_tolerance: f32,
}

impl Default for GeometryHealthOptions {
    fn default() -> Self {
        Self {
            distance_tolerance: 1e-5,
            normal_dot_tolerance: 1e-4,
            area_tolerance: 1e-6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeometryHealthIssueKind {
    DuplicateCoplanarFaces,
    OpenEdges,
    NonmanifoldEdges,
    ZeroAreaFaces,
    SliverFaces,
    SelfIntersections,
    CoincidentShells,
    OverlappingVisibleBodies,
    UnownedVisibleFaces,
    SelectableInternalEntities,
    HostedFillOutsideOpening,
    HostFeatureOutsideHostDomain,
    InvalidFaceReference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryHealthIssue {
    pub kind: GeometryHealthIssueKind,
    pub message: String,
    pub face_indices: Vec<usize>,
    pub measured_area: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GeometryHealthReport {
    pub issues: Vec<GeometryHealthIssue>,
}

impl GeometryHealthReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn issues_by_kind(
        &self,
        kind: GeometryHealthIssueKind,
    ) -> impl Iterator<Item = &GeometryHealthIssue> {
        self.issues.iter().filter(move |issue| issue.kind == kind)
    }

    fn push(&mut self, issue: GeometryHealthIssue) {
        self.issues.push(issue);
    }
}

pub fn check_triangle_mesh_health(mesh: &TriangleMesh) -> GeometryHealthReport {
    check_triangle_mesh_health_with_options(mesh, GeometryHealthOptions::default())
}

pub fn check_triangle_mesh_health_with_options(
    mesh: &TriangleMesh,
    options: GeometryHealthOptions,
) -> GeometryHealthReport {
    let mut report = GeometryHealthReport::default();
    let mut triangles = Vec::new();

    for (face_index, face) in mesh.faces.iter().enumerate() {
        let [a, b, c] = *face;
        let Some(a) = mesh.vertices.get(a as usize).copied() else {
            report.push(invalid_face_reference(face_index));
            continue;
        };
        let Some(b) = mesh.vertices.get(b as usize).copied() else {
            report.push(invalid_face_reference(face_index));
            continue;
        };
        let Some(c) = mesh.vertices.get(c as usize).copied() else {
            report.push(invalid_face_reference(face_index));
            continue;
        };

        let edge_ab = b - a;
        let edge_ac = c - a;
        let normal_cross = edge_ab.cross(edge_ac);
        let doubled_area = normal_cross.length();
        if doubled_area <= options.area_tolerance {
            report.push(GeometryHealthIssue {
                kind: GeometryHealthIssueKind::ZeroAreaFaces,
                message: format!("Face {face_index} has near-zero area"),
                face_indices: vec![face_index],
                measured_area: Some(doubled_area * 0.5),
            });
            continue;
        }

        triangles.push(HealthTriangle {
            face_index,
            vertices: [a, b, c],
            normal: normal_cross / doubled_area,
            area: doubled_area * 0.5,
        });
    }

    for left_index in 0..triangles.len() {
        for right_index in left_index + 1..triangles.len() {
            let left = &triangles[left_index];
            let right = &triangles[right_index];
            if !are_coplanar(left, right, options) {
                continue;
            }

            let overlap_area = coplanar_overlap_area(left, right);
            if overlap_area > options.area_tolerance {
                report.push(GeometryHealthIssue {
                    kind: GeometryHealthIssueKind::DuplicateCoplanarFaces,
                    message: format!(
                        "Faces {} and {} overlap on the same plane",
                        left.face_index, right.face_index
                    ),
                    face_indices: vec![left.face_index, right.face_index],
                    measured_area: Some(overlap_area.min(left.area).min(right.area)),
                });
            }
        }
    }

    report
}

#[derive(Debug, Clone)]
struct HealthTriangle {
    face_index: usize,
    vertices: [Vec3; 3],
    normal: Vec3,
    area: f32,
}

fn invalid_face_reference(face_index: usize) -> GeometryHealthIssue {
    GeometryHealthIssue {
        kind: GeometryHealthIssueKind::InvalidFaceReference,
        message: format!("Face {face_index} references a missing vertex"),
        face_indices: vec![face_index],
        measured_area: None,
    }
}

fn are_coplanar(
    left: &HealthTriangle,
    right: &HealthTriangle,
    options: GeometryHealthOptions,
) -> bool {
    if left.normal.dot(right.normal).abs() < 1.0 - options.normal_dot_tolerance {
        return false;
    }

    let plane_point = left.vertices[0];
    right
        .vertices
        .iter()
        .all(|vertex| left.normal.dot(*vertex - plane_point).abs() <= options.distance_tolerance)
}

fn coplanar_overlap_area(left: &HealthTriangle, right: &HealthTriangle) -> f32 {
    let axis = dominant_axis(left.normal);
    let left_2d = project_triangle(left, axis);
    let right_2d = ensure_ccw(project_triangle(right, axis));
    let mut subject = ensure_ccw(left_2d).to_vec();

    for clip_index in 0..3 {
        let clip_a = right_2d[clip_index];
        let clip_b = right_2d[(clip_index + 1) % 3];
        subject = clip_polygon_against_edge(&subject, clip_a, clip_b);
        if subject.is_empty() {
            return 0.0;
        }
    }

    polygon_area(&subject).abs()
}

fn dominant_axis(normal: Vec3) -> usize {
    let abs = normal.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        0
    } else if abs.y >= abs.z {
        1
    } else {
        2
    }
}

fn project_triangle(triangle: &HealthTriangle, drop_axis: usize) -> [Vec2; 3] {
    triangle.vertices.map(|vertex| match drop_axis {
        0 => Vec2::new(vertex.y, vertex.z),
        1 => Vec2::new(vertex.x, vertex.z),
        _ => Vec2::new(vertex.x, vertex.y),
    })
}

fn ensure_ccw(points: [Vec2; 3]) -> [Vec2; 3] {
    if signed_triangle_area(points) >= 0.0 {
        points
    } else {
        [points[0], points[2], points[1]]
    }
}

fn signed_triangle_area(points: [Vec2; 3]) -> f32 {
    ((points[1] - points[0]).perp_dot(points[2] - points[0])) * 0.5
}

fn polygon_area(points: &[Vec2]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }

    let mut area = 0.0;
    for index in 0..points.len() {
        let next = (index + 1) % points.len();
        area += points[index].perp_dot(points[next]);
    }
    area * 0.5
}

fn clip_polygon_against_edge(subject: &[Vec2], clip_a: Vec2, clip_b: Vec2) -> Vec<Vec2> {
    if subject.is_empty() {
        return Vec::new();
    }

    let mut output = Vec::new();
    let mut previous = *subject.last().unwrap();
    let mut previous_inside = is_inside_clip_edge(previous, clip_a, clip_b);

    for &current in subject {
        let current_inside = is_inside_clip_edge(current, clip_a, clip_b);
        if current_inside {
            if !previous_inside {
                output.push(line_intersection(previous, current, clip_a, clip_b));
            }
            output.push(current);
        } else if previous_inside {
            output.push(line_intersection(previous, current, clip_a, clip_b));
        }
        previous = current;
        previous_inside = current_inside;
    }

    output
}

fn is_inside_clip_edge(point: Vec2, clip_a: Vec2, clip_b: Vec2) -> bool {
    (clip_b - clip_a).perp_dot(point - clip_a) >= -1e-7
}

fn line_intersection(line_a: Vec2, line_b: Vec2, clip_a: Vec2, clip_b: Vec2) -> Vec2 {
    let line_direction = line_b - line_a;
    let clip_direction = clip_b - clip_a;
    let denominator = line_direction.perp_dot(clip_direction);
    if denominator.abs() <= f32::EPSILON {
        return line_a;
    }

    let t = (clip_a - line_a).perp_dot(clip_direction) / denominator;
    line_a + line_direction * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(vertices: Vec<Vec3>, faces: Vec<[u32; 3]>) -> TriangleMesh {
        TriangleMesh {
            vertices,
            faces,
            normals: None,
            name: None,
        }
    }

    #[test]
    fn adjacent_coplanar_triangles_do_not_report_duplicate_faces() {
        let mesh = mesh(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );

        let report = check_triangle_mesh_health(&mesh);

        assert!(report.is_clean(), "{report:#?}");
    }

    #[test]
    fn duplicate_coplanar_triangle_is_reported_even_with_opposite_winding() {
        let mesh = mesh(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            vec![[0, 1, 2], [0, 2, 1]],
        );

        let report = check_triangle_mesh_health(&mesh);
        let issues: Vec<_> = report
            .issues_by_kind(GeometryHealthIssueKind::DuplicateCoplanarFaces)
            .collect();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].face_indices, vec![0, 1]);
    }

    #[test]
    fn partially_overlapping_coplanar_triangles_are_reported() {
        let mesh = mesh(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(0.0, 2.0, 0.0),
                Vec3::new(0.5, 0.5, 0.0),
                Vec3::new(2.5, 0.5, 0.0),
                Vec3::new(0.5, 2.5, 0.0),
            ],
            vec![[0, 1, 2], [3, 4, 5]],
        );

        let report = check_triangle_mesh_health(&mesh);
        let issues: Vec<_> = report
            .issues_by_kind(GeometryHealthIssueKind::DuplicateCoplanarFaces)
            .collect();

        assert_eq!(issues.len(), 1, "{report:#?}");
        assert!(issues[0].measured_area.unwrap_or_default() > 0.0);
    }

    #[test]
    fn zero_area_and_invalid_faces_are_reported() {
        let mesh = mesh(
            vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)],
            vec![[0, 0, 1], [0, 1, 7]],
        );

        let report = check_triangle_mesh_health(&mesh);

        assert_eq!(
            report
                .issues_by_kind(GeometryHealthIssueKind::ZeroAreaFaces)
                .count(),
            1
        );
        assert_eq!(
            report
                .issues_by_kind(GeometryHealthIssueKind::InvalidFaceReference)
                .count(),
            1
        );
    }
}
