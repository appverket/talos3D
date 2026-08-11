//! 3D world → 2D paper-mm capture.
//!
//! The entry point is [`build_drawing_scene`]: given the world and a
//! [`SheetView`], flatten every visible feature into one normalized
//! [`DrawingScene`]. [`capture_sheet`] is a compatibility wrapper that derives
//! a paper layout from that scene for SVG / PDF / DXF / PNG.
//!
//! Ground rules this module upholds:
//!
//! 1. **Orthographic-only.** Perspective drafting views are meaningless;
//!    we refuse the view and return `None`.
//! 2. **Normalized positions, explicit paper sizes.** Geometry positions in
//!    the scene are normalized to the orthographic view. Size attributes on
//!    dimension primitives (tick
//!    length, text height, stroke weight, extension gap) are already paper
//!    mm from the drafting renderer — we feed it `world_to_paper = 1.0`
//!    because we've projected the world positions into paper-mm 2D
//!    ourselves before calling it.
//! 3. **Unit-audited at the boundary.** The scene builder is the only
//!    place in the pipeline where two different coordinate systems meet.
//!    If something is wrong, it's wrong here — never deep inside a writer.

use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
};

use bevy::math::{Mat4, Vec2, Vec3};
use bevy::prelude::*;

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::{
    commands::find_entity_by_element_id_readonly,
    definition_preview_scene::PreviewOnly,
    dimension_line::{
        dimension_line_midpoint, dimension_line_offset_vector,
        render_dimension_line_projected_primitives, DimensionLineNode, DimensionLineVisibility,
    },
    document_properties::DocumentProperties,
    drafting::{
        self, DimPrimitive, DimensionAnnotationNode, DimensionInput, DimensionKind,
        DimensionStyleRegistry, DraftingVisibility,
    },
    identity::ElementId,
    section_fill::{extract_section_fills, SectionFillRegion},
    vector_drawing::{
        collect_active_clip_planes, collect_classified_visible_edges, collect_scene_triangles,
        drawing_overlay_excluded, EdgeType, MeshSubject,
    },
};

use super::{
    scene::{
        DrawingPrimitiveId, DrawingPrimitiveRole, DrawingScene, DrawingSceneAnnotation,
        DrawingSceneHatch, DrawingSceneLine,
    },
    sheet::{DraftingSheet, SheetStroke, SheetView},
};

// ─── Public API ───────────────────────────────────────────────────────────

/// Flatten the 3D world into a paper-mm [`DraftingSheet`] for the given
/// [`SheetView`]. Returns `None` if no camera is active, the scene has no
/// mesh data, or the view is non-orthographic (drafting with perspective
/// is nonsensical — see PP69).
pub fn capture_sheet(world: &World, view: &SheetView) -> Option<DraftingSheet> {
    let margin_mm = view.margin_mm;
    build_drawing_scene(world, view).map(|scene| DraftingSheet::from_scene(scene, margin_mm))
}

/// Build the one normalized semantic projection consumed by live and export
/// adapters. The compatibility [`capture_sheet`] entry point delegates here
/// and performs paper layout only after projection is complete.
pub fn build_drawing_scene(world: &World, view: &SheetView) -> Option<DrawingScene> {
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
    let active_draft = drafting::active_draft_snapshot(world);
    let member_filter = active_draft
        .as_ref()
        .map(|draft| draft.node.members.iter().copied().collect::<HashSet<_>>());

    // Camera frames.
    let (view_proj, camera_position, camera_forward) = build_view_proj(view);
    let paper_w = view.frustum_width_mm();
    let paper_h = view.frustum_height_mm();
    let ndc_to_drawing = NdcToDrawing { paper_w, paper_h };

    // 1) Collect visible mesh subjects and their triangles.
    let mut subject_query = world.try_query_filtered::<(
        Entity,
        &crate::plugins::identity::ElementId,
        &Mesh3d,
        &GlobalTransform,
        Option<&Visibility>,
    ), Without<PreviewOnly>>()?;
    let mut subjects = Vec::new();
    for (entity, element_id, mesh_handle, mesh_transform, visibility) in subject_query.iter(world) {
        if member_filter
            .as_ref()
            .is_some_and(|members| !members.contains(element_id))
        {
            continue;
        }
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
        subjects.push((
            *element_id,
            MeshSubject {
                entity,
                mesh_handle: mesh_handle.0.clone(),
                mesh_transform: *mesh_transform,
            },
        ));
    }
    let mesh_subjects: Vec<MeshSubject> = subjects
        .iter()
        .map(|(_, subject)| MeshSubject {
            entity: subject.entity,
            mesh_handle: subject.mesh_handle.clone(),
            mesh_transform: subject.mesh_transform,
        })
        .collect();
    let scene_triangles = collect_scene_triangles(&mesh_subjects, mesh_assets);
    // Active clip planes make "elevation beyond" visible — without
    // them, trusses and other geometry on the visible side of the
    // section are wrongly culled as occluded by the parts of the
    // enclosing meshes that have been cut away.
    let clip_planes = collect_active_clip_planes(world);

    let mut scene = DrawingScene::new(view.clone());
    if let Some(draft) = &active_draft {
        scene.draft_id = Some(draft.element_id);
        scene.draft_plane = Some(draft.node.plane.clone());
        for member in &draft.node.members {
            if find_entity_by_element_id_readonly(world, *member).is_none() {
                scene.findings.push(super::scene::DrawingSceneFinding {
                    code: "draft.member_missing",
                    message: format!(
                        "Draft {} references missing element {}",
                        draft.element_id.0, member.0
                    ),
                });
            }
        }
    }

    // 2) Visible edges → paper-mm line segments, classified.
    for (owner, subject) in &subjects {
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
        for (ordinal, (a_world, b_world, edge_type)) in classified.into_iter().enumerate() {
            if let (Some(a), Some(b)) = (
                project_world_to_normalized(a_world, &view_proj, &ndc_to_drawing),
                project_world_to_normalized(b_world, &view_proj, &ndc_to_drawing),
            ) {
                scene.lines.push(DrawingSceneLine {
                    id: DrawingPrimitiveId {
                        owner: *owner,
                        role: DrawingPrimitiveRole::VisibleEdge,
                        ordinal: ordinal as u32,
                    },
                    owner: *owner,
                    a,
                    b,
                    stroke: edge_stroke(edge_type),
                });
            }
        }
    }

    // 3) Clip-plane section fills → paper-mm polygons + section-cut outlines.
    let fill_regions = extract_section_fills(world, mesh_assets);
    for (fill_ordinal, region) in fill_regions.iter().enumerate() {
        if member_filter
            .as_ref()
            .is_some_and(|members| !members.contains(&region.owner))
        {
            continue;
        }
        if let Some(polygon) = project_polygon(region, &view_proj, &ndc_to_drawing) {
            // Outline the cut polygon with section-cut weight — heaviest
            // on the page by convention.
            for i in 0..polygon.len() {
                let j = (i + 1) % polygon.len();
                scene.lines.push(DrawingSceneLine {
                    id: DrawingPrimitiveId {
                        owner: region.owner,
                        role: DrawingPrimitiveRole::SectionCutEdge,
                        ordinal: (fill_ordinal * polygon.len() + i) as u32,
                    },
                    owner: region.owner,
                    a: polygon[i],
                    b: polygon[j],
                    stroke: SheetStroke::SectionCut,
                });
            }
            scene.hatches.push(DrawingSceneHatch {
                id: DrawingPrimitiveId {
                    owner: region.owner,
                    role: DrawingPrimitiveRole::SectionFill,
                    ordinal: fill_ordinal as u32,
                },
                owner: region.owner,
                polygon,
                pattern: region.pattern,
            });
        }
    }

    // 4) Rich drafting annotations — project to paper-mm 2D, then the
    //    renderer emits paper-mm primitives directly.
    scene.annotations =
        capture_annotations(world, &view_proj, &ndc_to_drawing, member_filter.as_ref());

    // 5) Finalise bounds (content bbox + view margin).
    canonicalize_scene(&mut scene);
    scene.recompute_bounds();
    scene.source_model_revision = drawing_scene_revision(&scene);

    Some(scene)
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

struct NdcToDrawing {
    paper_w: f32,
    paper_h: f32,
}

impl NdcToDrawing {
    fn normalized(&self, ndc: Vec3) -> Vec2 {
        Vec2::new(ndc.x * 0.5 + 0.5, ndc.y * 0.5 + 0.5)
    }

    fn paper(&self, ndc: Vec3) -> Vec2 {
        self.normalized(ndc) * Vec2::new(self.paper_w, self.paper_h)
    }

    fn paper_to_normalized(&self, paper: Vec2) -> Vec2 {
        paper / Vec2::new(self.paper_w, self.paper_h)
    }
}

fn project_world_to_normalized(point: Vec3, view_proj: &Mat4, map: &NdcToDrawing) -> Option<Vec2> {
    let clip = *view_proj * point.extend(1.0);
    if clip.w.abs() < 1e-7 {
        return None;
    }
    Some(map.normalized(clip.truncate() / clip.w))
}

fn project_world_to_paper(point: Vec3, view_proj: &Mat4, map: &NdcToDrawing) -> Option<Vec2> {
    let clip = *view_proj * point.extend(1.0);
    if clip.w.abs() < 1e-7 {
        return None;
    }
    Some(map.paper(clip.truncate() / clip.w))
}

fn project_polygon(
    region: &SectionFillRegion,
    view_proj: &Mat4,
    map: &NdcToDrawing,
) -> Option<Vec<Vec2>> {
    let projected: Vec<Vec2> = region
        .polygon_3d
        .iter()
        .filter_map(|p| project_world_to_normalized(*p, view_proj, map))
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
    map: &NdcToDrawing,
    member_filter: Option<&HashSet<ElementId>>,
) -> Vec<DrawingSceneAnnotation> {
    let mut out = Vec::new();

    let Some(registry) = world.get_resource::<DimensionStyleRegistry>() else {
        return capture_legacy_dimension_lines(world, view_proj, map, member_filter, out);
    };
    let visibility = world
        .get_resource::<DraftingVisibility>()
        .cloned()
        .unwrap_or_default();
    if !visibility.show_all {
        return capture_legacy_dimension_lines(world, view_proj, map, member_filter, out);
    }
    let Some(mut q) = world.try_query::<(&ElementId, &DimensionAnnotationNode)>() else {
        return capture_legacy_dimension_lines(world, view_proj, map, member_filter, out);
    };
    for (element_id, node) in q.iter(world) {
        if member_filter.is_some_and(|members| !members.contains(element_id)) {
            continue;
        }
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
        out.push(DrawingSceneAnnotation {
            id: DrawingPrimitiveId {
                owner: *element_id,
                role: DrawingPrimitiveRole::Annotation,
                ordinal: 0,
            },
            owner: *element_id,
            primitives: normalize_primitives(prims, map),
        });
    }
    capture_legacy_dimension_lines(world, view_proj, map, member_filter, out)
}

fn capture_legacy_dimension_lines(
    world: &World,
    view_proj: &Mat4,
    map: &NdcToDrawing,
    member_filter: Option<&HashSet<ElementId>>,
    mut out: Vec<DrawingSceneAnnotation>,
) -> Vec<DrawingSceneAnnotation> {
    let visible = world
        .get_resource::<DimensionLineVisibility>()
        .map(|visibility| visibility.show_all)
        .unwrap_or(true);
    let Some(doc_props) = world.get_resource::<DocumentProperties>() else {
        return out;
    };
    if !visible {
        return out;
    }
    let Some(mut q) = world.try_query::<(&ElementId, &DimensionLineNode)>() else {
        return out;
    };
    for (element_id, node) in q.iter(world) {
        if member_filter.is_some_and(|members| !members.contains(element_id)) {
            continue;
        }
        if !node.visible {
            continue;
        }
        let Some(start) = project_world_to_paper(node.start, view_proj, map) else {
            continue;
        };
        let Some(end) = project_world_to_paper(node.end, view_proj, map) else {
            continue;
        };
        let midpoint = dimension_line_midpoint(node);
        let Some(mid) = project_world_to_paper(midpoint, view_proj, map) else {
            continue;
        };
        let Some(mid_plus_offset) = project_world_to_paper(
            midpoint + dimension_line_offset_vector(node),
            view_proj,
            map,
        ) else {
            continue;
        };
        let primitives = render_dimension_line_projected_primitives(
            node,
            doc_props,
            start,
            end,
            mid_plus_offset - mid,
            1.0,
        );
        out.push(DrawingSceneAnnotation {
            id: DrawingPrimitiveId {
                owner: *element_id,
                role: DrawingPrimitiveRole::Annotation,
                ordinal: 0,
            },
            owner: *element_id,
            primitives: normalize_primitives(primitives, map),
        });
    }
    out
}

fn normalize_primitives(primitives: Vec<DimPrimitive>, map: &NdcToDrawing) -> Vec<DimPrimitive> {
    primitives
        .into_iter()
        .map(|primitive| match primitive {
            DimPrimitive::LineSegment { a, b, stroke_mm } => DimPrimitive::LineSegment {
                a: map.paper_to_normalized(a),
                b: map.paper_to_normalized(b),
                stroke_mm,
            },
            DimPrimitive::Tick {
                pos,
                rotation_rad,
                length_mm,
                stroke_mm,
            } => DimPrimitive::Tick {
                pos: map.paper_to_normalized(pos),
                rotation_rad,
                length_mm,
                stroke_mm,
            },
            DimPrimitive::Arrow {
                tip,
                tail,
                width_mm,
                filled,
                stroke_mm,
            } => DimPrimitive::Arrow {
                tip: map.paper_to_normalized(tip),
                tail: map.paper_to_normalized(tail),
                width_mm,
                filled,
                stroke_mm,
            },
            DimPrimitive::Dot { pos, radius_mm } => DimPrimitive::Dot {
                pos: map.paper_to_normalized(pos),
                radius_mm,
            },
            DimPrimitive::Text {
                anchor,
                content,
                height_mm,
                rotation_rad,
                anchor_mode,
                font_family,
                color_hex,
            } => DimPrimitive::Text {
                anchor: map.paper_to_normalized(anchor),
                content,
                height_mm,
                rotation_rad,
                anchor_mode,
                font_family,
                color_hex,
            },
        })
        .collect()
}

fn canonicalize_scene(scene: &mut DrawingScene) {
    for line in &mut scene.lines {
        if compare_vec2(line.b, line.a).is_lt() {
            std::mem::swap(&mut line.a, &mut line.b);
        }
    }
    scene.lines.sort_by(|left, right| {
        left.owner
            .0
            .cmp(&right.owner.0)
            .then_with(|| role_rank(left.id.role).cmp(&role_rank(right.id.role)))
            .then_with(|| stroke_rank(left.stroke).cmp(&stroke_rank(right.stroke)))
            .then_with(|| compare_vec2(left.a, right.a))
            .then_with(|| compare_vec2(left.b, right.b))
    });
    assign_line_ordinals(&mut scene.lines);

    for hatch in &mut scene.hatches {
        canonicalize_polygon(&mut hatch.polygon);
    }
    scene.hatches.sort_by(|left, right| {
        left.owner
            .0
            .cmp(&right.owner.0)
            .then_with(|| compare_polygon(&left.polygon, &right.polygon))
    });
    let mut previous_hatch_owner = None;
    let mut hatch_ordinal = 0_u32;
    for hatch in &mut scene.hatches {
        if previous_hatch_owner != Some(hatch.owner) {
            previous_hatch_owner = Some(hatch.owner);
            hatch_ordinal = 0;
        }
        hatch.id = DrawingPrimitiveId {
            owner: hatch.owner,
            role: DrawingPrimitiveRole::SectionFill,
            ordinal: hatch_ordinal,
        };
        hatch_ordinal = hatch_ordinal.saturating_add(1);
    }

    scene
        .annotations
        .sort_by_key(|annotation| annotation.owner.0);
    let mut previous_annotation_owner = None;
    let mut annotation_ordinal = 0_u32;
    for annotation in &mut scene.annotations {
        if previous_annotation_owner != Some(annotation.owner) {
            previous_annotation_owner = Some(annotation.owner);
            annotation_ordinal = 0;
        }
        annotation.id = DrawingPrimitiveId {
            owner: annotation.owner,
            role: DrawingPrimitiveRole::Annotation,
            ordinal: annotation_ordinal,
        };
        annotation_ordinal = annotation_ordinal.saturating_add(1);
    }
}

fn canonicalize_polygon(polygon: &mut Vec<Vec2>) {
    if polygon.len() < 2 {
        return;
    }

    let start = polygon
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| compare_vec2(**left, **right))
        .map(|(index, _)| index)
        .unwrap_or(0);
    let len = polygon.len();
    let forward: Vec<Vec2> = (0..len)
        .map(|offset| polygon[(start + offset) % len])
        .collect();
    let reverse: Vec<Vec2> = (0..len)
        .map(|offset| polygon[(start + len - offset) % len])
        .collect();
    *polygon = if compare_polygon(&forward, &reverse).is_le() {
        forward
    } else {
        reverse
    };
}

fn assign_line_ordinals(lines: &mut [DrawingSceneLine]) {
    let mut previous_key = None;
    let mut ordinal = 0_u32;
    for line in lines {
        let key = (line.owner, line.id.role);
        if previous_key != Some(key) {
            previous_key = Some(key);
            ordinal = 0;
        }
        line.id = DrawingPrimitiveId {
            owner: line.owner,
            role: line.id.role,
            ordinal,
        };
        ordinal = ordinal.saturating_add(1);
    }
}

fn compare_polygon(left: &[Vec2], right: &[Vec2]) -> std::cmp::Ordering {
    left.len().cmp(&right.len()).then_with(|| {
        left.iter()
            .zip(right)
            .map(|(left, right)| compare_vec2(*left, *right))
            .find(|ordering| !ordering.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn compare_vec2(left: Vec2, right: Vec2) -> std::cmp::Ordering {
    left.x
        .total_cmp(&right.x)
        .then_with(|| left.y.total_cmp(&right.y))
}

fn role_rank(role: DrawingPrimitiveRole) -> u8 {
    match role {
        DrawingPrimitiveRole::VisibleEdge => 0,
        DrawingPrimitiveRole::SectionCutEdge => 1,
        DrawingPrimitiveRole::SectionFill => 2,
        DrawingPrimitiveRole::Annotation => 3,
    }
}

fn stroke_rank(stroke: SheetStroke) -> u8 {
    match stroke {
        SheetStroke::SectionCut => 0,
        SheetStroke::Silhouette => 1,
        SheetStroke::Crease => 2,
        SheetStroke::Boundary => 3,
        SheetStroke::Dimension => 4,
        SheetStroke::Hatch => 5,
    }
}

fn drawing_scene_revision(scene: &DrawingScene) -> u64 {
    let mut hasher = DefaultHasher::new();
    scene.draft_id.hash(&mut hasher);
    if let Some(plane) = &scene.draft_plane {
        for value in [
            plane.origin.x,
            plane.origin.y,
            plane.origin.z,
            plane.normal.x,
            plane.normal.y,
            plane.normal.z,
            plane.tangent.x,
            plane.tangent.y,
            plane.tangent.z,
            plane.bitangent.x,
            plane.bitangent.y,
            plane.bitangent.z,
        ] {
            value.to_bits().hash(&mut hasher);
        }
    }
    for value in [
        scene.view.eye.x,
        scene.view.eye.y,
        scene.view.eye.z,
        scene.view.target.x,
        scene.view.target.y,
        scene.view.target.z,
        scene.view.up.x,
        scene.view.up.y,
        scene.view.up.z,
        scene.view.ortho_height_m,
        scene.view.aspect,
        scene.view.scale_denominator,
    ] {
        value.to_bits().hash(&mut hasher);
    }
    for line in &scene.lines {
        line.id.hash(&mut hasher);
        line.stroke.hash(&mut hasher);
        hash_vec2(line.a, &mut hasher);
        hash_vec2(line.b, &mut hasher);
    }
    for hatch in &scene.hatches {
        hatch.id.hash(&mut hasher);
        for point in &hatch.polygon {
            hash_vec2(*point, &mut hasher);
        }
        hash_hatch_pattern(hatch.pattern, &mut hasher);
    }
    for annotation in &scene.annotations {
        annotation.id.hash(&mut hasher);
        for primitive in &annotation.primitives {
            hash_primitive(primitive, &mut hasher);
        }
    }
    hasher.finish()
}

fn hash_vec2(point: Vec2, hasher: &mut impl Hasher) {
    point.x.to_bits().hash(hasher);
    point.y.to_bits().hash(hasher);
}

fn hash_hatch_pattern(
    pattern: crate::plugins::section_fill::HatchPattern,
    hasher: &mut impl Hasher,
) {
    use crate::plugins::section_fill::HatchPattern;
    match pattern {
        HatchPattern::DiagonalLines {
            angle_deg,
            spacing_mm,
        } => {
            0_u8.hash(hasher);
            angle_deg.to_bits().hash(hasher);
            spacing_mm.to_bits().hash(hasher);
        }
        HatchPattern::Crosshatch {
            angle_deg,
            spacing_mm,
        } => {
            1_u8.hash(hasher);
            angle_deg.to_bits().hash(hasher);
            spacing_mm.to_bits().hash(hasher);
        }
        HatchPattern::WoodGrain {
            angle_deg,
            spacing_mm,
        } => {
            2_u8.hash(hasher);
            angle_deg.to_bits().hash(hasher);
            spacing_mm.to_bits().hash(hasher);
        }
        HatchPattern::SolidFill => 3_u8.hash(hasher),
        HatchPattern::NoFill => 4_u8.hash(hasher),
    }
}

fn hash_primitive(primitive: &DimPrimitive, hasher: &mut impl Hasher) {
    match primitive {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            0_u8.hash(hasher);
            hash_vec2(*a, hasher);
            hash_vec2(*b, hasher);
            stroke_mm.to_bits().hash(hasher);
        }
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => {
            1_u8.hash(hasher);
            hash_vec2(*pos, hasher);
            rotation_rad.to_bits().hash(hasher);
            length_mm.to_bits().hash(hasher);
            stroke_mm.to_bits().hash(hasher);
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm,
        } => {
            2_u8.hash(hasher);
            hash_vec2(*tip, hasher);
            hash_vec2(*tail, hasher);
            width_mm.to_bits().hash(hasher);
            filled.hash(hasher);
            stroke_mm.to_bits().hash(hasher);
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            3_u8.hash(hasher);
            hash_vec2(*pos, hasher);
            radius_mm.to_bits().hash(hasher);
        }
        DimPrimitive::Text {
            anchor,
            content,
            height_mm,
            rotation_rad,
            anchor_mode,
            font_family,
            color_hex,
        } => {
            4_u8.hash(hasher);
            hash_vec2(*anchor, hasher);
            content.hash(hasher);
            height_mm.to_bits().hash(hasher);
            rotation_rad.to_bits().hash(hasher);
            (*anchor_mode as u8).hash(hasher);
            font_family.hash(hasher);
            color_hex.hash(hasher);
        }
    }
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
    drawing_normalized_to_world(view, paper / Vec2::new(paper_w, paper_h))
}

/// Map normalized drawing coordinates back onto the focal plane captured by
/// [`SheetView`]. This is the sole inverse used by paper picking and the live
/// DrawingScene backend until durable `DraftPlane` takes ownership in DRAFT
/// 2.1.
pub fn drawing_normalized_to_world(view: &SheetView, drawing: Vec2) -> Option<Vec3> {
    let ndc = drawing * 2.0 - Vec2::ONE;

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
    fn ndc_mapping_separates_normalized_scene_from_paper_layout() {
        let map = NdcToDrawing {
            paper_w: 200.0,
            paper_h: 100.0,
        };
        let n0 = map.normalized(Vec3::new(-1.0, -1.0, 0.0));
        let n1 = map.normalized(Vec3::new(1.0, 1.0, 0.0));
        assert!((n0 - Vec2::ZERO).length() < 1e-4);
        assert!((n1 - Vec2::ONE).length() < 1e-4);
        assert!((map.paper(Vec3::new(1.0, 1.0, 0.0)) - Vec2::new(200.0, 100.0)).length() < 1e-4);
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
    fn canonical_polygon_is_independent_of_start_vertex_and_winding() {
        let expected = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 0.0),
        ];
        let mut rotated = vec![
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 0.0),
            Vec2::new(0.0, 1.0),
        ];
        let mut reversed = vec![
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(0.0, 0.0),
        ];

        canonicalize_polygon(&mut rotated);
        canonicalize_polygon(&mut reversed);

        assert_eq!(rotated, expected);
        assert_eq!(reversed, expected);
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
        let map = NdcToDrawing {
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

    #[test]
    fn active_draft_membership_scopes_annotations_and_reports_stale_references() {
        let mut world = World::new();
        world.insert_resource(Assets::<Mesh>::default());
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(DimensionStyleRegistry::default());
        world.insert_resource(DraftingVisibility::default());
        // Register the render-query component types without contributing an
        // authored subject to the scene.
        world.spawn((
            Mesh3d::default(),
            GlobalTransform::default(),
            Visibility::Hidden,
            PreviewOnly,
        ));

        let mut draft = drafting::DraftNode::new("Scoped");
        draft.members = vec![ElementId(10), ElementId(999)];
        draft.normalize_and_validate().unwrap();
        world.spawn((ElementId(1), draft));
        for (id, y) in [(10, 0.0), (11, 1.0)] {
            world.spawn((
                ElementId(id),
                DimensionAnnotationNode {
                    kind: DimensionKind::Aligned,
                    a: Vec3::new(0.0, y, 0.0),
                    b: Vec3::new(1.0, y, 0.0),
                    offset: Vec3::new(0.0, 0.2, 0.0),
                    style_name: "architectural_metric".to_string(),
                    text_override: None,
                    visible: true,
                },
            ));
        }
        let mut workspace = drafting::DraftingWorkspaceState::default();
        workspace.select_draft(Some(ElementId(1)));
        world.insert_resource(workspace);

        let view = SheetView {
            eye: Vec3::new(0.0, 0.0, 10.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 4.0,
            aspect: 1.0,
            scale_denominator: 50.0,
            margin_mm: 10.0,
        };
        let scene = build_drawing_scene(&world, &view).expect("drawing scene");
        assert_eq!(scene.draft_id, Some(ElementId(1)));
        assert_eq!(
            scene
                .annotations
                .iter()
                .map(|annotation| annotation.owner)
                .collect::<Vec<_>>(),
            vec![ElementId(10)]
        );
        assert!(scene
            .findings
            .iter()
            .any(|finding| finding.code == "draft.member_missing"));
    }
}
