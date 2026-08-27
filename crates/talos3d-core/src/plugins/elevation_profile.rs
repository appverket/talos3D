//! Orthographic elevation profiles derived from authored geometry.
//!
//! An elevation is the representation a builder, a planner and a child drawing
//! a house all reason in: the outline you see standing square on to one face,
//! and the material regions inside it. Almost everything anyone recognises as
//! "wrong" about a building at a glance is a property of that outline — an apex
//! off the centreline, a roof that fails to project past the wall, a material
//! that changes where no joint exists.
//!
//! This module computes that representation **from the authored meshes**, not
//! from the viewport. That matters for two reasons:
//!
//! 1. `take_screenshot` returns the viewport *with its overlays* — pivot
//!    markers, selection outlines, rotation rings, the compass. A dark pivot
//!    marker sitting on a ridge is indistinguishable from a hole in the roof.
//!    Evidence used to accept or reject a model must not contain them.
//! 2. A profile is machine-comparable. "Does this match the archetype?" and
//!    "does this match what I said I was going to build?" are both diffs over
//!    this structure, and neither is answerable from pixels.
//!
//! The implementation is a small deterministic software rasteriser: project
//! every mesh triangle onto the elevation plane, keep the nearest surface per
//! cell, and read the outline and material regions off the result. No GPU, no
//! camera, no frame timing, and identical output for identical input.

use bevy::{
    prelude::*,
    render::mesh::{Indices, VertexAttributeValues},
};
use serde::{Deserialize, Serialize};

use crate::curation::material_specs::MaterialSpecRegistry;
use crate::plugins::materials::MaterialAssignment;

/// Which face of the building the viewer is square on to.
///
/// Talos3D cardinal convention at `north_axis_deg = 0`: North = +Z, East = -X,
/// South = -Z, West = +X. "South elevation" means the viewer stands to the
/// south and looks north, so it shows the face pointing south.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElevationDirection {
    North,
    South,
    East,
    West,
}

impl ElevationDirection {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "north" | "n" => Some(Self::North),
            "south" | "s" => Some(Self::South),
            "east" | "e" => Some(Self::East),
            "west" | "w" => Some(Self::West),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::North => "north",
            Self::South => "south",
            Self::East => "east",
            Self::West => "west",
        }
    }

    /// The direction the viewer looks, and the in-plane horizontal axis. The
    /// vertical axis is always +Y.
    fn axes(self) -> (Vec3, Vec3) {
        match self {
            // Viewer south of the building, looking north (+Z).
            Self::South => (Vec3::Z, Vec3::X),
            // Viewer north of the building, looking south (-Z).
            Self::North => (Vec3::NEG_Z, Vec3::NEG_X),
            // Viewer west of the building, looking east (-X).
            Self::West => (Vec3::NEG_X, Vec3::Z),
            // Viewer east of the building, looking west (+X).
            Self::East => (Vec3::X, Vec3::NEG_Z),
        }
    }
}

/// A contiguous run of one material down a vertical scan line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialBand {
    /// Lower bound of the band, in metres above the model's lowest point.
    pub from_height_m: f32,
    /// Upper bound of the band, in metres.
    pub to_height_m: f32,
    /// Resolved render material id, or `None` for geometry with no material —
    /// which renders in the engine default and is what an unfinished surface
    /// looks like.
    pub material_id: Option<String>,
}

/// What one elevation of the model looks like.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElevationProfile {
    pub direction: String,
    /// Horizontal extent of the projected model, in metres.
    pub width_m: f32,
    /// Vertical extent, in metres.
    pub height_m: f32,
    /// World Y of the lowest projected point; heights below are relative to it.
    pub base_y_m: f32,
    /// Number of sample columns across the width.
    pub columns: usize,
    /// Height of the model's upper outline at each column, in metres above
    /// `base_y_m`. `None` where the column is empty.
    pub upper_outline_m: Vec<Option<f32>>,
    /// Height of the lower outline at each column.
    pub lower_outline_m: Vec<Option<f32>>,
    /// Number of sample rows up the height.
    pub rows: usize,
    /// Height of one sample row, in metres.
    pub row_height_m: f32,
    /// Covered width at each row, in metres, from the bottom up. This is what
    /// distinguishes a roof that projects past its walls from one flush with
    /// them: the width jumps outward at the eave.
    pub width_at_row_m: Vec<f32>,
    /// Local maxima of the upper outline: the peaks a person would call apexes.
    /// `position_ratio` is 0.0 at the left edge and 1.0 at the right.
    pub apexes: Vec<Apex>,
    /// Material bands down the vertical centreline.
    pub centre_bands: Vec<MaterialBand>,
    /// Every material visible in this elevation, with the fraction of the
    /// covered area it occupies.
    pub material_coverage: Vec<MaterialCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Apex {
    pub position_ratio: f32,
    pub height_m: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialCoverage {
    pub material_id: Option<String>,
    pub area_fraction: f32,
}

/// Default raster resolution. High enough to resolve a bargeboard on a house-
/// sized elevation, low enough to compute in a few milliseconds.
pub const DEFAULT_RESOLUTION: usize = 256;

/// One rasterised cell: the nearest surface the viewer can see there.
#[derive(Clone, Copy)]
struct Cell {
    depth: f32,
    material: u32,
}

/// Project the whole scene.
pub fn compute_elevation_profile(
    world: &World,
    direction: ElevationDirection,
    resolution: usize,
) -> Option<ElevationProfile> {
    compute_elevation_profile_for(world, direction, resolution, None)
}

/// Project only `subject`, when given. A validator checking one building must
/// not have its outline confounded by terrain, neighbouring structures, or
/// anything else that happens to share the view.
pub fn compute_elevation_profile_for(
    world: &World,
    direction: ElevationDirection,
    resolution: usize,
    subject: Option<&bevy::platform::collections::HashSet<Entity>>,
) -> Option<ElevationProfile> {
    let resolution = resolution.clamp(16, 2048);
    let mesh_assets = world.get_resource::<Assets<Mesh>>()?;
    let specs = world.get_resource::<MaterialSpecRegistry>();

    // Collect projected triangles plus the material each belongs to. Material
    // ids are interned so the raster can hold a u32 per cell.
    let mut material_ids: Vec<Option<String>> = vec![None];
    let mut triangles: Vec<([Vec3; 3], u32)> = Vec::new();

    let mut query = world.try_query_filtered::<(
        Entity,
        &Mesh3d,
        &GlobalTransform,
        Option<&MaterialAssignment>,
        Option<&Visibility>,
    ), ()>()?;
    let (view_dir, u_axis) = direction.axes();

    for (entity, mesh_handle, transform, assignment, visibility) in query.iter(world) {
        if visibility.is_some_and(|v| *v == Visibility::Hidden) {
            continue;
        }
        if subject.is_some_and(|allowed| !allowed.contains(&entity)) {
            continue;
        }
        let Some(mesh) = mesh_assets.get(&mesh_handle.0) else {
            continue;
        };
        let Some(positions) = mesh_positions(mesh) else {
            continue;
        };
        let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
            continue;
        };

        let resolved = assignment.and_then(|a| a.render_material_id(specs));
        let material_index = match material_ids.iter().position(|id| *id == resolved) {
            Some(index) => index as u32,
            None => {
                material_ids.push(resolved);
                (material_ids.len() - 1) as u32
            }
        };

        let affine = transform.affine();
        for triangle in indices.chunks_exact(3) {
            let world_points = [
                affine.transform_point3(Vec3::from(positions[triangle[0] as usize])),
                affine.transform_point3(Vec3::from(positions[triangle[1] as usize])),
                affine.transform_point3(Vec3::from(positions[triangle[2] as usize])),
            ];
            triangles.push((world_points, material_index));
        }
    }

    if triangles.is_empty() {
        return None;
    }

    // Projected bounds.
    let project = |p: Vec3| Vec2::new(p.dot(u_axis), p.y);
    let (mut min_uv, mut max_uv) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for (points, _) in &triangles {
        for point in points {
            let uv = project(*point);
            min_uv = min_uv.min(uv);
            max_uv = max_uv.max(uv);
        }
    }
    let width = (max_uv.x - min_uv.x).max(1e-4);
    let height = (max_uv.y - min_uv.y).max(1e-4);

    // Square-ish cells: keep the vertical sampling proportional so a slope's
    // outline is resolved as well horizontally as vertically.
    let columns = resolution;
    let rows = ((resolution as f32 * height / width).round() as usize).clamp(16, 4096);
    let mut raster = vec![
        Cell {
            depth: f32::MAX,
            material: u32::MAX,
        };
        columns * rows
    ];

    for (points, material) in &triangles {
        rasterise_triangle(
            &mut raster,
            columns,
            rows,
            min_uv,
            width,
            height,
            points,
            *material,
            project,
            view_dir,
        );
    }

    let cell_height = height / rows as f32;
    let cell_width = width / columns as f32;

    // Outlines.
    let mut upper = vec![None; columns];
    let mut lower = vec![None; columns];
    for column in 0..columns {
        for row in 0..rows {
            if raster[row * columns + column].material != u32::MAX {
                let v = (row as f32 + 0.5) * cell_height;
                if lower[column].is_none() {
                    lower[column] = Some(v);
                }
                upper[column] = Some(v);
            }
        }
    }

    // Material bands down the centreline.
    let centre = columns / 2;
    let mut centre_bands: Vec<MaterialBand> = Vec::new();
    for row in 0..rows {
        let cell = raster[row * columns + centre];
        if cell.material == u32::MAX {
            continue;
        }
        let id = material_ids[cell.material as usize].clone();
        let from = row as f32 * cell_height;
        let to = from + cell_height;
        match centre_bands.last_mut() {
            Some(last)
                if last.material_id == id
                    && (from - last.to_height_m).abs() < cell_height * 0.5 =>
            {
                last.to_height_m = to;
            }
            _ => centre_bands.push(MaterialBand {
                from_height_m: from,
                to_height_m: to,
                material_id: id,
            }),
        }
    }

    // Coverage.
    let mut counts = vec![0usize; material_ids.len()];
    let mut covered = 0usize;
    for cell in &raster {
        if cell.material != u32::MAX {
            counts[cell.material as usize] += 1;
            covered += 1;
        }
    }
    let mut material_coverage: Vec<MaterialCoverage> = counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(index, count)| MaterialCoverage {
            material_id: material_ids[index].clone(),
            area_fraction: *count as f32 / covered.max(1) as f32,
        })
        .collect();
    material_coverage.sort_by(|a, b| b.area_fraction.total_cmp(&a.area_fraction));

    // Covered width per row.
    let mut width_at_row_m = vec![0.0f32; rows];
    for row in 0..rows {
        let covered = (0..columns)
            .filter(|column| raster[row * columns + column].material != u32::MAX)
            .count();
        width_at_row_m[row] = covered as f32 * cell_width;
    }

    let apexes = find_apexes(&upper, cell_width, columns);

    Some(ElevationProfile {
        direction: direction.label().to_string(),
        width_m: width,
        height_m: height,
        base_y_m: min_uv.y,
        columns,
        rows,
        row_height_m: cell_height,
        width_at_row_m,
        upper_outline_m: upper,
        lower_outline_m: lower,
        apexes,
        centre_bands,
        material_coverage,
    })
}

/// Peaks in the upper outline. A gable elevation has exactly one, on the
/// centreline; a hip has none; a saltbox has one off-centre. Small ripples from
/// rasterisation are ignored by requiring a peak to stand clear of its
/// neighbourhood.
fn find_apexes(upper: &[Option<f32>], cell_width: f32, columns: usize) -> Vec<Apex> {
    // A peak must stand at least this far above the outline at the edge of its
    // neighbourhood, so seam and trim detail never registers as a roof form.
    const PROMINENCE_M: f32 = 0.15;
    // How far either side counts as the neighbourhood.
    let window = ((0.75 / cell_width.max(1e-4)).round() as usize).clamp(2, columns.max(4) / 2);

    let mut apexes = Vec::new();
    let mut index = 0usize;
    while index < columns {
        let Some(height) = upper[index] else {
            index += 1;
            continue;
        };
        // Extend across a flat run so a ridge seen exactly end-on reports once.
        let mut end = index;
        while end + 1 < columns
            && upper[end + 1].is_some_and(|next| (next - height).abs() < PROMINENCE_M * 0.2)
        {
            end += 1;
        }

        let left = index.saturating_sub(window);
        let right = (end + window).min(columns - 1);

        // Two conditions, and the second is the one that matters. The run must
        // be the highest thing in its neighbourhood, AND it must stand proud of
        // the neighbourhood's EDGES. Measuring prominence against the nearest
        // neighbour instead — which an earlier version did — compares a ridge
        // against the column beside it, one raster step down the slope. On a
        // 27-degree roof at typical resolution that step is under 30 mm, so
        // every genuine apex was rejected while the outline was plainly a
        // triangle.
        let is_local_max = (left..=right)
            .filter(|i| *i < index || *i > end)
            .filter_map(|i| upper[i])
            .all(|neighbour| neighbour <= height + 1e-4);

        // The minimum of the two edges, not the maximum: a shed roof's ridge
        // sits hard against one end of the elevation, so the window is clamped
        // on that side and that edge is the apex itself. Taking the lower edge
        // keeps a monopitch detectable without admitting a flat roof, where
        // both edges are level with the "peak".
        let edge_height = [
            upper.get(left).copied().flatten(),
            upper.get(right).copied().flatten(),
        ]
        .into_iter()
        .flatten()
        .fold(f32::MAX, f32::min);
        let stands_proud = edge_height == f32::MAX || height - edge_height >= PROMINENCE_M;

        if is_local_max && stands_proud {
            apexes.push(Apex {
                position_ratio: ((index + end) as f32 * 0.5) / (columns - 1).max(1) as f32,
                height_m: height,
            });
        }
        index = end + 1;
    }
    apexes
}

#[allow(clippy::too_many_arguments)]
fn rasterise_triangle(
    raster: &mut [Cell],
    columns: usize,
    rows: usize,
    min_uv: Vec2,
    width: f32,
    height: f32,
    points: &[Vec3; 3],
    material: u32,
    project: impl Fn(Vec3) -> Vec2,
    view_dir: Vec3,
) {
    let uv: Vec<Vec2> = points.iter().map(|p| project(*p)).collect();
    let depth: Vec<f32> = points.iter().map(|p| p.dot(view_dir)).collect();

    let to_cell = |p: Vec2| {
        Vec2::new(
            (p.x - min_uv.x) / width * columns as f32,
            (p.y - min_uv.y) / height * rows as f32,
        )
    };
    let a = to_cell(uv[0]);
    let b = to_cell(uv[1]);
    let c = to_cell(uv[2]);

    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let max_x = (a.x.max(b.x).max(c.x).ceil() as isize).clamp(0, columns as isize - 1) as usize;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let max_y = (a.y.max(b.y).max(c.y).ceil() as isize).clamp(0, rows as isize - 1) as usize;

    let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
    if area.abs() < 1e-9 {
        return;
    }

    for row in min_y..=max_y {
        for column in min_x..=max_x {
            let p = Vec2::new(column as f32 + 0.5, row as f32 + 0.5);
            // Barycentric coordinates; inside when all three are non-negative
            // (sign-normalised so winding does not matter).
            let w0 = ((b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y)) / area;
            let w1 = ((p.x - a.x) * (c.y - a.y) - (c.x - a.x) * (p.y - a.y)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                continue;
            }
            let cell_depth = w2 * depth[0] + w1 * depth[1] + w0 * depth[2];
            let cell = &mut raster[row * columns + column];
            if cell_depth < cell.depth {
                cell.depth = cell_depth;
                cell.material = material;
            }
        }
    }
}

fn mesh_positions(mesh: &Mesh) -> Option<Vec<[f32; 3]>> {
    match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
        VertexAttributeValues::Float32x3(values) => Some(values.clone()),
        _ => None,
    }
}

fn mesh_triangle_indices(mesh: &Mesh, vertex_count: usize) -> Option<Vec<u32>> {
    match mesh.indices() {
        Some(Indices::U32(values)) => Some(values.clone()),
        Some(Indices::U16(values)) => Some(values.iter().map(|v| *v as u32).collect()),
        None if vertex_count.is_multiple_of(3) => Some((0..vertex_count as u32).collect()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinal_axes_follow_the_talos3d_compass() {
        // North = +Z, so the north elevation is seen looking along -Z.
        assert_eq!(ElevationDirection::North.axes().0, Vec3::NEG_Z);
        assert_eq!(ElevationDirection::South.axes().0, Vec3::Z);
        // East = -X, so the east elevation is seen looking along +X.
        assert_eq!(ElevationDirection::East.axes().0, Vec3::X);
        assert_eq!(ElevationDirection::West.axes().0, Vec3::NEG_X);
    }

    #[test]
    fn parses_names_and_initials() {
        assert_eq!(
            ElevationDirection::parse("South"),
            Some(ElevationDirection::South)
        );
        assert_eq!(
            ElevationDirection::parse("w"),
            Some(ElevationDirection::West)
        );
        assert_eq!(ElevationDirection::parse("up"), None);
    }

    /// A single peak standing clear of a flat outline is one apex; a flat
    /// outline has none that stand proud of their neighbourhood.
    #[test]
    fn finds_a_single_central_apex() {
        let columns = 101;
        let upper: Vec<Option<f32>> = (0..columns)
            .map(|i| {
                let x = i as f32 / (columns - 1) as f32;
                Some(2.0 + 1.5 * (1.0 - (x - 0.5).abs() * 2.0))
            })
            .collect();
        let apexes = find_apexes(&upper, 0.05, columns);
        assert_eq!(apexes.len(), 1, "{apexes:?}");
        assert!(
            (apexes[0].position_ratio - 0.5).abs() < 0.05,
            "apex must sit on the centreline: {apexes:?}"
        );
    }

    /// Regression. The synthetic triangle above passes with almost any
    /// prominence rule because its columns step by ~30 mm each. A real gable at
    /// real raster resolution steps by far less, and an earlier version of this
    /// function compared a peak against its immediate neighbour rather than the
    /// edge of its neighbourhood — so it found no apex at all on an outline that
    /// was plainly a triangle. These are the measured numbers from a 7.14 m wide
    /// 27-degree gable sampled at 128 columns.
    #[test]
    fn finds_the_apex_of_a_real_gable_at_raster_resolution() {
        let columns = 128;
        let width_m = 7.14_f32;
        let cell_width = width_m / columns as f32;
        let pitch = 27.0_f32.to_radians();
        let upper: Vec<Option<f32>> = (0..columns)
            .map(|i| {
                let from_centre = ((i as f32 + 0.5) - columns as f32 / 2.0).abs() * cell_width;
                Some(4.52 - from_centre * pitch.tan())
            })
            .collect();

        let apexes = find_apexes(&upper, cell_width, columns);
        assert_eq!(apexes.len(), 1, "{apexes:?}");
        assert!(
            (apexes[0].position_ratio - 0.5).abs() < 0.05,
            "apex must sit on the centreline: {apexes:?}"
        );
    }

    /// A monopitch peaks hard against one end, where the neighbourhood window is
    /// clamped and one "edge" is the peak itself. Prominence therefore has to be
    /// measured against the LOWER edge, or every shed roof reports no ridge.
    #[test]
    fn finds_the_ridge_of_a_monopitch_at_the_end_of_the_elevation() {
        let columns = 128;
        let cell_width = 7.0 / columns as f32;
        let upper: Vec<Option<f32>> = (0..columns)
            .map(|i| Some(4.0 - (i as f32 * cell_width) * 0.3))
            .collect();

        let apexes = find_apexes(&upper, cell_width, columns);
        assert_eq!(apexes.len(), 1, "{apexes:?}");
        assert!(
            apexes[0].position_ratio < 0.1,
            "the ridge of a monopitch sits at one end: {apexes:?}"
        );
    }

    #[test]
    fn a_flat_roof_has_no_apex() {
        let columns = 101;
        let upper: Vec<Option<f32>> = vec![Some(3.0); columns];
        assert!(find_apexes(&upper, 0.05, columns).is_empty());
    }
}
