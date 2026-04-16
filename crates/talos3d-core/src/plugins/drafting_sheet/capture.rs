//! 3D world → 2D paper-mm capture.
//!
//! The entry point is [`capture_sheet`]: given the world and a [`SheetView`],
//! flatten every visible feature into a [`DraftingSheet`] in paper
//! millimetres. The sheet is then the *single source of truth* for every
//! downstream exporter (SVG / PDF / DXF / PNG).
//!
//! Ground rules this module upholds:
//!
//! 1. **Orthographic-only.** Perspective drafting views are meaningless;
//!    we refuse the view and return `None`.
//! 2. **Paper-mm is the only unit.** Every coordinate written into the
//!    sheet is paper mm. Size attributes on dimension primitives (tick
//!    length, text height, stroke weight, extension gap) are already paper
//!    mm from the drafting renderer — we feed it `world_to_paper = 1.0`
//!    because we've projected the world positions into paper-mm 2D
//!    ourselves before calling it.
//! 3. **Unit-audited at the boundary.** The capture pass is the only
//!    place in the pipeline where two different coordinate systems meet.
//!    If something is wrong, it's wrong here — never deep inside a writer.

use bevy::math::{Mat4, Vec2, Vec3};
use bevy::prelude::*;

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::{
    drafting::{
        self, DimPrimitive, DimensionAnnotationNode, DimensionInput, DimensionKind,
        DimensionStyleRegistry, DraftingVisibility,
    },
    section_fill::{extract_section_fills, SectionFillRegion},
    vector_drawing::{
        collect_active_clip_planes, collect_classified_visible_edges, collect_scene_triangles,
        drawing_overlay_excluded, EdgeType, MeshSubject,
    },
};

use super::sheet::{DraftingSheet, SheetHatch, SheetLine, SheetStroke, SheetView};

// ─── Public API ───────────────────────────────────────────────────────────

/// Flatten the 3D world into a paper-mm [`DraftingSheet`] for the given
/// [`SheetView`]. Returns `None` if no camera is active, the scene has no
/// mesh data, or the view is non-orthographic (drafting with perspective
/// is nonsensical — see PP69).
pub fn capture_sheet(world: &World, view: &SheetView) -> Option<DraftingSheet> {
    if !view.ortho_height_m.is_finite() || view.ortho_height_m <= 0.0 {
        return None;
    }
    if !view.aspect.is_finite() || view.aspect <= 0.0 {
        return None;
    }
    if !view.scale_denominator.is_finite() || view.scale_denominator <= 0.0 {
        return None;
    }

    let mesh_assets = world.get_resource::<Assets<Mesh>>()?;
    let registry = world.get_resource::<CapabilityRegistry>()?;

    // Camera frames.
    let (view_proj, camera_position, camera_forward) = build_view_proj(view);
    let paper_w = view.frustum_width_mm();
    let paper_h = view.frustum_height_mm();
    let ndc_to_paper = NdcToPaper { paper_w, paper_h };

    // 1) Collect visible mesh subjects and their triangles.
    let mut subject_query = world.try_query::<(
        Entity,
        &crate::plugins::identity::ElementId,
        &Mesh3d,
        &GlobalTransform,
        Option<&Visibility>,
    )>()?;
    let mut subjects = Vec::new();
    for (entity, _eid, mesh_handle, mesh_transform, visibility) in subject_query.iter(world) {
        if visibility.is_some_and(|v| *v == Visibility::Hidden) {
            continue;
        }
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        if drawing_overlay_excluded(snapshot.type_name()) {
            continue;
        }
        subjects.push(MeshSubject {
            entity,
            mesh_handle: mesh_handle.0.clone(),
            mesh_transform: *mesh_transform,
        });
    }
    let scene_triangles = collect_scene_triangles(&subjects, mesh_assets);
    // Active clip planes make "elevation beyond" visible — without
    // them, trusses and other geometry on the visible side of the
    // section are wrongly culled as occluded by the parts of the
    // enclosing meshes that have been cut away.
    let clip_planes = collect_active_clip_planes(world);

    let mut sheet = DraftingSheet::new(view.scale_denominator);

    // 2) Visible edges → paper-mm line segments, classified.
    for subject in &subjects {
        let Some(mesh) = mesh_assets.get(&subject.mesh_handle) else {
            continue;
        };
        let classified = collect_classified_visible_edges(
            mesh,
            &subject.mesh_transform,
            subject.entity,
            camera_position,
            camera_forward,
            true, // orthographic
            &scene_triangles,
            &clip_planes,
        );
        for (a_world, b_world, edge_type) in classified {
            if let (Some(a), Some(b)) = (
                project_world_to_paper(a_world, &view_proj, &ndc_to_paper),
                project_world_to_paper(b_world, &view_proj, &ndc_to_paper),
            ) {
                sheet.lines.push(SheetLine {
                    a,
                    b,
                    stroke: edge_stroke(edge_type),
                });
            }
        }
    }

    // 3) Clip-plane section fills → paper-mm polygons + section-cut outlines.
    let fill_regions = extract_section_fills(world, mesh_assets);
    for region in &fill_regions {
        if let Some(polygon_paper) = project_polygon(region, &view_proj, &ndc_to_paper) {
            // Outline the cut polygon with section-cut weight — heaviest
            // on the page by convention.
            for i in 0..polygon_paper.len() {
                let j = (i + 1) % polygon_paper.len();
                sheet.lines.push(SheetLine {
                    a: polygon_paper[i],
                    b: polygon_paper[j],
                    stroke: SheetStroke::SectionCut,
                });
            }
            sheet.hatches.push(SheetHatch {
                polygon: polygon_paper,
                pattern: region.pattern,
            });
        }
    }

    // 4) Rich drafting annotations — project to paper-mm 2D, then the
    //    renderer emits paper-mm primitives directly.
    sheet.annotations = capture_annotations(world, &view_proj, &ndc_to_paper);

    // 5) Finalise bounds (content bbox + view margin).
    sheet.recompute_bounds(view.margin_mm);

    Some(sheet)
}

/// Build a [`SheetView`] from the current 3D orbit camera in the world
/// (the "Front" view preset, at an explicit drawing scale). Returns
/// `None` if no orthographic camera is active.
pub fn sheet_view_from_active_camera(
    world: &World,
    scale_denominator: f32,
    margin_mm: f32,
) -> Option<SheetView> {
    use crate::plugins::camera::{CameraProjectionMode, OrbitCamera};

    let mut camera_query = world.try_query::<(&OrbitCamera, &GlobalTransform, &Projection)>()?;
    let (orbit, transform, projection) = camera_query.iter(world).next()?;

    // Drafting requires orthographic.
    if !matches!(orbit.projection_mode, CameraProjectionMode::Isometric) {
        return None;
    }
    let Projection::Orthographic(ortho) = projection else {
        return None;
    };

    let eye = transform.translation();
    let forward = transform.forward().as_vec3();
    let up = transform.up().as_vec3();
    // `orbit.radius` is the distance from the camera to its focus along
    // the view direction; pin target at that offset so the view spec
    // captures the same framing the user sees.
    let target = eye + forward * orbit.radius.max(0.0);

    let ortho_height_m = ortho.area.height().abs().max(1e-3);
    let aspect = if ortho.area.height().abs() > 1e-6 {
        (ortho.area.width() / ortho.area.height()).abs()
    } else {
        16.0 / 9.0
    };

    Some(SheetView {
        eye,
        target,
        up,
        ortho_height_m,
        aspect,
        scale_denominator,
        margin_mm,
    })
}

// ─── View-proj construction ──────────────────────────────────────────────

fn build_view_proj(view: &SheetView) -> (Mat4, Vec3, Vec3) {
    let view_matrix = Mat4::look_at_rh(view.eye, view.target, view.up);
    let half_h = view.ortho_height_m * 0.5;
    let half_w = half_h * view.aspect;
    // A comfortable orthographic depth range — large enough that geometry
    // at a normal architectural scene distance fits without clipping.
    let proj = Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, -10_000.0, 10_000.0);
    let camera_forward = (view.target - view.eye)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    (proj * view_matrix, view.eye, camera_forward)
}

struct NdcToPaper {
    paper_w: f32,
    paper_h: f32,
}

impl NdcToPaper {
    fn apply(&self, ndc: Vec3) -> Vec2 {
        // NDC is in +y-up. Paper is +y-up too (we do not flip here — the
        // writers own the y-down coordinate conventions of their formats).
        Vec2::new(
            (ndc.x * 0.5 + 0.5) * self.paper_w,
            (ndc.y * 0.5 + 0.5) * self.paper_h,
        )
    }
}

fn project_world_to_paper(point: Vec3, view_proj: &Mat4, map: &NdcToPaper) -> Option<Vec2> {
    let clip = *view_proj * point.extend(1.0);
    if clip.w.abs() < 1e-7 {
        return None;
    }
    Some(map.apply(clip.truncate() / clip.w))
}

fn project_polygon(
    region: &SectionFillRegion,
    view_proj: &Mat4,
    map: &NdcToPaper,
) -> Option<Vec<Vec2>> {
    let projected: Vec<Vec2> = region
        .polygon_3d
        .iter()
        .filter_map(|p| project_world_to_paper(*p, view_proj, map))
        .collect();
    if projected.len() >= 3 {
        Some(projected)
    } else {
        None
    }
}

fn edge_stroke(edge: EdgeType) -> SheetStroke {
    match edge {
        EdgeType::SectionCut => SheetStroke::SectionCut,
        EdgeType::Silhouette => SheetStroke::Silhouette,
        EdgeType::Crease => SheetStroke::Crease,
        EdgeType::Boundary => SheetStroke::Boundary,
        EdgeType::Dimension => SheetStroke::Dimension,
    }
}

// ─── Annotation capture ──────────────────────────────────────────────────

fn capture_annotations(
    world: &World,
    view_proj: &Mat4,
    map: &NdcToPaper,
) -> Vec<Vec<DimPrimitive>> {
    let Some(registry) = world.get_resource::<DimensionStyleRegistry>() else {
        return Vec::new();
    };
    let visibility = world
        .get_resource::<DraftingVisibility>()
        .cloned()
        .unwrap_or_default();
    if !visibility.show_all {
        return Vec::new();
    }
    let Some(mut q) = world.try_query::<&DimensionAnnotationNode>() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for node in q.iter(world) {
        if !node.visible || !visibility.is_visible(&node.style_name, node.kind.tag()) {
            continue;
        }
        let Some(a_paper) = project_world_to_paper(node.a, view_proj, map) else {
            continue;
        };
        let Some(b_paper) = project_world_to_paper(node.b, view_proj, map) else {
            continue;
        };

        // Project the offset *vector* by subtracting the midpoint, so we
        // preserve it as a paper-mm offset rather than an absolute
        // position. This keeps extension-line side choice meaningful in
        // 2D paper space.
        let mid_world = (node.a + node.b) * 0.5;
        let mid_plus_offset = mid_world + node.offset;
        let (Some(m_paper), Some(mp_paper)) = (
            project_world_to_paper(mid_world, view_proj, map),
            project_world_to_paper(mid_plus_offset, view_proj, map),
        ) else {
            continue;
        };
        let offset_paper = mp_paper - m_paper;

        // 2D dim → 3D Vec3 with z=0 so the drafting renderer can consume
        // it. We pass `world_to_paper = 1.0` because positions are
        // already paper-mm; sizes (tick length, text height, etc.) stay
        // in paper-mm and land in the same unit as positions. No
        // rescale, no unit mismatch.
        let direction = direction_paper(&node.kind, a_paper, b_paper);
        let mapped_kind = match &node.kind {
            DimensionKind::Linear { .. } => DimensionKind::Linear {
                direction: Vec3::new(direction.x, direction.y, 0.0),
            },
            other => other.clone(),
        };
        // The drafting renderer derives its display number from the
        // distance between the input's `a` and `b` (interpreted as
        // metres). We've already converted them to paper-mm, so we
        // cannot let it re-derive — compute the correct world-metre
        // value once, format it with the style's number format, and
        // pass it as a pre-formatted text override.
        let style = registry.resolve(Some(&node.style_name));
        let measured_metres = measure_world_length(node);
        let text = node
            .text_override
            .clone()
            .unwrap_or_else(|| style.number_format.format_metres(measured_metres));

        let input = DimensionInput {
            kind: mapped_kind,
            a: Vec3::new(a_paper.x, a_paper.y, 0.0),
            b: Vec3::new(b_paper.x, b_paper.y, 0.0),
            offset: Vec3::new(offset_paper.x, offset_paper.y, 0.0),
            text_override: Some(text),
        };

        // `world_to_paper = 1.0` because inputs are already paper-mm.
        let prims = drafting::render_dimension(&input, &style, 1.0);
        out.push(prims);
    }
    out
}

// ─── Paper-mm → world inverse projection ──────────────────────────────────

/// Map a paper-millimetre 2D point on a captured sheet back to a 3D
/// world point that projects to it. Because the projection is
/// orthographic, any point along the camera forward axis works; we
/// return the one that sits on the plane through `view.target` with
/// normal = camera forward. That means "same depth as what the view is
/// focused on", which is the right default for sheet-local annotation
/// authoring.
///
/// Returns `None` if the view is degenerate (zero-sized frustum or
/// zero-vector forward direction).
pub fn sheet_paper_to_world(view: &SheetView, paper: Vec2) -> Option<Vec3> {
    let paper_w = view.frustum_width_mm();
    let paper_h = view.frustum_height_mm();
    if paper_w <= 0.0 || paper_h <= 0.0 {
        return None;
    }
    // Paper → NDC (same +y-up convention the capture uses).
    let ndc = Vec2::new(paper.x / paper_w * 2.0 - 1.0, paper.y / paper_h * 2.0 - 1.0);

    let forward = (view.target - view.eye).try_normalize()?;
    let right = forward.cross(view.up).try_normalize()?;
    let up = right.cross(forward).try_normalize()?;
    let half_h = view.ortho_height_m * 0.5;
    let half_w = half_h * view.aspect;

    // Point on the focal plane (through `view.target`, perpendicular to
    // `forward`) that projects to `ndc`.
    Some(view.target + right * (ndc.x * half_w) + up * (ndc.y * half_h))
}

/// Compute the measured length (in world metres) that a dimension
/// annotation represents, using its authored kind and 3D endpoints.
/// This is the number the user expects to see — independent of how we
/// project the dim onto the sheet.
fn measure_world_length(node: &DimensionAnnotationNode) -> f32 {
    match &node.kind {
        DimensionKind::Linear { direction } => {
            let dir = direction.try_normalize().unwrap_or(Vec3::X);
            (node.b - node.a).dot(dir).abs()
        }
        _ => (node.a - node.b).length(),
    }
}

fn direction_paper(kind: &DimensionKind, a: Vec2, b: Vec2) -> Vec2 {
    match kind {
        DimensionKind::Linear { direction } => {
            // Best-effort: use the projected axis the user authored. We
            // project direction_world by applying it as a delta from a.
            // In the common orthographic case (axis-aligned dim on an
            // aligned view) this degenerates to `(b - a).normalize()`,
            // which is what we want.
            let proj = Vec2::new(direction.x, direction.y);
            proj.try_normalize()
                .unwrap_or_else(|| (b - a).try_normalize().unwrap_or(Vec2::X))
        }
        _ => (b - a).try_normalize().unwrap_or(Vec2::X),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndc_to_paper_maps_corners_to_paper_extents() {
        let map = NdcToPaper {
            paper_w: 200.0,
            paper_h: 100.0,
        };
        let p0 = map.apply(Vec3::new(-1.0, -1.0, 0.0));
        let p1 = map.apply(Vec3::new(1.0, 1.0, 0.0));
        assert!((p0 - Vec2::new(0.0, 0.0)).length() < 1e-4);
        assert!((p1 - Vec2::new(200.0, 100.0)).length() < 1e-4);
    }

    #[test]
    fn build_view_proj_projects_origin_to_ndc_origin() {
        let view = SheetView {
            eye: Vec3::new(0.0, 0.0, 10.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 8.0,
            aspect: 2.0,
            scale_denominator: 50.0,
            margin_mm: 10.0,
        };
        let (vp, _, _) = build_view_proj(&view);
        let clip = vp * Vec3::ZERO.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(ndc.x.abs() < 1e-4);
        assert!(ndc.y.abs() < 1e-4);
    }

    #[test]
    fn front_view_projects_world_height_consistently() {
        let view = SheetView {
            eye: Vec3::new(0.0, 2.25, 15.0),
            target: Vec3::new(0.0, 2.25, 0.0),
            up: Vec3::Y,
            ortho_height_m: 8.0,
            aspect: 1.778,
            scale_denominator: 50.0,
            margin_mm: 10.0,
        };
        let (vp, _, _) = build_view_proj(&view);
        let map = NdcToPaper {
            paper_w: view.frustum_width_mm(),
            paper_h: view.frustum_height_mm(),
        };
        // World y=0 (ground) maps to paper y=?  The focus is at y=2.25
        // with half-height 4, so y=0 is at NDC.y = -0.5625 →
        // paper y = (1 + -0.5625)/2 * paper_h = 0.21875 * 160 = 35 mm.
        let ground = project_world_to_paper(Vec3::ZERO, &vp, &map).unwrap();
        let paper_h = view.frustum_height_mm(); // 8 * 1000 / 50 = 160 mm
        assert!(
            (ground.y - 0.21875 * paper_h).abs() < 0.01,
            "ground y = {}",
            ground.y
        );
    }
}
