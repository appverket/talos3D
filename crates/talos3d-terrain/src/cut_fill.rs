use bevy::prelude::Vec2;
use talos3d_core::plugins::modeling::primitives::TriangleMesh;

use crate::generation::sample_surface_elevation;

const MIN_SAMPLE_SPACING: f32 = 1.0e-4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutFillResult {
    pub cut_volume: f64,
    pub fill_volume: f64,
    pub net_volume: f64,
    pub sample_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CutFillOptions {
    pub boundary: Vec<Vec2>,
    pub sample_spacing: f32,
}

impl CutFillOptions {
    pub fn new(sample_spacing: f32) -> Self {
        Self {
            boundary: Vec::new(),
            sample_spacing,
        }
    }

    pub fn with_boundary(mut self, boundary: Vec<Vec2>) -> Self {
        self.boundary = boundary;
        self
    }
}

pub fn cut_fill_between_surfaces(
    existing: &TriangleMesh,
    proposed: &TriangleMesh,
    options: &CutFillOptions,
) -> Option<CutFillResult> {
    analyze_cut_fill(existing, options, |x, z| {
        sample_surface_elevation(proposed, x, z)
    })
}

pub fn cut_fill_against_datum(
    existing: &TriangleMesh,
    datum_y: f32,
    options: &CutFillOptions,
) -> Option<CutFillResult> {
    analyze_cut_fill(existing, options, |_, _| Some(datum_y))
}

fn analyze_cut_fill(
    existing: &TriangleMesh,
    options: &CutFillOptions,
    mut proposed_elevation: impl FnMut(f32, f32) -> Option<f32>,
) -> Option<CutFillResult> {
    let spacing = options.sample_spacing.max(MIN_SAMPLE_SPACING);
    let (min, max) = analysis_bounds(existing, &options.boundary)?;
    if min.x >= max.x || min.y >= max.y {
        return None;
    }

    let cell_area = f64::from(spacing) * f64::from(spacing);
    let mut cut_volume = 0.0;
    let mut fill_volume = 0.0;
    let mut sample_count = 0;
    let mut x = min.x + spacing * 0.5;
    while x <= max.x {
        let mut z = min.y + spacing * 0.5;
        while z <= max.y {
            let point = Vec2::new(x, z);
            if options.boundary.len() >= 3 && !point_in_polygon(point, &options.boundary) {
                z += spacing;
                continue;
            }

            let Some(existing_y) = sample_surface_elevation(existing, x, z) else {
                z += spacing;
                continue;
            };
            let Some(proposed_y) = proposed_elevation(x, z) else {
                z += spacing;
                continue;
            };

            let delta = f64::from(proposed_y - existing_y);
            if delta < 0.0 {
                cut_volume += -delta * cell_area;
            } else {
                fill_volume += delta * cell_area;
            }
            sample_count += 1;
            z += spacing;
        }
        x += spacing;
    }

    (sample_count > 0).then_some(CutFillResult {
        cut_volume,
        fill_volume,
        net_volume: cut_volume - fill_volume,
        sample_count,
    })
}

fn analysis_bounds(mesh: &TriangleMesh, boundary: &[Vec2]) -> Option<(Vec2, Vec2)> {
    if boundary.len() >= 3 {
        return polygon_bounds(boundary);
    }

    let mut vertices = mesh.vertices.iter();
    let first = vertices.next()?;
    let mut min = Vec2::new(first.x, first.z);
    let mut max = min;
    for vertex in vertices {
        min.x = min.x.min(vertex.x);
        min.y = min.y.min(vertex.z);
        max.x = max.x.max(vertex.x);
        max.y = max.y.max(vertex.z);
    }
    Some((min, max))
}

fn polygon_bounds(polygon: &[Vec2]) -> Option<(Vec2, Vec2)> {
    let mut points = polygon.iter().copied();
    let first = points.next()?;
    let mut min = first;
    let mut max = first;
    for point in points {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
    }
    Some((min, max))
}

fn point_in_polygon(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let pi = polygon[i];
        let pj = polygon[j];
        let crosses = (pi.y > point.y) != (pj.y > point.y)
            && point.x < (pj.x - pi.x) * (point.y - pi.y) / (pj.y - pi.y) + pi.x;
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use bevy::prelude::Vec3;

    use super::*;

    fn flat_square(elevation: f32) -> TriangleMesh {
        TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, elevation, 0.0),
                Vec3::new(2.0, elevation, 0.0),
                Vec3::new(2.0, elevation, 2.0),
                Vec3::new(0.0, elevation, 2.0),
            ],
            faces: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
            name: None,
        }
    }

    fn sloped_square(base: f32, x_slope: f32) -> TriangleMesh {
        TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, base, 0.0),
                Vec3::new(2.0, base + x_slope * 2.0, 0.0),
                Vec3::new(2.0, base + x_slope * 2.0, 2.0),
                Vec3::new(0.0, base, 2.0),
            ],
            faces: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
            name: None,
        }
    }

    #[test]
    fn computes_cut_when_proposed_surface_is_lower() {
        let existing = flat_square(2.0);
        let proposed = flat_square(1.5);
        let result =
            cut_fill_between_surfaces(&existing, &proposed, &CutFillOptions::new(1.0)).unwrap();

        assert_eq!(result.sample_count, 4);
        assert!((result.cut_volume - 2.0).abs() < 1.0e-6);
        assert_eq!(result.fill_volume, 0.0);
        assert!((result.net_volume - 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn computes_fill_when_proposed_surface_is_higher() {
        let existing = flat_square(2.0);
        let proposed = flat_square(2.25);
        let result =
            cut_fill_between_surfaces(&existing, &proposed, &CutFillOptions::new(0.5)).unwrap();

        assert_eq!(result.sample_count, 16);
        assert_eq!(result.cut_volume, 0.0);
        assert!((result.fill_volume - 1.0).abs() < 1.0e-6);
        assert!((result.net_volume + 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn computes_mixed_cut_and_fill_against_datum() {
        let existing = sloped_square(0.0, 1.0);
        let result = cut_fill_against_datum(&existing, 1.0, &CutFillOptions::new(0.5)).unwrap();

        assert_eq!(result.sample_count, 16);
        assert!((result.cut_volume - 1.0).abs() < 1.0e-6);
        assert!((result.fill_volume - 1.0).abs() < 1.0e-6);
        assert!(result.net_volume.abs() < 1.0e-6);
    }

    #[test]
    fn clips_analysis_to_boundary_polygon() {
        let existing = flat_square(2.0);
        let proposed = flat_square(1.0);
        let boundary = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 2.0),
            Vec2::new(0.0, 2.0),
        ];
        let options = CutFillOptions::new(1.0).with_boundary(boundary);
        let result = cut_fill_between_surfaces(&existing, &proposed, &options).unwrap();

        assert_eq!(result.sample_count, 2);
        assert!((result.cut_volume - 2.0).abs() < 1.0e-6);
        assert_eq!(result.fill_volume, 0.0);
    }

    #[test]
    fn returns_none_when_no_samples_overlap() {
        let existing = flat_square(2.0);
        let proposed = flat_square(1.0);
        let boundary = vec![
            Vec2::new(5.0, 5.0),
            Vec2::new(6.0, 5.0),
            Vec2::new(6.0, 6.0),
            Vec2::new(5.0, 6.0),
        ];
        let options = CutFillOptions::new(1.0).with_boundary(boundary);

        assert_eq!(
            cut_fill_between_surfaces(&existing, &proposed, &options),
            None
        );
    }
}
