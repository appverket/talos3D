/// Section fill system: plane-mesh intersection, hatch patterns, and material inference.
///
/// When a clipping plane cuts through geometry, this module computes the 2D
/// intersection polygons and assigns hatch patterns based on the material
/// (inferred from name or explicit metadata).
///
/// Hatch patterns follow architectural drafting conventions:
/// - Concrete: diagonal lines at 45°
/// - Wood: grain lines (parallel with alternating spacing)
/// - Steel/Metal: crosshatch at 45°
/// - Insulation: wavy/zigzag (approximated as alternating diagonals)
/// - Earth/Soil: dots/stipple (approximated as fine crosshatch at 30°)
/// - Glass: no fill (outline only)
/// - Masonry/Brick: diagonal lines at 45° with wider spacing
/// - Generic solid: solid fill
use bevy::{
    mesh::{Indices, VertexAttributeValues},
    prelude::*,
};
use serde::{Deserialize, Serialize};

use crate::curation::MaterialSpecRegistry;
use crate::plugins::{
    clipping_planes::ClipPlaneNode,
    materials::{MaterialAssignment, MaterialDef, MaterialRegistry},
};

// ─── Hatch pattern types ─────────────────────────────────────────────────────

/// Architectural section hatch pattern.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum HatchPattern {
    /// Diagonal lines at a given angle. Standard for concrete.
    DiagonalLines { angle_deg: f32, spacing_mm: f32 },
    /// Two sets of diagonal lines crossing. Standard for steel/metal.
    Crosshatch { angle_deg: f32, spacing_mm: f32 },
    /// Parallel lines with alternating wide/narrow gaps. Standard for wood grain.
    WoodGrain { angle_deg: f32, spacing_mm: f32 },
    /// Solid fill colour. For generic materials or when pattern is not needed.
    SolidFill,
    /// Outline only — no fill. Standard for glass.
    NoFill,
}

impl Default for HatchPattern {
    fn default() -> Self {
        Self::DiagonalLines {
            angle_deg: 45.0,
            spacing_mm: 3.0,
        }
    }
}

/// A resolved section fill for one cut region.
#[derive(Debug, Clone)]
pub struct SectionFillRegion {
    /// Closed polygon in 3D world space (on the clip plane).
    pub polygon_3d: Vec<Vec3>,
    /// The hatch pattern to apply.
    pub pattern: HatchPattern,
    /// Fill colour (typically derived from material base_color, darkened).
    pub fill_color: [f32; 4],
    /// Material name for debugging/display.
    pub material_name: Option<String>,
}

/// A projected section fill for vector export.
#[derive(Debug, Clone)]
pub struct ProjectedSectionFill {
    /// Closed polygon in 2D viewport space.
    pub polygon: Vec<[f32; 2]>,
    /// The hatch pattern to apply.
    pub pattern: HatchPattern,
    /// Fill colour as RGBA.
    pub fill_color: [f32; 4],
}

// ─── Material to hatch pattern inference ─────────────────────────────────────

/// Infer a hatch pattern from a material name using architectural conventions.
///
/// Matches common material name substrings (case-insensitive) to standard
/// drafting patterns. Falls back to diagonal lines if no match is found.
pub fn infer_hatch_from_name(name: &str) -> HatchPattern {
    let lower = name.to_ascii_lowercase();

    // Glass / glazing → no fill
    if lower.contains("glass") || lower.contains("glaz") || lower.contains("window") {
        return HatchPattern::NoFill;
    }

    // Steel / metal / aluminum → crosshatch
    if lower.contains("steel")
        || lower.contains("metal")
        || lower.contains("iron")
        || lower.contains("alumi")
        || lower.contains("copper")
        || lower.contains("zinc")
        || lower.contains("brass")
    {
        return HatchPattern::Crosshatch {
            angle_deg: 45.0,
            spacing_mm: 2.0,
        };
    }

    // Wood / timber → wood grain
    if lower.contains("wood")
        || lower.contains("timber")
        || lower.contains("cedar")
        || lower.contains("pine")
        || lower.contains("oak")
        || lower.contains("birch")
        || lower.contains("plywood")
        || lower.contains("lumber")
        || lower.contains("maple")
        || lower.contains("walnut")
        || lower.contains("teak")
        || lower.contains("mahogany")
    {
        return HatchPattern::WoodGrain {
            angle_deg: 0.0,
            spacing_mm: 2.5,
        };
    }

    // Insulation → crosshatch at 30° with wide spacing
    if lower.contains("insul") || lower.contains("mineral wool") || lower.contains("rockwool") {
        return HatchPattern::Crosshatch {
            angle_deg: 30.0,
            spacing_mm: 5.0,
        };
    }

    // Earth / soil / gravel → fine crosshatch
    if lower.contains("earth")
        || lower.contains("soil")
        || lower.contains("gravel")
        || lower.contains("sand")
        || lower.contains("ground")
    {
        return HatchPattern::Crosshatch {
            angle_deg: 30.0,
            spacing_mm: 1.5,
        };
    }

    // Brick / masonry → diagonal with wider spacing
    if lower.contains("brick")
        || lower.contains("masonry")
        || lower.contains("block")
        || lower.contains("stone")
    {
        return HatchPattern::DiagonalLines {
            angle_deg: 45.0,
            spacing_mm: 4.0,
        };
    }

    // Concrete / cement → standard diagonal
    if lower.contains("concret") || lower.contains("cement") || lower.contains("mortar") {
        return HatchPattern::DiagonalLines {
            angle_deg: 45.0,
            spacing_mm: 3.0,
        };
    }

    // Gypsum / drywall / plaster → light diagonal
    if lower.contains("gypsum")
        || lower.contains("drywall")
        || lower.contains("plaster")
        || lower.contains("plasterboard")
    {
        return HatchPattern::DiagonalLines {
            angle_deg: 45.0,
            spacing_mm: 5.0,
        };
    }

    // Default: diagonal lines at 45°
    HatchPattern::DiagonalLines {
        angle_deg: 45.0,
        spacing_mm: 3.0,
    }
}

/// Resolve the hatch pattern for a material, checking explicit metadata first,
/// then falling back to name-based inference.
pub fn resolve_hatch_pattern(material: &MaterialDef) -> HatchPattern {
    // Could later check material.section_pattern field if added
    infer_hatch_from_name(&material.name)
}

/// Resolve a default fill colour from a material definition.
///
/// Uses a darkened version of the base colour for better contrast on white paper.
pub fn resolve_fill_color(material: &MaterialDef) -> [f32; 4] {
    let [r, g, b, _a] = material.base_color;
    // Darken to 40% intensity for section fill on white paper
    [r * 0.4, g * 0.4, b * 0.4, 0.3]
}

/// Default hatch pattern and colour when no material is assigned.
pub fn default_section_fill() -> (HatchPattern, [f32; 4]) {
    (
        HatchPattern::DiagonalLines {
            angle_deg: 45.0,
            spacing_mm: 3.0,
        },
        [0.3, 0.3, 0.3, 0.3],
    )
}

// ─── Plane-mesh intersection ─────────────────────────────────────────────────

/// Compute the intersection of a clip plane with a mesh, producing closed
/// polygon loops in 3D world space.
///
/// Algorithm: for each triangle, test which edges cross the plane. For
/// triangles that straddle the plane, compute the two intersection points.
/// Then chain these edge segments into closed loops.
pub fn intersect_plane_mesh(
    plane_origin: Vec3,
    plane_normal: Vec3,
    mesh: &Mesh,
    mesh_transform: &GlobalTransform,
) -> Vec<Vec<Vec3>> {
    let Some(positions) = mesh_positions(mesh) else {
        return Vec::new();
    };
    let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
        return Vec::new();
    };

    let mut segments: Vec<(Vec3, Vec3)> = Vec::new();

    for tri in indices.chunks(3) {
        if tri.len() < 3 {
            continue;
        }
        let (Some(a), Some(b), Some(c)) = (
            positions.get(tri[0] as usize).copied(),
            positions.get(tri[1] as usize).copied(),
            positions.get(tri[2] as usize).copied(),
        ) else {
            continue;
        };
        let wa = mesh_transform.transform_point(Vec3::from(a));
        let wb = mesh_transform.transform_point(Vec3::from(b));
        let wc = mesh_transform.transform_point(Vec3::from(c));

        if let Some(seg) = triangle_plane_intersection(wa, wb, wc, plane_origin, plane_normal) {
            segments.push(seg);
        }
    }

    chain_segments_into_loops(segments)
}

/// Find the intersection of a single triangle with a plane.
///
/// Returns `Some((p1, p2))` if the triangle straddles the plane, where p1 and
/// p2 are the two intersection points on the triangle edges.
fn triangle_plane_intersection(
    a: Vec3,
    b: Vec3,
    c: Vec3,
    plane_origin: Vec3,
    plane_normal: Vec3,
) -> Option<(Vec3, Vec3)> {
    let da = (a - plane_origin).dot(plane_normal);
    let db = (b - plane_origin).dot(plane_normal);
    let dc = (c - plane_origin).dot(plane_normal);

    // Collect intersection points from edges that cross the plane
    let mut points = Vec::with_capacity(2);

    if let Some(p) = edge_plane_intersection(a, b, da, db) {
        points.push(p);
    }
    if let Some(p) = edge_plane_intersection(b, c, db, dc) {
        points.push(p);
    }
    if points.len() < 2 {
        if let Some(p) = edge_plane_intersection(c, a, dc, da) {
            points.push(p);
        }
    }

    if points.len() >= 2 {
        Some((points[0], points[1]))
    } else {
        None
    }
}

/// Intersect an edge (v0→v1) with a plane, given signed distances d0 and d1.
fn edge_plane_intersection(v0: Vec3, v1: Vec3, d0: f32, d1: f32) -> Option<Vec3> {
    // Edge crosses plane if signs differ (and neither is exactly zero,
    // but we treat epsilon as crossing)
    if d0 * d1 >= 0.0 {
        return None;
    }
    let t = d0 / (d0 - d1);
    Some(v0 + (v1 - v0) * t)
}

/// Chain unordered line segments into closed polygon loops.
///
/// Uses a greedy nearest-endpoint approach with a distance tolerance.
fn chain_segments_into_loops(segments: Vec<(Vec3, Vec3)>) -> Vec<Vec<Vec3>> {
    if segments.is_empty() {
        return Vec::new();
    }

    let epsilon_sq = 1e-6_f32;
    let mut remaining: Vec<(Vec3, Vec3)> = segments;
    let mut loops = Vec::new();

    while let Some(seed) = remaining.pop() {
        let mut chain = vec![seed.0, seed.1];
        let mut changed = true;

        while changed {
            changed = false;
            let tail = *chain.last().unwrap();
            let head = chain[0];

            // Try to extend the tail
            let mut best_idx = None;
            let mut best_dist = f32::MAX;
            for (i, seg) in remaining.iter().enumerate() {
                let d0 = tail.distance_squared(seg.0);
                let d1 = tail.distance_squared(seg.1);
                let min_d = d0.min(d1);
                if min_d < best_dist {
                    best_dist = min_d;
                    best_idx = Some((i, d0 <= d1));
                }
            }

            if best_dist < epsilon_sq {
                if let Some((idx, start_matches)) = best_idx {
                    let seg = remaining.swap_remove(idx);
                    if start_matches {
                        chain.push(seg.1);
                    } else {
                        chain.push(seg.0);
                    }
                    changed = true;
                }
            }

            // Check if the loop is closed
            if chain.len() >= 3 && head.distance_squared(tail) < epsilon_sq {
                break;
            }
        }

        // Close the loop if endpoints are close
        if chain.len() >= 3 {
            loops.push(chain);
        }
    }

    loops
}

// ─── Section fill extraction from ECS ────────────────────────────────────────

/// Extract section fill regions from the world for all active clip planes.
pub fn extract_section_fills(world: &World, mesh_assets: &Assets<Mesh>) -> Vec<SectionFillRegion> {
    let material_registry = world.get_resource::<MaterialRegistry>();

    let clip_query = world.try_query::<&ClipPlaneNode>();
    let Some(mut clip_query) = clip_query else {
        return Vec::new();
    };
    let active_planes: Vec<ClipPlaneNode> = clip_query
        .iter(world)
        .filter(|p| p.active)
        .cloned()
        .collect();

    if active_planes.is_empty() {
        return Vec::new();
    }

    let mesh_query = world.try_query::<(
        Entity,
        &Mesh3d,
        &GlobalTransform,
        Option<&Visibility>,
        Option<&MaterialAssignment>,
    )>();
    let Some(mut mesh_query) = mesh_query else {
        return Vec::new();
    };

    let mut fills = Vec::new();

    for (_entity, mesh_handle, mesh_transform, visibility, mat_assign) in mesh_query.iter(world) {
        if visibility.is_some_and(|v| *v == Visibility::Hidden) {
            continue;
        }

        let Some(mesh) = mesh_assets.get(&mesh_handle.0) else {
            continue;
        };

        // Resolve material → hatch pattern
        let (pattern, fill_color, mat_name) =
            resolve_entity_section_style(mat_assign, material_registry, world.get_resource());

        if matches!(pattern, HatchPattern::NoFill) {
            continue;
        }

        for plane in &active_planes {
            let loops = intersect_plane_mesh(plane.origin, plane.normal, mesh, mesh_transform);
            for polygon in loops {
                if polygon.len() >= 3 {
                    fills.push(SectionFillRegion {
                        polygon_3d: polygon,
                        pattern,
                        fill_color,
                        material_name: mat_name.clone(),
                    });
                }
            }
        }
    }

    fills
}

fn resolve_entity_section_style(
    mat_assign: Option<&MaterialAssignment>,
    registry: Option<&MaterialRegistry>,
    specs: Option<&MaterialSpecRegistry>,
) -> (HatchPattern, [f32; 4], Option<String>) {
    if let (Some(assign), Some(reg)) = (mat_assign, registry) {
        if let Some(material_id) = assign.render_material_id(specs) {
            if let Some(mat) = reg.get(&material_id) {
                let pattern = resolve_hatch_pattern(mat);
                let color = resolve_fill_color(mat);
                return (pattern, color, Some(mat.name.clone()));
            }
        }
    }
    let (pattern, color) = default_section_fill();
    (pattern, color, None)
}

// ─── Hatch line generation ───────────────────────────────────────────────────

/// Generate hatch lines within a 2D polygon for a given pattern.
///
/// Returns a list of line segments (x1,y1,x2,y2) clipped to the polygon boundary.
pub fn generate_hatch_lines(
    polygon: &[[f32; 2]],
    pattern: HatchPattern,
    viewport_scale: f32,
) -> Vec<[f32; 4]> {
    match pattern {
        HatchPattern::NoFill | HatchPattern::SolidFill => Vec::new(),
        HatchPattern::DiagonalLines {
            angle_deg,
            spacing_mm,
        } => generate_parallel_hatch(polygon, angle_deg, spacing_mm * viewport_scale),
        HatchPattern::Crosshatch {
            angle_deg,
            spacing_mm,
        } => {
            let mut lines =
                generate_parallel_hatch(polygon, angle_deg, spacing_mm * viewport_scale);
            lines.extend(generate_parallel_hatch(
                polygon,
                angle_deg + 90.0,
                spacing_mm * viewport_scale,
            ));
            lines
        }
        HatchPattern::WoodGrain {
            angle_deg,
            spacing_mm,
        } => {
            // Alternating narrow and wide spacing to simulate wood grain
            let narrow = spacing_mm * viewport_scale * 0.5;
            let wide = spacing_mm * viewport_scale * 1.5;
            generate_alternating_hatch(polygon, angle_deg, narrow, wide)
        }
    }
}

/// Generate parallel hatch lines through a polygon at a given angle and spacing.
fn generate_parallel_hatch(polygon: &[[f32; 2]], angle_deg: f32, spacing: f32) -> Vec<[f32; 4]> {
    if polygon.len() < 3 || spacing <= 0.0 {
        return Vec::new();
    }

    let angle = angle_deg.to_radians();
    let cos_a = angle.cos();
    let sin_a = angle.sin();

    // Direction perpendicular to hatch lines (used to measure distance)
    let perp = [cos_a, sin_a];

    // Project all polygon vertices onto the perpendicular direction
    let projections: Vec<f32> = polygon
        .iter()
        .map(|v| v[0] * perp[0] + v[1] * perp[1])
        .collect();
    let min_proj = projections.iter().copied().fold(f32::INFINITY, f32::min);
    let max_proj = projections
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);

    // Hatch line direction
    let dir = [-sin_a, cos_a];

    let mut lines = Vec::new();
    let mut d = min_proj + spacing * 0.5;
    while d < max_proj {
        // Find intersections of the hatch line at distance `d` with polygon edges
        if let Some(seg) = clip_line_to_polygon(polygon, d, perp, dir) {
            lines.push(seg);
        }
        d += spacing;
    }

    lines
}

/// Generate alternating narrow/wide spaced hatch lines (for wood grain).
fn generate_alternating_hatch(
    polygon: &[[f32; 2]],
    angle_deg: f32,
    narrow: f32,
    wide: f32,
) -> Vec<[f32; 4]> {
    if polygon.len() < 3 || narrow <= 0.0 || wide <= 0.0 {
        return Vec::new();
    }

    let angle = angle_deg.to_radians();
    let cos_a = angle.cos();
    let sin_a = angle.sin();
    let perp = [cos_a, sin_a];
    let dir = [-sin_a, cos_a];

    let projections: Vec<f32> = polygon
        .iter()
        .map(|v| v[0] * perp[0] + v[1] * perp[1])
        .collect();
    let min_proj = projections.iter().copied().fold(f32::INFINITY, f32::min);
    let max_proj = projections
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);

    let mut lines = Vec::new();
    let mut d = min_proj + narrow * 0.5;
    let mut use_narrow = true;
    while d < max_proj {
        if let Some(seg) = clip_line_to_polygon(polygon, d, perp, dir) {
            lines.push(seg);
        }
        d += if use_narrow { narrow } else { wide };
        use_narrow = !use_narrow;
    }

    lines
}

/// Clip an infinite line (defined by perpendicular offset `d` along `perp`,
/// running in direction `dir`) to the polygon boundary.
///
/// Returns the clipped segment as `[x1, y1, x2, y2]`, or `None` if no intersection.
fn clip_line_to_polygon(
    polygon: &[[f32; 2]],
    d: f32,
    perp: [f32; 2],
    dir: [f32; 2],
) -> Option<[f32; 4]> {
    let n = polygon.len();
    let mut intersections: Vec<f32> = Vec::new();

    for i in 0..n {
        let j = (i + 1) % n;
        let vi = polygon[i];
        let vj = polygon[j];

        let di = vi[0] * perp[0] + vi[1] * perp[1] - d;
        let dj = vj[0] * perp[0] + vj[1] * perp[1] - d;

        if di * dj < 0.0 {
            let t = di / (di - dj);
            let ix = vi[0] + t * (vj[0] - vi[0]);
            let iy = vi[1] + t * (vj[1] - vi[1]);
            // Project intersection onto the hatch line direction
            let param = ix * dir[0] + iy * dir[1];
            intersections.push(param);
        }
    }

    if intersections.len() < 2 {
        return None;
    }

    intersections.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Use the first and last intersection for the overall span
    // (handles convex and some concave cases)
    let t_min = intersections[0];
    let t_max = *intersections.last().unwrap();

    if (t_max - t_min).abs() < 1e-6 {
        return None;
    }

    // Reconstruct the 2D endpoints from the line parameterisation
    // A point on the line at parameter `d` along perp, `t` along dir:
    //   P = perp * d + dir * t  (conceptually, but we need the actual point)
    // Actually we need to reconstruct from the origin. The line is:
    //   P(t) = base_point + dir * t
    // where base_point is any point on the line. We can pick:
    //   base = perp * d (projected from origin)
    let base_x = perp[0] * d;
    let base_y = perp[1] * d;

    Some([
        base_x + dir[0] * t_min,
        base_y + dir[1] * t_min,
        base_x + dir[0] * t_max,
        base_y + dir[1] * t_max,
    ])
}

// ─── Mesh helpers ────────────────────────────────────────────────────────────

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
        None if vertex_count % 3 == 0 => Some((0..vertex_count as u32).collect()),
        None => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_plane_intersection_horizontal_cut() {
        // Triangle: (0,0,0), (1,0,0), (0.5, 2, 0) cut by Y=1 plane
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(1.0, 0.0, 0.0);
        let c = Vec3::new(0.5, 2.0, 0.0);
        let origin = Vec3::new(0.0, 1.0, 0.0);
        let normal = Vec3::Y;

        let result = triangle_plane_intersection(a, b, c, origin, normal);
        assert!(result.is_some());
        let (p1, p2) = result.unwrap();
        // At y=1, the intersection should be between edges a-c and b-c
        assert!((p1.y - 1.0).abs() < 0.01);
        assert!((p2.y - 1.0).abs() < 0.01);
    }

    #[test]
    fn triangle_plane_no_intersection_when_all_above() {
        let a = Vec3::new(0.0, 2.0, 0.0);
        let b = Vec3::new(1.0, 3.0, 0.0);
        let c = Vec3::new(0.5, 4.0, 0.0);
        let origin = Vec3::new(0.0, 1.0, 0.0);
        let normal = Vec3::Y;

        let result = triangle_plane_intersection(a, b, c, origin, normal);
        assert!(result.is_none());
    }

    #[test]
    fn chain_segments_forms_loop() {
        let segments = vec![
            (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)),
            (Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 0.0)),
            (Vec3::new(1.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 0.0)),
        ];
        let loops = chain_segments_into_loops(segments);
        assert_eq!(loops.len(), 1);
        assert!(loops[0].len() >= 3);
    }

    #[test]
    fn infer_hatch_concrete() {
        let p = infer_hatch_from_name("Reinforced Concrete");
        assert!(
            matches!(p, HatchPattern::DiagonalLines { angle_deg, .. } if (angle_deg - 45.0).abs() < 0.01)
        );
    }

    #[test]
    fn infer_hatch_steel() {
        let p = infer_hatch_from_name("Structural Steel");
        assert!(matches!(p, HatchPattern::Crosshatch { .. }));
    }

    #[test]
    fn infer_hatch_wood() {
        let p = infer_hatch_from_name("Red Cedar Plywood");
        assert!(matches!(p, HatchPattern::WoodGrain { .. }));
    }

    #[test]
    fn infer_hatch_glass() {
        let p = infer_hatch_from_name("Blue Tint Glazing");
        assert!(matches!(p, HatchPattern::NoFill));
    }

    #[test]
    fn infer_hatch_insulation() {
        let p = infer_hatch_from_name("Mineral Wool Insulation");
        assert!(
            matches!(p, HatchPattern::Crosshatch { angle_deg, .. } if (angle_deg - 30.0).abs() < 0.01)
        );
    }

    #[test]
    fn infer_hatch_default() {
        let p = infer_hatch_from_name("Unknown Material XYZ");
        assert!(matches!(p, HatchPattern::DiagonalLines { .. }));
    }

    #[test]
    fn parallel_hatch_generates_lines_in_square() {
        let square = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let lines = generate_parallel_hatch(&square, 0.0, 2.0);
        // Should generate ~5 horizontal lines across the 10-unit square
        assert!(lines.len() >= 4);
        assert!(lines.len() <= 6);
    }

    #[test]
    fn crosshatch_generates_double_lines() {
        let square = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let diagonal_only = generate_parallel_hatch(&square, 45.0, 3.0);
        let crosshatch = generate_hatch_lines(
            &square,
            HatchPattern::Crosshatch {
                angle_deg: 45.0,
                spacing_mm: 3.0,
            },
            1.0,
        );
        // Crosshatch should have roughly twice as many lines as single direction
        assert!(crosshatch.len() > diagonal_only.len());
    }

    #[test]
    fn edge_plane_intersection_computes_midpoint() {
        let v0 = Vec3::new(0.0, 0.0, 0.0);
        let v1 = Vec3::new(0.0, 2.0, 0.0);
        let result = edge_plane_intersection(v0, v1, -1.0, 1.0);
        assert!(result.is_some());
        let p = result.unwrap();
        assert!((p.y - 1.0).abs() < 0.01);
    }

    #[test]
    fn edge_plane_no_intersection_same_side() {
        let v0 = Vec3::new(0.0, 2.0, 0.0);
        let v1 = Vec3::new(0.0, 3.0, 0.0);
        let result = edge_plane_intersection(v0, v1, 1.0, 2.0);
        assert!(result.is_none());
    }

    #[test]
    fn solid_fill_generates_no_hatch_lines() {
        let square = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let lines = generate_hatch_lines(&square, HatchPattern::SolidFill, 1.0);
        assert!(lines.is_empty());
    }

    #[test]
    fn no_fill_generates_no_hatch_lines() {
        let square = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let lines = generate_hatch_lines(&square, HatchPattern::NoFill, 1.0);
        assert!(lines.is_empty());
    }
}
