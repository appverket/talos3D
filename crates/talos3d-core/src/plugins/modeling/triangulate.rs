/// 2D ear-clipping polygon triangulation.
///
/// Converts a simple, CCW-wound polygon into a set of triangles suitable for
/// use as polygon caps on extruded profiles.  The algorithm runs in O(n²) time
/// which is perfectly acceptable for the profile sizes that appear in practice
/// (typically < 200 vertices after arc tessellation).
///
/// # Algorithm
///
/// 1. Build a doubly-linked ring of vertex indices.
/// 2. Classify each vertex as *convex* (left-turn at that vertex, given CCW
///    winding) or *reflex* (right-turn).
/// 3. Repeat until only 3 vertices remain:
///    - Find the first vertex that is an **ear**: convex, and the triangle
///      formed by its predecessor, itself, and its successor contains none of
///      the remaining reflex vertices.
///    - Emit that triangle, remove the vertex from the ring, and reclassify
///      its two neighbours.
use bevy::math::Vec2;

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

/// Triangulate a simple polygon using ear clipping.
///
/// Returns triangle indices into the input `points` array.  Each triple
/// `[a, b, c]` is wound counter-clockwise (matching the input winding).
///
/// # Assumptions
///
/// - `points` form a **simple** polygon (no self-intersections).
/// - Winding is **counter-clockwise** (positive signed area).
/// - Duplicate consecutive vertices and collinear runs are handled gracefully
///   — degenerate ears are skipped rather than emitted.
///
/// # Returns
///
/// An empty `Vec` is returned for degenerate input (fewer than 3 points or
/// fully collinear polygons).
#[must_use]
pub fn ear_clip_triangulate(points: &[Vec2]) -> Vec<[u32; 3]> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }

    // Degenerate: all points collinear.
    if signed_area(points).abs() < f32::EPSILON {
        return Vec::new();
    }

    // Build the doubly-linked ring.  `prev[i]` and `next[i]` hold the
    // predecessor / successor indices in the current ring.
    let mut prev = (0..n)
        .map(|i| if i == 0 { n - 1 } else { i - 1 })
        .collect::<Vec<_>>();
    let mut next = (0..n).map(|i| (i + 1) % n).collect::<Vec<_>>();

    // Pre-classify every vertex.
    let mut is_ear = vec![false; n];
    let mut active = n; // number of vertices still in the ring

    // Initial ear classification pass.
    for i in 0..n {
        is_ear[i] = test_ear(i, points, &prev, &next);
    }

    let mut triangles = Vec::with_capacity(n.saturating_sub(2));
    let mut cursor = 0usize; // walk the ring from here

    // Guard against infinite loops on truly degenerate input.
    let max_iterations = n * n + n;
    let mut iterations = 0usize;

    while active > 3 {
        iterations += 1;
        if iterations > max_iterations {
            // Degenerate polygon that the algorithm cannot reduce — bail.
            break;
        }

        if is_ear[cursor] {
            let p = prev[cursor];
            let nx = next[cursor];

            // Ensure the ear is non-degenerate before emitting it.
            if !degenerate_triangle(points[p], points[cursor], points[nx]) {
                triangles.push([p as u32, cursor as u32, nx as u32]);
            }

            // Remove vertex from ring.
            next[p] = nx;
            prev[nx] = p;
            active -= 1;

            // Reclassify the two neighbours.
            is_ear[p] = test_ear(p, points, &prev, &next);
            is_ear[nx] = test_ear(nx, points, &prev, &next);

            // Advance cursor to next active vertex (not the one we just removed).
            cursor = nx;
        } else {
            cursor = next[cursor];
        }
    }

    // Emit the final triangle if it is non-degenerate.
    if active == 3 {
        let a = cursor;
        let b = next[a];
        let c = next[b];
        if !degenerate_triangle(points[a], points[b], points[c]) {
            triangles.push([a as u32, b as u32, c as u32]);
        }
    }

    triangles
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Signed area of the polygon (positive ⟹ CCW).
fn signed_area(pts: &[Vec2]) -> f32 {
    let n = pts.len();
    let mut area = 0.0f32;
    for i in 0..n {
        let j = (i + 1) % n;
        area += pts[i].x * pts[j].y;
        area -= pts[j].x * pts[i].y;
    }
    area * 0.5
}

/// 2D cross product (z-component of the 3D cross product of two 2D vectors).
///
/// Positive means the turn from `(o→a)` to `(o→b)` is counter-clockwise.
#[inline]
fn cross2(o: Vec2, a: Vec2, b: Vec2) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

/// A triangle is degenerate when its area is effectively zero (collinear
/// vertices or duplicate points).
#[inline]
fn degenerate_triangle(a: Vec2, b: Vec2, c: Vec2) -> bool {
    cross2(a, b, c).abs() < 1e-10
}

/// Test whether vertex `v` is an ear in the current ring state.
///
/// A vertex is an ear when:
/// 1. The interior angle at `v` is convex (cross product > 0 for CCW polygon).
/// 2. No other active vertex lies strictly inside the triangle
///    `prev[v] – v – next[v]`.
///
/// Collinear vertices (cross product exactly 0) are treated as *not* ears so
/// that we prefer properly triangulated output; they will be clipped when
/// their neighbours become ears.
fn test_ear(v: usize, points: &[Vec2], prev: &[usize], next: &[usize]) -> bool {
    let p = prev[v];
    let nx = next[v];

    let a = points[p];
    let b = points[v];
    let c = points[nx];

    // Must be a left-turn (convex vertex in CCW polygon).
    let cross = cross2(a, b, c);
    if cross <= 0.0 {
        return false;
    }

    // Check that no other active vertex lies inside or on the boundary of
    // triangle ABC.  Using a non-strict (inclusive) containment test is
    // critical: a reflex vertex that lies exactly on the diagonal `a–c` would
    // produce an overlapping triangulation if the ear were accepted.
    let mut cur = next[nx]; // start after the triangle vertices
    while cur != p {
        let pt = points[cur];
        if point_in_triangle_inclusive(pt, a, b, c) {
            return false;
        }
        cur = next[cur];
    }

    true
}

/// Returns `true` when `p` is inside or on the boundary of triangle `(a, b, c)`.
///
/// An inclusive test is used so that reflex vertices that lie exactly on a
/// proposed ear diagonal correctly invalidate that ear candidate.
#[inline]
fn point_in_triangle_inclusive(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let d0 = cross2(a, b, p);
    let d1 = cross2(b, c, p);
    let d2 = cross2(c, a, p);

    // All non-negative or all non-positive ⟹ inside or on boundary.
    (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Compute the signed area of a triangle given its three vertex positions.
    fn triangle_area(pts: &[Vec2], tri: [u32; 3]) -> f32 {
        let a = pts[tri[0] as usize];
        let b = pts[tri[1] as usize];
        let c = pts[tri[2] as usize];
        cross2(a, b, c) * 0.5
    }

    /// Assert that all emitted triangles have positive area (CCW) and that the
    /// total area matches the polygon area.
    fn assert_valid_triangulation(pts: &[Vec2], tris: &[[u32; 3]]) {
        let expected_tris = pts.len() - 2;
        assert_eq!(
            tris.len(),
            expected_tris,
            "expected {expected_tris} triangles for {}-gon, got {}",
            pts.len(),
            tris.len(),
        );

        let poly_area = signed_area(pts);
        let tri_area_sum: f32 = tris.iter().map(|&t| triangle_area(pts, t)).sum();

        assert!(
            (poly_area - tri_area_sum).abs() < 1e-4,
            "area mismatch: polygon {poly_area:.6} vs triangles {tri_area_sum:.6}",
        );

        for &tri in tris {
            let area = triangle_area(pts, tri);
            assert!(
                area > 0.0,
                "triangle {tri:?} has non-positive area {area:.8}",
            );
        }
    }

    #[test]
    fn unit_triangle() {
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
        ];
        let tris = ear_clip_triangulate(&pts);
        assert_valid_triangulation(&pts, &tris);
    }

    #[test]
    fn unit_square() {
        // CCW square
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let tris = ear_clip_triangulate(&pts);
        assert_valid_triangulation(&pts, &tris);
    }

    #[test]
    fn l_shape_concave() {
        // CCW L-shape (concave polygon)
        //  (0,2)──(1,2)
        //    │      │
        //  (0,1)  (1,1)──(2,1)
        //    │              │
        //  (0,0)──────────(2,0)
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 2.0),
            Vec2::new(0.0, 2.0),
        ];
        let tris = ear_clip_triangulate(&pts);
        assert_valid_triangulation(&pts, &tris);
    }

    #[test]
    fn arrow_head_concave() {
        // Pentagon with one concave notch (CCW):
        //
        //  (0,2)──────────(2,2)
        //    │    (1,1)     │
        //    │   (notch)    │
        //  (0,0)──────────(2,0)
        //
        // The notch at (1,1) makes vertex 3 reflex.
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(1.0, 1.0), // concave notch
            Vec2::new(0.0, 2.0),
        ];
        let tris = ear_clip_triangulate(&pts);
        assert_valid_triangulation(&pts, &tris);
    }

    #[test]
    fn collinear_points_on_edge() {
        // Square with an extra collinear vertex on the top edge — should still
        // produce a valid triangulation (one extra triangle).
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(1.0, 2.0), // collinear with neighbours
            Vec2::new(0.0, 2.0),
        ];
        let tris = ear_clip_triangulate(&pts);
        // 5 vertices → 3 triangles; area = 4.0
        assert_eq!(tris.len(), 3);
        let total: f32 = tris.iter().map(|&t| triangle_area(&pts, t)).sum();
        assert!((total - 4.0).abs() < 1e-4);
    }

    #[test]
    fn too_few_points_returns_empty() {
        assert!(ear_clip_triangulate(&[]).is_empty());
        assert!(ear_clip_triangulate(&[Vec2::ZERO]).is_empty());
        assert!(ear_clip_triangulate(&[Vec2::ZERO, Vec2::X]).is_empty());
    }

    #[test]
    fn fully_collinear_returns_empty() {
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(2.0, 0.0),
        ];
        assert!(ear_clip_triangulate(&pts).is_empty());
    }

    #[test]
    fn regular_hexagon() {
        use std::f32::consts::TAU;
        let pts: Vec<Vec2> = (0..6)
            .map(|i| {
                let angle = TAU * i as f32 / 6.0;
                Vec2::new(angle.cos(), angle.sin())
            })
            .collect();
        // Ensure CCW (it is by construction for counter-clockwise angle sweep).
        let tris = ear_clip_triangulate(&pts);
        assert_valid_triangulation(&pts, &tris);
    }

    #[test]
    fn thin_rectangle() {
        // Very thin rectangle — stress-tests near-degenerate ear detection.
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 0.001),
            Vec2::new(0.0, 0.001),
        ];
        let tris = ear_clip_triangulate(&pts);
        assert_valid_triangulation(&pts, &tris);
    }
}
