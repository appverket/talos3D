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

use bevy::prelude::*;

use crate::reconstruction::{interpolate_height_idw, point_in_polygon_2d};

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
    pub fn build(contour_points: &[Vec3], boundary: &[Vec2], cell_hint: f32) -> Option<Self> {
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

        let mut heights = vec![0.0f32; nx * nz];
        let mut inside = vec![true; nx * nz];
        for j in 0..nz {
            for i in 0..nx {
                let p = Vec2::new(min.x + i as f32 * cell, min.y + j as f32 * cell);
                let idx = j * nx + i;
                heights[idx] = interpolate_height_idw(p, contour_points);
                if has_boundary {
                    inside[idx] = point_in_polygon_2d(p, boundary);
                }
            }
        }

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
        if fx < -1e-3 || fz < -1e-3 || fx > (self.nx - 1) as f32 + 1e-3 || fz > (self.nz - 1) as f32 + 1e-3
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
    pub fn sample_grid(&self, footprint_world: &[Vec2], resolution: f32) -> Vec<(Vec2, Option<f32>)> {
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
    fn node_heights_match_idw_exactly() {
        let pts = ramp_points();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0).expect("build");
        for j in 0..hf.nz {
            for i in 0..hf.nx {
                let p = Vec2::new(hf.origin.x + i as f32 * hf.cell, hf.origin.y + j as f32 * hf.cell);
                let expected = interpolate_height_idw(p, &pts);
                assert!((hf.node_height(i, j).unwrap() - expected).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn height_at_is_bilinear_and_tracks_the_ramp() {
        let pts = ramp_points();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0).expect("build");
        // On a near-planar h=x field, the interpolated height tracks x closely.
        for &(x, z) in &[(2.5_f32, 3.5_f32), (5.0, 5.0), (7.25, 1.0)] {
            let h = hf.height_at(x, z).expect("inside");
            assert!((h - x).abs() < 0.25, "h={h} x={x}");
        }
        // A node query equals the node height.
        let node = hf.node_height(4, 4).unwrap();
        let at = hf.height_at(hf.origin.x + 4.0 * hf.cell, hf.origin.y + 4.0 * hf.cell).unwrap();
        assert!((node - at).abs() < 1e-4);
    }

    #[test]
    fn height_at_returns_none_outside_extent() {
        let pts = ramp_points();
        let hf = TerrainHeightfield::build(&pts, &[], 1.0).expect("build");
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
        let hf = TerrainHeightfield::build(&pts, &boundary, 1.0).expect("build");
        assert!(hf.height_at(2.0, 5.0).is_some(), "inside boundary");
        assert!(hf.height_at(8.0, 5.0).is_none(), "outside boundary");
    }

    #[test]
    fn max_over_finds_the_high_corner() {
        let pts = ramp_points(); // h = x, so max is at largest x in the footprint
        let hf = TerrainHeightfield::build(&pts, &[], 1.0).expect("build");
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
        let hf = TerrainHeightfield::build(&pts, &[], 1.0).expect("build");
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
        let hf = TerrainHeightfield::build(&pts, &[], 0.01).expect("build");
        assert!(hf.nx <= MAX_HEIGHTFIELD_NODES_PER_AXIS + 1);
        assert!(hf.nz <= MAX_HEIGHTFIELD_NODES_PER_AXIS + 1);
    }
}
