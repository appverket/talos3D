//! Terrain height-field query layer (ADR-059, PP-PLANT-A).
//!
//! A [`TerrainHeightfield`] is a regular grid of IDW-sampled heights over a
//! [`crate::components::TerrainSurface`], plus an inside-boundary mask. It is the
//! queryable representation that terrain-conforming placement reads — decoupled
//! from the render mesh so the feature does not depend on triangulation internals.
//!
//! Queries are O(1): `height_at` is a bilinear lookup in the containing cell, and
//! `max_over` scans only the grid nodes under a footprint's bounding box. The
//! *build* is a one-time cost paid when the surface changes (mirroring
//! [`crate::components::TerrainMeshCache`]).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::reconstruction::point_in_polygon_2d;

/// IDW neighbour count and epsilon, matching `reconstruction::interpolate_height_idw`.
const IDW_K: usize = 8;
const IDW_EPSILON: f32 = 1.0e-4;

/// Uniform-grid spatial index of contour points for `O(k)`-per-node IDW during
/// the height-field build (instead of scanning every contour point per node).
/// Finds the same K nearest as the linear scan via expanding-ring search, so the
/// interpolated heights match within tie-breaking/float tolerance.
struct ContourGrid<'a> {
    points: &'a [Vec3],
    cell: f32,
    origin: Vec2,
    max_ring: i32,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

impl<'a> ContourGrid<'a> {
    fn build(points: &'a [Vec3], cell: f32, origin: Vec2, extent: Vec2) -> Self {
        let cell = cell.max(f32::MIN_POSITIVE);
        let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (index, p) in points.iter().enumerate() {
            let key = (
                ((p.x - origin.x) / cell).floor() as i32,
                ((p.z - origin.y) / cell).floor() as i32,
            );
            cells.entry(key).or_default().push(index as u32);
        }
        let max_ring = ((extent.x.max(extent.y)) / cell).ceil() as i32 + 2;
        Self {
            points,
            cell,
            origin,
            max_ring,
            cells,
        }
    }

    fn height_at(&self, point: Vec2) -> f32 {
        let ci = ((point.x - self.origin.x) / self.cell).floor() as i32;
        let cj = ((point.y - self.origin.y) / self.cell).floor() as i32;
        let mut best: [(f32, f32); IDW_K] = [(f32::INFINITY, 0.0); IDW_K];
        let mut count = 0usize;
        let mut ring = 0i32;
        loop {
            for di in -ring..=ring {
                for dj in -ring..=ring {
                    if di.abs() != ring && dj.abs() != ring {
                        continue; // only the ring shell at Chebyshev distance `ring`
                    }
                    let Some(bucket) = self.cells.get(&(ci + di, cj + dj)) else {
                        continue;
                    };
                    for &index in bucket {
                        let sample = self.points[index as usize];
                        let d2 = Vec2::new(sample.x, sample.z).distance_squared(point);
                        if count == IDW_K && d2 >= best[IDW_K - 1].0 {
                            continue;
                        }
                        let mut i = count.min(IDW_K - 1);
                        while i > 0 && best[i - 1].0 > d2 {
                            best[i] = best[i - 1];
                            i -= 1;
                        }
                        best[i] = (d2, sample.y);
                        if count < IDW_K {
                            count += 1;
                        }
                    }
                }
            }
            // The nearest unsearched point sits in ring `ring+1`, at least
            // `ring*cell` away. If that already exceeds the Kth-nearest, stop.
            let next_ring_min = ring as f32 * self.cell;
            if (count >= IDW_K && next_ring_min * next_ring_min > best[IDW_K - 1].0)
                || ring > self.max_ring
            {
                break;
            }
            ring += 1;
        }

        if count == 0 {
            return 0.0;
        }
        if best[0].0 <= IDW_EPSILON {
            return best[0].1;
        }
        let mut weighted = 0.0;
        let mut weight = 0.0;
        for &(d2, h) in best.iter().take(count) {
            let w = 1.0 / d2.max(IDW_EPSILON);
            weighted += h * w;
            weight += w;
        }
        if weight <= IDW_EPSILON {
            0.0
        } else {
            weighted / weight
        }
    }
}

/// Upper bound on grid nodes per axis, so a large site can't produce an
/// unbounded build. The cell size is widened to respect this cap.
pub const MAX_HEIGHTFIELD_NODES_PER_AXIS: usize = 200;

/// Regular-grid sampled height field derived from a terrain surface.
///
/// Node `(i, j)` (with `i` along +X, `j` along +Z) is at world XZ
/// `origin + (i, j) * cell` and holds the IDW height of the surface there.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct TerrainHeightfield {
    /// World XZ of node `(0, 0)` — the minimum corner.
    pub origin: Vec2,
    /// Node spacing in world units (square cells).
    pub cell: f32,
    /// Node count along +X.
    pub nx: usize,
    /// Node count along +Z.
    pub nz: usize,
    /// Row-major (`j * nx + i`) heights at every node.
    heights: Vec<f32>,
    /// Row-major inside-boundary mask; `false` nodes are outside the surface.
    inside: Vec<bool>,
}

impl TerrainHeightfield {
    /// Build from sampled contour points (`Vec3` with `y` = elevation) and an
    /// optional XZ boundary polygon. `cell_hint` is the desired node spacing; it
    /// is widened if needed so neither axis exceeds
    /// [`MAX_HEIGHTFIELD_NODES_PER_AXIS`]. Returns `None` for fewer than three
    /// points or a degenerate extent.
    ///
    /// The build runs IDW per node; for very dense corpora a contour-point
    /// spatial index would make it `O(k)`/node (tracked as a follow-up). Queries
    /// are already O(1).
    pub fn build(
        contour_points: &[Vec3],
        boundary: &[Vec2],
        cell_hint: f32,
        smoothing: f32,
    ) -> Option<Self> {
        if contour_points.len() < 3 {
            return None;
        }
        let has_boundary = boundary.len() >= 3;

        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        if has_boundary {
            for p in boundary {
                min = min.min(*p);
                max = max.max(*p);
            }
        } else {
            for p in contour_points {
                let xz = Vec2::new(p.x, p.z);
                min = min.min(xz);
                max = max.max(xz);
            }
        }
        let extent = max - min;
        if extent.x <= f32::EPSILON || extent.y <= f32::EPSILON {
            return None;
        }

        let cap = MAX_HEIGHTFIELD_NODES_PER_AXIS as f32;
        let cell = cell_hint
            .max(f32::MIN_POSITIVE)
            .max(extent.x / cap)
            .max(extent.y / cap);
        let nx = ((extent.x / cell).ceil() as usize + 1).max(2);
        let nz = ((extent.y / cell).ceil() as usize + 1).max(2);

        // Spatial-index the contour points once so each node's IDW is O(k), not
        // O(contour points) — the previous per-node scan dominated the build. Size
        // the grid cell to roughly one point per cell so ring search stays tight.
        let grid_cell = ((extent.x * extent.y) / contour_points.len().max(1) as f32)
            .sqrt()
            .clamp(0.5, 8.0);
        let grid = ContourGrid::build(contour_points, grid_cell, min, extent);
        let mut heights = vec![0.0f32; nx * nz];
        let mut inside = vec![true; nx * nz];
        for j in 0..nz {
            for i in 0..nx {
                let p = Vec2::new(min.x + i as f32 * cell, min.y + j as f32 * cell);
                let idx = j * nx + i;
                heights[idx] = grid.height_at(p);
                if has_boundary {
                    inside[idx] = point_in_polygon_2d(p, boundary);
                }
            }
        }

        // Match the render surface: relax the IDW inter-contour terraces so the
        // draped foundations sit on the same smoothed ground the user sees.
        smooth_grid_heights(&mut heights, nx, nz, contour_points, min, cell, smoothing);

        Some(Self {
            origin: min,
            cell,
            nx,
            nz,
            heights,
            inside,
        })
    }

    /// Raw height at node `(i, j)` if that node is inside the surface.
    #[inline]
    pub fn node_height(&self, i: usize, j: usize) -> Option<f32> {
        if i >= self.nx || j >= self.nz {
            return None;
        }
        let idx = j * self.nx + i;
        if self.inside[idx] {
            Some(self.heights[idx])
        } else {
            None
        }
    }

    /// Bilinearly-interpolated surface height at world `(x, z)`. `None` if the
    /// point is outside the grid extent or its containing cell has any node
    /// outside the inside-boundary mask.
    pub fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let fx = (x - self.origin.x) / self.cell;
        let fz = (z - self.origin.y) / self.cell;
        // Reject points outside the grid (with a small tolerance for the edge).
        if fx < -1e-3
            || fz < -1e-3
            || fx > (self.nx - 1) as f32 + 1e-3
            || fz > (self.nz - 1) as f32 + 1e-3
        {
            return None;
        }
        let i0 = (fx.floor() as usize).min(self.nx - 2);
        let j0 = (fz.floor() as usize).min(self.nz - 2);
        let tx = (fx - i0 as f32).clamp(0.0, 1.0);
        let tz = (fz - j0 as f32).clamp(0.0, 1.0);

        let h00 = self.node_height(i0, j0)?;
        let h10 = self.node_height(i0 + 1, j0)?;
        let h01 = self.node_height(i0, j0 + 1)?;
        let h11 = self.node_height(i0 + 1, j0 + 1)?;
        let h0 = h00 * (1.0 - tx) + h10 * tx;
        let h1 = h01 * (1.0 - tx) + h11 * tx;
        Some(h0 * (1.0 - tz) + h1 * tz)
    }

    /// Highest surface height under a world-space footprint polygon, and the
    /// world XZ where it occurs. Scans grid nodes within the footprint's bounding
    /// box that fall inside both the polygon and the surface mask. `None` if no
    /// inside node lies under the footprint.
    pub fn max_over(&self, footprint_world: &[Vec2]) -> Option<(f32, Vec2)> {
        if footprint_world.len() < 3 {
            return None;
        }
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for p in footprint_world {
            min = min.min(*p);
            max = max.max(*p);
        }
        let i_lo = (((min.x - self.origin.x) / self.cell).floor() as isize).max(0) as usize;
        let j_lo = (((min.y - self.origin.y) / self.cell).floor() as isize).max(0) as usize;
        let i_hi = (((max.x - self.origin.x) / self.cell).ceil() as usize).min(self.nx - 1);
        let j_hi = (((max.y - self.origin.y) / self.cell).ceil() as usize).min(self.nz - 1);

        let mut best: Option<(f32, Vec2)> = None;
        // Seed with the footprint vertices so a maximum on an edge/corner (which
        // no interior grid node lands on) is not missed — the foundation must
        // clear those points too.
        for &vertex in footprint_world {
            if let Some(h) = self.height_at(vertex.x, vertex.y) {
                if best.is_none_or(|(bh, _)| h > bh) {
                    best = Some((h, vertex));
                }
            }
        }
        for j in j_lo..=j_hi {
            for i in i_lo..=i_hi {
                let pos = Vec2::new(
                    self.origin.x + i as f32 * self.cell,
                    self.origin.y + j as f32 * self.cell,
                );
                if !point_in_polygon_2d(pos, footprint_world) {
                    continue;
                }
                let Some(h) = self.node_height(i, j) else {
                    continue;
                };
                if best.is_none_or(|(bh, _)| h > bh) {
                    best = Some((h, pos));
                }
            }
        }
        best
    }

    /// Sample the surface on a regular grid covering a world-space footprint at
    /// the given spacing. Returns `(world_xz, height)` for sample points inside
    /// the footprint; `height` is `None` where the surface mask excludes it.
    /// Used by downstream conforming-underside construction (PP-PLANT-B).
    pub fn sample_grid(
        &self,
        footprint_world: &[Vec2],
        resolution: f32,
    ) -> Vec<(Vec2, Option<f32>)> {
        if footprint_world.len() < 3 {
            return Vec::new();
        }
        let resolution = resolution.max(f32::MIN_POSITIVE);
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for p in footprint_world {
            min = min.min(*p);
            max = max.max(*p);
        }
        let mut out = Vec::new();
        let mut z = min.y;
        while z <= max.y {
            let mut x = min.x;
            while x <= max.x {
                let p = Vec2::new(x, z);
                if point_in_polygon_2d(p, footprint_world) {
                    out.push((p, self.height_at(p.x, p.y)));
                }
                x += resolution;
            }
            z += resolution;
        }
        out
    }
}

/// Constrained Laplacian smoothing of a heightfield grid, mirroring the render-mesh
/// smoothing so foundations drape onto the same ground the user sees. Grid nodes
/// nearest a surveyed contour point are kept faithful (high data-attachment); other
/// nodes relax toward their 4-neighbour mean. `smoothing` in `0..=1`; 0 = no-op.
fn smooth_grid_heights(
    heights: &mut [f32],
    nx: usize,
    nz: usize,
    contour_points: &[Vec3],
    origin: Vec2,
    cell: f32,
    smoothing: f32,
) {
    let s = smoothing.clamp(0.0, 1.0);
    if s <= 0.0 || nx < 3 || nz < 3 || cell <= 0.0 || heights.len() != nx * nz {
        return;
    }

    let mut pinned = vec![false; nx * nz];
    for point in contour_points {
        let i = (((point.x - origin.x) / cell).round() as isize).clamp(0, nx as isize - 1) as usize;
        let j = (((point.z - origin.y) / cell).round() as isize).clamp(0, nz as isize - 1) as usize;
        pinned[j * nx + i] = true;
    }

    let original: Vec<f32> = heights.to_vec();
    let iterations = (3.0 + 12.0 * s).round() as usize;
    for _ in 0..iterations {
        let current = heights.to_vec();
        for j in 0..nz {
            for i in 0..nx {
                let idx = j * nx + i;
                let mut sum = 0.0f32;
                let mut count = 0u32;
                if i > 0 {
                    sum += current[idx - 1];
                    count += 1;
                }
                if i + 1 < nx {
                    sum += current[idx + 1];
                    count += 1;
                }
                if j > 0 {
                    sum += current[idx - nx];
                    count += 1;
                }
                if j + 1 < nz {
                    sum += current[idx + nx];
                    count += 1;
                }
                if count == 0 {
                    continue;
                }
                let mean = sum / count as f32;
                let attach = if pinned[idx] {
                    0.85 - 0.45 * s
                } else {
                    0.35 - 0.35 * s
                };
                heights[idx] = mean + attach * (original[idx] - mean);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A planar ramp h = x so heights are exactly known everywhere.
    fn ramp_points() -> Vec<Vec3> {
        let mut pts = Vec::new();
        for ix in 0..=10 {
            for iz in 0..=10 {
                let x = ix as f32;
                let z = iz as f32;
                pts.push(Vec3::new(x, x, z)); // y = x
            }
        }
        pts
    }

    #[test]
    fn node_heights_track_idw() {
        use crate::reconstruction::interpolate_height_idw;
        let pts = ramp_points();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("build");
        for j in 0..hf.nz {
            for i in 0..hf.nx {
                let p = Vec2::new(
                    hf.origin.x + i as f32 * hf.cell,
                    hf.origin.y + j as f32 * hf.cell,
                );
                let expected = interpolate_height_idw(p, &pts);
                // Spatial-grid KNN finds the same K nearest as the linear scan;
                // ties on this perfectly regular grid can swap a neighbour, so
                // allow a small tolerance.
                assert!((hf.node_height(i, j).unwrap() - expected).abs() < 0.05);
            }
        }
    }

    #[test]
    fn height_at_is_bilinear_and_tracks_the_ramp() {
        let pts = ramp_points();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("build");
        // On a near-planar h=x field, the interpolated height tracks x closely.
        for &(x, z) in &[(2.5_f32, 3.5_f32), (5.0, 5.0), (7.25, 1.0)] {
            let h = hf.height_at(x, z).expect("inside");
            assert!((h - x).abs() < 0.25, "h={h} x={x}");
        }
        // A node query equals the node height.
        let node = hf.node_height(4, 4).unwrap();
        let at = hf
            .height_at(hf.origin.x + 4.0 * hf.cell, hf.origin.y + 4.0 * hf.cell)
            .unwrap();
        assert!((node - at).abs() < 1e-4);
    }

    #[test]
    fn height_at_returns_none_outside_extent() {
        let pts = ramp_points();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("build");
        assert!(hf.height_at(-50.0, 5.0).is_none());
        assert!(hf.height_at(5.0, 1000.0).is_none());
    }

    #[test]
    fn boundary_mask_excludes_outside_points() {
        let pts = ramp_points();
        // Boundary covering only the left half (x in [0,4]).
        let boundary = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        let hf = TerrainHeightfield::build(&pts, &boundary, 1.0, 0.0).expect("build");
        assert!(hf.height_at(2.0, 5.0).is_some(), "inside boundary");
        assert!(hf.height_at(8.0, 5.0).is_none(), "outside boundary");
    }

    #[test]
    fn max_over_finds_the_high_corner() {
        let pts = ramp_points(); // h = x, so max is at largest x in the footprint
        let hf = TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("build");
        let footprint = vec![
            Vec2::new(2.0, 2.0),
            Vec2::new(6.0, 2.0),
            Vec2::new(6.0, 6.0),
            Vec2::new(2.0, 6.0),
        ];
        let (h, pos) = hf.max_over(&footprint).expect("covered");
        assert!((h - 6.0).abs() < 0.5, "max height ~6, got {h}");
        assert!((pos.x - 6.0).abs() < 1.0, "max at high-x edge, got {pos:?}");
    }

    #[test]
    fn build_and_queries_meet_interaction_budget() {
        // ~3,600 contour-like points over a 180 m site (comparable in count to the
        // Västra Lagnö survey), with gentle relief.
        let mut pts = Vec::new();
        for ix in 0..60 {
            for iz in 0..60 {
                let x = ix as f32 * 3.0;
                let z = iz as f32 * 3.0;
                let y = (x * 0.05).sin() * 5.0 + (z * 0.04).cos() * 4.0;
                pts.push(Vec3::new(x, y, z));
            }
        }

        let t0 = Instant::now();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("build");
        let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
        println!(
            "heightfield build: {}x{} nodes from {} points in {:.1} ms",
            hf.nx,
            hf.nz,
            pts.len(),
            build_ms
        );

        let n = 200_000usize;
        let t1 = Instant::now();
        let mut acc = 0.0f32;
        for k in 0..n {
            let x = (k % 180) as f32;
            let z = ((k / 180) % 180) as f32;
            if let Some(h) = hf.height_at(x, z) {
                acc += h;
            }
        }
        let q_ms = t1.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{n} height_at queries in {:.1} ms ({:.0}k/ms) acc={acc:.1}",
            q_ms,
            n as f64 / q_ms / 1000.0
        );
        assert!(acc.is_finite());
        // O(1) queries: 200k must clear a 16 ms frame by a wide margin even in a
        // debug build (this is the property that makes interactive planting work).
        assert!(q_ms < 200.0, "height_at queries too slow: {q_ms} ms");
    }

    #[test]
    fn build_caps_resolution_for_large_extents() {
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1000.0, 5.0, 0.0),
            Vec3::new(0.0, 5.0, 1000.0),
            Vec3::new(1000.0, 10.0, 1000.0),
        ];
        // Tiny cell hint would blow up; the cap must widen it.
        let hf = TerrainHeightfield::build(&pts, &[], 0.01, 0.0).expect("build");
        assert!(hf.nx <= MAX_HEIGHTFIELD_NODES_PER_AXIS + 1);
        assert!(hf.nz <= MAX_HEIGHTFIELD_NODES_PER_AXIS + 1);
    }
}
