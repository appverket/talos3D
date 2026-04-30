use bevy::prelude::{Resource, Vec2, Vec3};
use talos3d_core::plugins::{identity::ElementId, modeling::primitives::TriangleMesh};

const FLAT_SLOPE_COLOR: [f32; 4] = [0.16, 0.62, 0.24, 1.0];
const MODERATE_SLOPE_COLOR: [f32; 4] = [0.94, 0.78, 0.18, 1.0];
const STEEP_SLOPE_COLOR: [f32; 4] = [0.88, 0.22, 0.12, 1.0];
const ASPECT_NORTH_COLOR: [f32; 4] = [0.12, 0.38, 0.92, 1.0];
const ASPECT_EAST_COLOR: [f32; 4] = [0.18, 0.66, 0.32, 1.0];
const ASPECT_SOUTH_COLOR: [f32; 4] = [0.86, 0.18, 0.16, 1.0];
const ASPECT_WEST_COLOR: [f32; 4] = [0.94, 0.78, 0.16, 1.0];
const ELEVATION_LOW_COLOR: [f32; 4] = [0.14, 0.36, 0.82, 1.0];
const ELEVATION_HIGH_COLOR: [f32; 4] = [0.92, 0.92, 0.86, 1.0];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriangleVisualization {
    pub face: [u32; 3],
    pub value: f32,
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerrainVisualizationMode {
    #[default]
    Standard,
    Slope,
    Aspect,
    ElevationBands,
    CutFill,
}

impl TerrainVisualizationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Slope => "slope",
            Self::Aspect => "aspect",
            Self::ElevationBands => "elevation_bands",
            Self::CutFill => "cut_fill",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Slope => "Slope",
            Self::Aspect => "Aspect",
            Self::ElevationBands => "Elevation bands",
            Self::CutFill => "Cut/fill",
        }
    }
}

#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct TerrainVisualizationState {
    pub mode: TerrainVisualizationMode,
    pub elevation_band_width: f32,
}

impl Default for TerrainVisualizationState {
    fn default() -> Self {
        Self {
            mode: TerrainVisualizationMode::Standard,
            elevation_band_width: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CutFillVisualizationTarget {
    ProposedSurface(ElementId),
    Datum(f32),
}

#[derive(Resource, Debug, Clone, PartialEq)]
pub struct CutFillVisualizationState {
    pub existing_surface_id: ElementId,
    pub target: CutFillVisualizationTarget,
    pub boundary: Vec<Vec2>,
}

pub fn visualization_for_mode(
    mesh: &TriangleMesh,
    state: TerrainVisualizationState,
) -> Vec<TriangleVisualization> {
    match state.mode {
        TerrainVisualizationMode::Standard => Vec::new(),
        TerrainVisualizationMode::Slope => slope_visualization(mesh),
        TerrainVisualizationMode::Aspect => aspect_visualization(mesh),
        TerrainVisualizationMode::ElevationBands => {
            elevation_band_visualization(mesh, state.elevation_band_width)
        }
        TerrainVisualizationMode::CutFill => Vec::new(),
    }
}

pub fn slope_visualization(mesh: &TriangleMesh) -> Vec<TriangleVisualization> {
    mesh.faces
        .iter()
        .filter_map(|face| {
            let (a, b, c) = face_points(mesh, *face)?;
            let slope_degrees = triangle_slope_degrees(a, b, c);
            Some(TriangleVisualization {
                face: *face,
                value: slope_degrees,
                color: slope_color(slope_degrees),
            })
        })
        .collect()
}

pub fn aspect_visualization(mesh: &TriangleMesh) -> Vec<TriangleVisualization> {
    mesh.faces
        .iter()
        .filter_map(|face| {
            let (a, b, c) = face_points(mesh, *face)?;
            let aspect_degrees = triangle_aspect_degrees(a, b, c)?;
            Some(TriangleVisualization {
                face: *face,
                value: aspect_degrees,
                color: aspect_color(aspect_degrees),
            })
        })
        .collect()
}

pub fn elevation_band_visualization(
    mesh: &TriangleMesh,
    band_width: f32,
) -> Vec<TriangleVisualization> {
    let Some((min_y, max_y)) = elevation_range(mesh) else {
        return Vec::new();
    };
    let width = band_width.max(1.0e-4);
    mesh.faces
        .iter()
        .filter_map(|face| {
            let (a, b, c) = face_points(mesh, *face)?;
            let mean_y = (a.y + b.y + c.y) / 3.0;
            let band_index = ((mean_y - min_y) / width).floor();
            let band_elevation = min_y + band_index * width;
            let t = if (max_y - min_y).abs() <= f32::EPSILON {
                0.0
            } else {
                ((band_elevation - min_y) / (max_y - min_y)).clamp(0.0, 1.0)
            };
            Some(TriangleVisualization {
                face: *face,
                value: band_elevation,
                color: lerp_color(ELEVATION_LOW_COLOR, ELEVATION_HIGH_COLOR, t),
            })
        })
        .collect()
}

fn face_points(mesh: &TriangleMesh, face: [u32; 3]) -> Option<(Vec3, Vec3, Vec3)> {
    Some((
        *mesh.vertices.get(face[0] as usize)?,
        *mesh.vertices.get(face[1] as usize)?,
        *mesh.vertices.get(face[2] as usize)?,
    ))
}

fn triangle_slope_degrees(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let normal = (b - a).cross(c - a).normalize_or_zero();
    if normal.length_squared() <= f32::EPSILON {
        return 0.0;
    }
    normal
        .dot(Vec3::Y)
        .abs()
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

fn triangle_aspect_degrees(a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let mut normal = (b - a).cross(c - a).normalize_or_zero();
    if normal.length_squared() <= f32::EPSILON {
        return None;
    }
    if normal.y < 0.0 {
        normal = -normal;
    }
    if normal.y.abs() <= f32::EPSILON {
        return None;
    }
    let descent = Vec2::new(normal.x / normal.y, normal.z / normal.y);
    if descent.length_squared() <= f32::EPSILON {
        return Some(0.0);
    }
    let mut degrees = descent.x.atan2(-descent.y).to_degrees();
    if degrees < 0.0 {
        degrees += 360.0;
    }
    Some(degrees)
}

fn elevation_range(mesh: &TriangleMesh) -> Option<(f32, f32)> {
    let mut vertices = mesh.vertices.iter();
    let first = vertices.next()?;
    let mut min_y = first.y;
    let mut max_y = first.y;
    for vertex in vertices {
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
    }
    Some((min_y, max_y))
}

fn slope_color(slope_degrees: f32) -> [f32; 4] {
    if slope_degrees <= 15.0 {
        lerp_color(FLAT_SLOPE_COLOR, MODERATE_SLOPE_COLOR, slope_degrees / 15.0)
    } else {
        lerp_color(
            MODERATE_SLOPE_COLOR,
            STEEP_SLOPE_COLOR,
            ((slope_degrees - 15.0) / 30.0).clamp(0.0, 1.0),
        )
    }
}

fn aspect_color(aspect_degrees: f32) -> [f32; 4] {
    let normalized = aspect_degrees.rem_euclid(360.0);
    if normalized < 90.0 {
        lerp_color(ASPECT_NORTH_COLOR, ASPECT_EAST_COLOR, normalized / 90.0)
    } else if normalized < 180.0 {
        lerp_color(
            ASPECT_EAST_COLOR,
            ASPECT_SOUTH_COLOR,
            (normalized - 90.0) / 90.0,
        )
    } else if normalized < 270.0 {
        lerp_color(
            ASPECT_SOUTH_COLOR,
            ASPECT_WEST_COLOR,
            (normalized - 180.0) / 90.0,
        )
    } else {
        lerp_color(
            ASPECT_WEST_COLOR,
            ASPECT_NORTH_COLOR,
            (normalized - 270.0) / 90.0,
        )
    }
}

fn lerp_color(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
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
    fn slope_visualization_classifies_flat_and_steep_triangles() {
        let terrain = mesh(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
            ],
            vec![[0, 1, 2], [3, 4, 5]],
        );

        let map = slope_visualization(&terrain);

        assert_eq!(map.len(), 2);
        assert!(map[0].value.abs() < 1.0e-5);
        assert!(map[1].value > 30.0);
        assert_ne!(map[0].color, map[1].color);
    }

    #[test]
    fn aspect_visualization_reports_compass_direction() {
        let terrain = mesh(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
            ],
            vec![[0, 1, 2]],
        );

        let map = aspect_visualization(&terrain);

        assert_eq!(map.len(), 1);
        assert!((map[0].value - 270.0).abs() < 1.0e-4);
        assert_eq!(map[0].color, ASPECT_WEST_COLOR);
    }

    #[test]
    fn elevation_bands_snap_to_configured_interval() {
        let terrain = mesh(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.2, 0.0),
                Vec3::new(0.0, 0.4, 1.0),
                Vec3::new(1.0, 1.1, 0.0),
                Vec3::new(2.0, 1.4, 0.0),
                Vec3::new(1.0, 1.7, 1.0),
            ],
            vec![[0, 1, 2], [3, 4, 5]],
        );

        let map = elevation_band_visualization(&terrain, 1.0);

        assert_eq!(map.len(), 2);
        assert_eq!(map[0].value, 0.0);
        assert_eq!(map[1].value, 1.0);
        assert_ne!(map[0].color, map[1].color);
    }

    #[test]
    fn visualization_ignores_invalid_faces() {
        let terrain = mesh(vec![Vec3::ZERO, Vec3::X, Vec3::Z], vec![[0, 1, 9]]);

        assert!(slope_visualization(&terrain).is_empty());
        assert!(aspect_visualization(&terrain).is_empty());
        assert!(elevation_band_visualization(&terrain, 1.0).is_empty());
    }
}
