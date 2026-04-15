/// Vector drawing geometry extraction for architectural-quality SVG/PDF export.
///
/// Projects visible 3D edges and dimension annotations into 2D line segments
/// classified by edge type, enabling true vector output with proper line weights.
/// See ADR-036 for architectural context.
use bevy::{
    camera::CameraProjection,
    mesh::{Indices, VertexAttributeValues},
    prelude::*,
};
use std::{collections::HashMap, io::Write};

use crate::{
    capability_registry::CapabilityRegistry,
    plugins::{
        camera::{CameraProjectionMode, OrbitCamera},
        dimension_line::{DimensionLineNode, DimensionLineVisibility},
        document_properties::DocumentProperties,
        drafting::{self, DimPrimitive, DimensionAnnotationNode, DimensionStyleRegistry, DraftingVisibility},
        render_pipeline::RenderSettings,
        section_fill::{
            extract_section_fills, generate_hatch_lines, HatchPattern, ProjectedSectionFill,
        },
    },
};

// ─── Line weight defaults (mm at 1:1) ───────────────────────────────────────

const WEIGHT_SECTION_CUT_MM: f32 = 0.70;
const WEIGHT_SILHOUETTE_MM: f32 = 0.35;
const WEIGHT_CREASE_MM: f32 = 0.35;
const WEIGHT_BOUNDARY_MM: f32 = 0.35;
const WEIGHT_DIMENSION_MM: f32 = 0.18;

const FEATURE_EDGE_COS_THRESHOLD: f32 = 0.85;
const VISIBLE_EDGE_RAY_SAMPLE_T_VALUES: [f32; 3] = [0.2, 0.5, 0.8];
const EDGE_VISIBILITY_EPSILON: f32 = 0.01;
const ORTHOGRAPHIC_VISIBILITY_RAY_LENGTH: f32 = 10_000.0;
const EDGE_QUANTIZATION_SCALE: f32 = 10_000.0;
const DIMENSION_LINE_TICK_HALF: f32 = 0.06;

// ─── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeType {
    Silhouette,
    Crease,
    Boundary,
    SectionCut,
    Dimension,
}

impl EdgeType {
    pub fn default_weight_mm(&self) -> f32 {
        match self {
            Self::SectionCut => WEIGHT_SECTION_CUT_MM,
            Self::Silhouette => WEIGHT_SILHOUETTE_MM,
            Self::Crease => WEIGHT_CREASE_MM,
            Self::Boundary => WEIGHT_BOUNDARY_MM,
            Self::Dimension => WEIGHT_DIMENSION_MM,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectedEdge {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub edge_type: EdgeType,
}

#[derive(Debug, Clone)]
pub struct ProjectedDimensionLabel {
    /// Absolute viewport-pixel x coordinate (same coord space as
    /// [`ProjectedEdge`] and [`ProjectedSectionFill`]).
    pub x: f32,
    /// Absolute viewport-pixel y coordinate, +y down.
    pub y: f32,
    pub text: String,
    pub font_size_pt: f32,
}

#[derive(Debug, Clone)]
pub struct DrawingViewport {
    pub width: f32,
    pub height: f32,
    pub world_width: f32,
    pub world_height: f32,
    pub scale: f32,
    /// Canvas units per paper millimetre. Used by every exporter to size
    /// edge strokes, hatch strokes, and any mm-typed attribute so that
    /// geometry outlines, dimension strokes, and text all share one
    /// scale and the conventional arch-drafting proportions
    /// (section-cut > object > dimension) survive the export.
    pub paper_mm_to_canvas: f32,
}

#[derive(Debug, Clone)]
pub struct DrawingGeometry {
    pub edges: Vec<ProjectedEdge>,
    pub dimension_labels: Vec<ProjectedDimensionLabel>,
    pub section_fills: Vec<ProjectedSectionFill>,
    /// Rich drafting-dimension primitives produced by the drafting plugin.
    /// Already in viewport-pixel coordinates — writers consume directly.
    pub drafting_primitives: Vec<Vec<DimPrimitive>>,
    pub viewport: DrawingViewport,
}

// ─── Extraction from ECS world ───────────────────────────────────────────────

pub fn extract_drawing_geometry(world: &World) -> Option<DrawingGeometry> {
    let registry = world.get_resource::<CapabilityRegistry>()?;
    let mesh_assets = world.get_resource::<Assets<Mesh>>()?;
    let doc_props = world.get_resource::<DocumentProperties>()?;
    let dim_visibility = world.get_resource::<DimensionLineVisibility>()?;
    let _settings = world.get_resource::<RenderSettings>()?;

    let mut camera_query =
        world.try_query::<(&OrbitCamera, &GlobalTransform, &Projection, &Camera)>()?;
    let (orbit, camera_gt, projection, camera) = camera_query.iter(world).next()?;

    let orthographic = matches!(orbit.projection_mode, CameraProjectionMode::Isometric);
    let camera_position = camera_gt.translation();
    let camera_forward = camera_gt.forward().as_vec3();

    let view_proj = camera_gt.to_matrix().inverse()
        * match projection {
            Projection::Orthographic(ortho) => ortho.get_clip_from_view(),
            Projection::Perspective(persp) => persp.get_clip_from_view(),
            _ => return None,
        };

    let (vp_width, vp_height, world_width, world_height) = viewport_dimensions(orbit, projection);

    // Collect mesh subjects
    let mut entity_query = world.try_query::<(
        Entity,
        &crate::plugins::identity::ElementId,
        &Mesh3d,
        &GlobalTransform,
        Option<&Visibility>,
    )>()?;

    let mut subjects = Vec::new();
    for (entity, _eid, mesh_handle, mesh_transform, visibility) in entity_query.iter(world) {
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

    let mut edges = Vec::new();
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
            orthographic,
            &scene_triangles,
        );
        for (start_3d, end_3d, edge_type) in classified {
            if let Some(edge) =
                project_edge(start_3d, end_3d, edge_type, &view_proj, vp_width, vp_height)
            {
                edges.push(edge);
            }
        }
    }

    // Collect dimension annotations
    let mut dimension_labels = Vec::new();
    let mut dim_edges = Vec::new();
    if dim_visibility.show_all {
        let mut dim_query = world.try_query::<&DimensionLineNode>()?;
        for node in dim_query.iter(world) {
            if !node.visible {
                continue;
            }
            let segments = dimension_segments(node.start, node.end, node.line_point, node.extension);
            for (seg_start, seg_end) in segments {
                if let Some(edge) = project_edge(
                    seg_start,
                    seg_end,
                    EdgeType::Dimension,
                    &view_proj,
                    vp_width,
                    vp_height,
                ) {
                    dim_edges.push(edge);
                }
            }

            let geometry = dimension_geometry(node.start, node.end, node.line_point, node.extension);
            let midpoint = geometry.line_midpoint();
            // Project via the same view_proj path as edges so the label
            // sits on its dimension line. `Camera::world_to_viewport`
            // returns *window* pixels which differ from the export
            // viewport pixel space, leading to mis-placed labels.
            if let Some(label_px) =
                project_point_to_pixels(midpoint, &view_proj, vp_width, vp_height)
            {
                let text = dimension_display_text(node, doc_props);
                dimension_labels.push(ProjectedDimensionLabel {
                    x: label_px.x,
                    y: label_px.y,
                    text,
                    font_size_pt: 10.0,
                });
            }
        }
    }
    edges.extend(dim_edges);

    // Section fills: intersect clip planes with meshes, project to 2D
    let fill_regions = extract_section_fills(world, mesh_assets);
    let mut section_fills = Vec::new();
    for region in &fill_regions {
        let projected: Vec<[f32; 2]> = region
            .polygon_3d
            .iter()
            .filter_map(|p| {
                let clip = view_proj * p.extend(1.0);
                if clip.w.abs() < 1e-7 {
                    return None;
                }
                let ndc = clip.truncate() / clip.w;
                Some([
                    (ndc.x * 0.5 + 0.5) * vp_width,
                    (1.0 - (ndc.y * 0.5 + 0.5)) * vp_height,
                ])
            })
            .collect();
        if projected.len() >= 3 {
            // Also emit the section cut outline edges
            for i in 0..projected.len() {
                let j = (i + 1) % projected.len();
                edges.push(ProjectedEdge {
                    x1: projected[i][0],
                    y1: projected[i][1],
                    x2: projected[j][0],
                    y2: projected[j][1],
                    edge_type: EdgeType::SectionCut,
                });
            }
            section_fills.push(ProjectedSectionFill {
                polygon: projected,
                pattern: region.pattern,
                fill_color: region.fill_color,
            });
        }
    }

    // Drafting plugin dimensions: render each annotation into paper-space
    // primitives using the viewport's pixels-per-world scale, then flip y so
    // primitives are in the same y-down pixel frame as the other edges.
    let (drafting_primitives, paper_mm_to_canvas) =
        extract_drafting_primitives(world, &view_proj, vp_width, vp_height);

    // Fit the export canvas to the actual content. The camera-derived
    // viewport gives us a paper *resolution*, but dimension lines and
    // their offsets typically extend outside the camera frustum. If we
    // ship those raw pixel coords with `viewport.{width,height}`, every
    // downstream writer (SVG viewBox, PDF MediaBox, DXF $EXTMAX) clips
    // the annotations off the page.
    //
    // Translate everything so the bounding box of all emitted content
    // starts at `MARGIN` and grow `viewport.{width,height}` to match.
    // World scale stays proportional so hatch density is unchanged.
    let mut geometry = DrawingGeometry {
        edges,
        dimension_labels,
        section_fills,
        drafting_primitives,
        viewport: DrawingViewport {
            width: vp_width,
            height: vp_height,
            world_width,
            world_height,
            scale: 1.0,
            paper_mm_to_canvas,
        },
    };
    fit_canvas_to_content(&mut geometry);
    Some(geometry)
}

/// Margin (in viewport pixels) added around the content bbox when the
/// canvas is auto-fit. Roughly 1cm at 96 DPI.
const CONTENT_MARGIN_PX: f32 = 40.0;

/// Translate every projected primitive so the bounding box of the
/// drawing's content sits at `(CONTENT_MARGIN_PX, CONTENT_MARGIN_PX)`,
/// then resize the viewport to encompass it (plus the margin on the
/// opposite side). The world scale (`world_width` / `world_height`) is
/// scaled by the same ratio as the viewport so pixels-per-world stays
/// constant — important for hatch line density.
fn fit_canvas_to_content(geometry: &mut DrawingGeometry) {
    let Some((min, max)) = content_bbox(geometry) else {
        return;
    };

    let dx = CONTENT_MARGIN_PX - min.x;
    let dy = CONTENT_MARGIN_PX - min.y;
    let new_w = (max.x - min.x) + CONTENT_MARGIN_PX * 2.0;
    let new_h = (max.y - min.y) + CONTENT_MARGIN_PX * 2.0;

    if dx.abs() < 0.5
        && dy.abs() < 0.5
        && (new_w - geometry.viewport.width).abs() < 0.5
        && (new_h - geometry.viewport.height).abs() < 0.5
    {
        return;
    }

    for edge in &mut geometry.edges {
        edge.x1 += dx;
        edge.y1 += dy;
        edge.x2 += dx;
        edge.y2 += dy;
    }
    for fill in &mut geometry.section_fills {
        for pt in &mut fill.polygon {
            pt[0] += dx;
            pt[1] += dy;
        }
    }
    for label in &mut geometry.dimension_labels {
        label.x += dx;
        label.y += dy;
    }
    let translate = bevy::math::Vec2::new(dx, dy);
    for dim in &mut geometry.drafting_primitives {
        for prim in dim.iter_mut() {
            translate_primitive_in_place(prim, translate);
        }
    }

    // Preserve pixels-per-world so hatch density is consistent.
    let old_w = geometry.viewport.width.max(1.0);
    let old_h = geometry.viewport.height.max(1.0);
    let new_world_w = geometry.viewport.world_width * (new_w / old_w);
    let new_world_h = geometry.viewport.world_height * (new_h / old_h);
    geometry.viewport.width = new_w;
    geometry.viewport.height = new_h;
    geometry.viewport.world_width = new_world_w;
    geometry.viewport.world_height = new_world_h;
}

fn content_bbox(
    geometry: &DrawingGeometry,
) -> Option<(bevy::math::Vec2, bevy::math::Vec2)> {
    let mut min = bevy::math::Vec2::splat(f32::INFINITY);
    let mut max = bevy::math::Vec2::splat(f32::NEG_INFINITY);
    let mut any = false;
    let mut update = |x: f32, y: f32| {
        if x.is_finite() && y.is_finite() {
            min.x = min.x.min(x);
            min.y = min.y.min(y);
            max.x = max.x.max(x);
            max.y = max.y.max(y);
        }
    };
    for edge in &geometry.edges {
        update(edge.x1, edge.y1);
        update(edge.x2, edge.y2);
        any = true;
    }
    for fill in &geometry.section_fills {
        for pt in &fill.polygon {
            update(pt[0], pt[1]);
            any = true;
        }
    }
    for label in &geometry.dimension_labels {
        update(label.x, label.y);
        any = true;
    }
    for dim in &geometry.drafting_primitives {
        for prim in dim {
            for p in primitive_extents(prim) {
                update(p.x, p.y);
                any = true;
            }
        }
    }
    if any {
        Some((min, max))
    } else {
        None
    }
}

fn primitive_extents(prim: &DimPrimitive) -> Vec<bevy::math::Vec2> {
    match prim {
        DimPrimitive::LineSegment { a, b, .. } => vec![*a, *b],
        DimPrimitive::Tick { pos, .. } | DimPrimitive::Dot { pos, .. } => vec![*pos],
        DimPrimitive::Arrow { tip, tail, .. } => vec![*tip, *tail],
        DimPrimitive::Text { anchor, .. } => vec![*anchor],
    }
}

fn translate_primitive_in_place(prim: &mut DimPrimitive, t: bevy::math::Vec2) {
    match prim {
        DimPrimitive::LineSegment { a, b, .. } => {
            *a += t;
            *b += t;
        }
        DimPrimitive::Tick { pos, .. } | DimPrimitive::Dot { pos, .. } => {
            *pos += t;
        }
        DimPrimitive::Arrow { tip, tail, .. } => {
            *tip += t;
            *tail += t;
        }
        DimPrimitive::Text { anchor, .. } => {
            *anchor += t;
        }
    }
}

/// Extract drafting plugin dimensions and project them into viewport-pixel
/// space. Returns one primitive list per visible annotation.
fn extract_drafting_primitives(
    world: &World,
    view_proj: &Mat4,
    vp_width: f32,
    vp_height: f32,
) -> (Vec<Vec<DimPrimitive>>, f32) {
    // Viewport-pixel canvas unit per paper millimetre. Every exporter
    // will use this to size edge strokes and hatch strokes, so the
    // section-cut > object > dimension weight hierarchy holds up.
    // Start with a dimensionless fallback; override below once we've
    // seen the real projection scale for the first dim.
    let mut paper_mm_to_canvas = paper_mm_to_canvas_fallback(vp_width);

    let Some(registry) = world.get_resource::<DimensionStyleRegistry>() else {
        return (Vec::new(), paper_mm_to_canvas);
    };
    let visibility = world
        .get_resource::<DraftingVisibility>()
        .cloned()
        .unwrap_or_default();
    if !visibility.show_all {
        return (Vec::new(), paper_mm_to_canvas);
    }
    let Some(mut q) = world.try_query::<&DimensionAnnotationNode>() else {
        return (Vec::new(), paper_mm_to_canvas);
    };
    let mut out = Vec::new();
    let mut learned_scale = false;
    for node in q.iter(world) {
        if !node.visible
            || !visibility.is_visible(&node.style_name, node.kind.tag())
        {
            continue;
        }
        // Project the dimension's two key endpoints to learn how far
        // one world metre stretches on the current canvas.
        let a_px = project_point_to_pixels(node.a, view_proj, vp_width, vp_height);
        let b_px = project_point_to_pixels(node.b, view_proj, vp_width, vp_height);
        let (Some(a_px), Some(b_px)) = (a_px, b_px) else {
            continue;
        };
        let world_len = (node.a - node.b).length().max(1e-6);
        let pixel_len = (a_px - b_px).length().max(1e-6);
        let canvas_px_per_world_m = pixel_len / world_len;
        // At arch scale 1:50, 1m world = 20mm paper. So 1mm paper =
        // `canvas_px_per_world_m / 20` canvas units.
        let px_per_paper_mm = (canvas_px_per_world_m / PAPER_MM_PER_WORLD_M).max(1.0);
        if !learned_scale {
            paper_mm_to_canvas = px_per_paper_mm;
            learned_scale = true;
        }

        // Render the annotation in pure paper-millimetre space: positions
        // and mm-sized attributes (stroke, tick length, text height,
        // extension gap, overrun) all share the same unit. Uniform scale
        // then faithfully converts them into canvas pixels.
        let primitives = drafting::render_annotation(node, registry, PAPER_MM_PER_WORLD_M);
        let primitives: Vec<_> = primitives
            .into_iter()
            .map(|p| scale_primitive_uniform(p, px_per_paper_mm))
            .collect();

        // After uniform scale, primitive positions are in canvas units
        // relative to the world origin (node.a.xy in world metres →
        // node.a.xy * canvas_px_per_world_m in canvas units). Translate
        // so `node.a` lands on `a_px` — with a +y-up → +y-down flip:
        //   viewport = (paper.x + Tx, -paper.y + Ty)
        //   Tx = a_px.x - node.a.x * canvas_px_per_world_m
        //   Ty = a_px.y + node.a.y * canvas_px_per_world_m
        let translate = bevy::math::Vec2::new(
            a_px.x - node.a.x * canvas_px_per_world_m,
            a_px.y + node.a.y * canvas_px_per_world_m,
        );
        let translated = primitives
            .into_iter()
            .map(|prim| transform_prim(prim, translate))
            .collect();
        out.push(translated);
    }
    (out, paper_mm_to_canvas)
}

/// Best-effort default when the scene has no rich dimensions yet. Assumes
/// the canvas width is meant to be a ~200-unit-wide 1:50 print of the
/// model; edges drawn at typical arch mm-weights will then fall in
/// roughly sensible absolute sizes without bespoke tuning.
fn paper_mm_to_canvas_fallback(vp_width: f32) -> f32 {
    (vp_width / 200.0).max(1.0)
}

/// Architectural paper scale denominator: 1:50. One world metre becomes
/// `1000 / 50 = 20` paper millimetres on the drawing. This is what the
/// rich drafting renderer is handed as `world_to_paper`, so that every
/// primitive — positions *and* millimetre sizes (ticks, text, stroke,
/// extension gaps, dimension-line overruns) — lives in the same paper-mm
/// coordinate system from the renderer's point of view.
const PAPER_MM_PER_WORLD_M: f32 = 20.0;

/// Multiply every spatial attribute of a primitive — position *and* size —
/// by a single scalar. Used to rescale the renderer's paper-mm output into
/// the canvas's viewport-pixel units so dimension line thickness, text
/// height, tick length, and extension-line gaps all share the same
/// per-millimetre factor (and therefore preserve their mutual
/// proportions).
fn scale_primitive_uniform(prim: DimPrimitive, factor: f32) -> DimPrimitive {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => DimPrimitive::LineSegment {
            a: a * factor,
            b: b * factor,
            stroke_mm: stroke_mm * factor,
        },
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => DimPrimitive::Tick {
            pos: pos * factor,
            rotation_rad,
            length_mm: length_mm * factor,
            stroke_mm: stroke_mm * factor,
        },
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm,
        } => DimPrimitive::Arrow {
            tip: tip * factor,
            tail: tail * factor,
            width_mm: width_mm * factor,
            filled,
            stroke_mm: stroke_mm * factor,
        },
        DimPrimitive::Dot { pos, radius_mm } => DimPrimitive::Dot {
            pos: pos * factor,
            radius_mm: radius_mm * factor,
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
            anchor: anchor * factor,
            content,
            height_mm: height_mm * factor,
            rotation_rad,
            anchor_mode,
            font_family,
            color_hex,
        },
    }
}

fn project_point_to_pixels(
    point: Vec3,
    view_proj: &Mat4,
    vp_width: f32,
    vp_height: f32,
) -> Option<bevy::math::Vec2> {
    let clip = *view_proj * point.extend(1.0);
    if clip.w.abs() < 1e-7 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(bevy::math::Vec2::new(
        (ndc.x * 0.5 + 0.5) * vp_width,
        (1.0 - (ndc.y * 0.5 + 0.5)) * vp_height,
    ))
}

/// Map a paper-space primitive (+y up) into viewport pixel space (+y down)
/// via `viewport = (paper.x + Tx, -paper.y + Ty)`. The caller computes
/// `translate = (Tx, Ty)` so that the primitive's anchor lands on the
/// projected anchor pixel — see `extract_drafting_primitives`.
fn transform_prim(prim: DimPrimitive, translate: bevy::math::Vec2) -> DimPrimitive {
    let map_point = |p: bevy::math::Vec2| {
        bevy::math::Vec2::new(p.x + translate.x, -p.y + translate.y)
    };
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => DimPrimitive::LineSegment {
            a: map_point(a),
            b: map_point(b),
            stroke_mm,
        },
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => DimPrimitive::Tick {
            pos: map_point(pos),
            // Flipping y also flips the sign of rotation angles (about the z axis).
            rotation_rad: -rotation_rad,
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
            tip: map_point(tip),
            tail: map_point(tail),
            width_mm,
            filled,
            stroke_mm,
        },
        DimPrimitive::Dot { pos, radius_mm } => DimPrimitive::Dot {
            pos: map_point(pos),
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
            anchor: map_point(anchor),
            content,
            height_mm,
            rotation_rad: -rotation_rad,
            anchor_mode,
            font_family,
            color_hex,
        },
    }
}

// ─── SVG generation ──────────────────────────────────────────────────────────

pub fn drawing_to_svg(drawing: &DrawingGeometry) -> Vec<u8> {
    let w = drawing.viewport.width;
    let h = drawing.viewport.height;
    let mut out = Vec::new();

    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#).unwrap();
    writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    )
    .unwrap();
    writeln!(out, r#"  <rect width="100%" height="100%" fill="white"/>"#).unwrap();

    let mm_to_canvas = drawing.viewport.paper_mm_to_canvas.max(0.5);
    // Section fills (rendered before edges so edges draw on top)
    let px_per_world = (w / drawing.viewport.world_width).max(0.5);
    for (i, fill) in drawing.section_fills.iter().enumerate() {
        // Build clip path from the polygon
        let clip_id = format!("section-clip-{i}");
        writeln!(out, r#"  <defs>"#).unwrap();
        writeln!(out, r#"    <clipPath id="{clip_id}">"#).unwrap();
        write!(out, r#"      <polygon points=""#).unwrap();
        for (j, pt) in fill.polygon.iter().enumerate() {
            if j > 0 {
                write!(out, " ").unwrap();
            }
            write!(out, "{:.2},{:.2}", pt[0], pt[1]).unwrap();
        }
        writeln!(out, r#""/>"#).unwrap();
        writeln!(out, r#"    </clipPath>"#).unwrap();
        writeln!(out, r#"  </defs>"#).unwrap();

        // Architectural drafting convention: cut surfaces use SOLID BLACK
        // poché or pure-black hatching against the white paper. No tinted
        // greys or rgba backgrounds — those read as 3D shading and
        // contradict the line-drawing aesthetic. The per-material
        // `fill_color` is ignored for paper output; only the *pattern*
        // matters (solid vs. hatch type).

        // Solid fill background — solid black poché.
        if matches!(fill.pattern, HatchPattern::SolidFill) {
            write!(out, r#"  <polygon points=""#).unwrap();
            for (j, pt) in fill.polygon.iter().enumerate() {
                if j > 0 {
                    write!(out, " ").unwrap();
                }
                write!(out, "{:.2},{:.2}", pt[0], pt[1]).unwrap();
            }
            writeln!(out, r#"" fill="black" stroke="none"/>"#).unwrap();
        } else if !matches!(fill.pattern, HatchPattern::NoFill) {
            // Hatch lines clipped to polygon — solid black, no background
            // fill. The unfilled polygon stays paper-white between hatch
            // lines, which is the standard arch-drafting look.
            let hatch_lines = generate_hatch_lines(&fill.polygon, fill.pattern, px_per_world);
            if !hatch_lines.is_empty() {
                // Hatch lines are the thinnest stroke on the drawing — the
                // lightest classic arch weight, same as a dimension line.
                let hatch_weight = 0.18 * mm_to_canvas;
                writeln!(
                    out,
                    r#"  <g class="section-hatch-{i}" clip-path="url(#{clip_id})" stroke="black" stroke-width="{hatch_weight:.2}" fill="none">"#
                )
                .unwrap();
                write!(out, r#"    <path d=""#).unwrap();
                for line in &hatch_lines {
                    write!(
                        out,
                        "M{:.2},{:.2}L{:.2},{:.2}",
                        line[0], line[1], line[2], line[3]
                    )
                    .unwrap();
                }
                writeln!(out, r#""/>"#).unwrap();
                writeln!(out, "  </g>").unwrap();
            }
        }
    }

    // Group edges by type for consistent line weights
    let edge_groups: &[(EdgeType, &str)] = &[
        (EdgeType::SectionCut, "section-cut"),
        (EdgeType::Silhouette, "silhouette"),
        (EdgeType::Crease, "crease"),
        (EdgeType::Boundary, "boundary"),
        (EdgeType::Dimension, "dimension"),
    ];

    for (edge_type, class_name) in edge_groups {
        // All edge strokes, section-cut hatches, and rich dimensions share
        // the same `paper_mm_to_canvas` conversion, so the weight
        // hierarchy (section-cut > object > dimension) survives the
        // export with its classical arch proportions intact.
        let weight_px = edge_type.default_weight_mm() * mm_to_canvas;
        let type_edges: Vec<_> = drawing
            .edges
            .iter()
            .filter(|e| e.edge_type == *edge_type)
            .collect();
        if type_edges.is_empty() {
            continue;
        }
        writeln!(
            out,
            r#"  <g class="{class_name}" stroke="black" stroke-width="{weight_px:.2}" stroke-linecap="round" fill="none">"#
        )
        .unwrap();
        // Batch into a single path for efficiency
        write!(out, r#"    <path d=""#).unwrap();
        for edge in &type_edges {
            write!(
                out,
                "M{:.2},{:.2}L{:.2},{:.2}",
                edge.x1, edge.y1, edge.x2, edge.y2
            )
            .unwrap();
        }
        writeln!(out, r#""/>"#).unwrap();
        writeln!(out, "  </g>").unwrap();
    }

    // Dimension labels — coordinates are absolute viewport pixels.
    if !drawing.dimension_labels.is_empty() {
        let _ = w; // silence unused-binding warning if introduced later
        writeln!(
            out,
            r#"  <g class="dimension-labels" font-family="Helvetica, Arial, sans-serif" text-anchor="middle">"#
        )
        .unwrap();
        for label in &drawing.dimension_labels {
            let fs = label.font_size_pt;
            writeln!(
                out,
                r#"    <text x="{:.1}" y="{:.1}" font-size="{fs}" fill="black">{}</text>"#,
                label.x,
                label.y,
                svg_escape(&label.text)
            )
            .unwrap();
        }
        writeln!(out, "  </g>").unwrap();
    }

    // Drafting plugin dimensions (rich primitives in viewport pixel coords).
    // Each annotation's primitives are wrapped in a <g class="dim-drafting">.
    for dim in &drawing.drafting_primitives {
        writeln!(out, r#"  <g class="dim-drafting">"#).unwrap();
        for prim in dim {
            write_drafting_primitive(&mut out, prim);
        }
        writeln!(out, "  </g>").unwrap();
    }

    writeln!(out, "</svg>").unwrap();
    out
}

fn write_drafting_primitive(out: &mut Vec<u8>, prim: &DimPrimitive) {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            writeln!(
                out,
                r##"    <line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="#000" stroke-width="{:.2}" stroke-linecap="round" fill="none"/>"##,
                a.x, a.y, b.x, b.y, stroke_mm
            )
            .unwrap();
        }
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => {
            let half = length_mm * 0.5;
            let c = rotation_rad.cos();
            let s = rotation_rad.sin();
            let x1 = pos.x - half * c;
            let y1 = pos.y - half * s;
            let x2 = pos.x + half * c;
            let y2 = pos.y + half * s;
            writeln!(
                out,
                r##"    <line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="#000" stroke-width="{:.2}" stroke-linecap="round" fill="none"/>"##,
                x1, y1, x2, y2, stroke_mm
            )
            .unwrap();
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm,
        } => {
            let axis = *tail - *tip;
            let len = axis.length().max(1e-5);
            let axis_u = axis / len;
            let perp = bevy::math::Vec2::new(-axis_u.y, axis_u.x);
            let half_w = width_mm * 0.5;
            let base_l = *tail + perp * half_w;
            let base_r = *tail - perp * half_w;
            let fill = if *filled { "#000" } else { "none" };
            writeln!(
                out,
                r##"    <polygon points="{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}" fill="{}" stroke="#000" stroke-width="{:.2}" stroke-linejoin="miter"/>"##,
                tip.x, tip.y, base_l.x, base_l.y, base_r.x, base_r.y, fill, stroke_mm
            )
            .unwrap();
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            writeln!(
                out,
                r##"    <circle cx="{:.2}" cy="{:.2}" r="{:.2}" fill="#000"/>"##,
                pos.x, pos.y, radius_mm
            )
            .unwrap();
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
            let (ta, db) = match anchor_mode {
                crate::plugins::drafting::TextAnchor::CenterBaseline => ("middle", "alphabetic"),
                crate::plugins::drafting::TextAnchor::Center => ("middle", "middle"),
            };
            let rot_deg = rotation_rad.to_degrees();
            writeln!(
                out,
                r##"    <text x="{:.2}" y="{:.2}" transform="rotate({:.2} {:.2} {:.2})" text-anchor="{}" dominant-baseline="{}" font-family="{}" font-size="{:.2}" fill="#{}">{}</text>"##,
                anchor.x,
                anchor.y,
                rot_deg,
                anchor.x,
                anchor.y,
                ta,
                db,
                svg_escape(font_family),
                height_mm,
                color_hex,
                svg_escape(content)
            )
            .unwrap();
        }
    }
}

fn svg_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ─── DXF generation ──────────────────────────────────────────────────────────

/// Export a drawing (walls + dimensions) to DXF AC1027 text format. The DXF
/// uses viewport pixel coordinates with y flipped so +y is up (DXF native).
pub fn drawing_to_dxf(drawing: &DrawingGeometry) -> String {
    use crate::plugins::drafting::export_dxf::{export_dxf, DxfUnit};
    let h = drawing.viewport.height;

    // Convert regular drawing edges to DimPrimitives so the DXF writer can
    // handle them uniformly. Apply y-flip here (viewport is +y-down, DXF +y-up).
    let mut drawing_primitives = Vec::new();
    for edge in &drawing.edges {
        drawing_primitives.push(DimPrimitive::LineSegment {
            a: bevy::math::Vec2::new(edge.x1, h - edge.y1),
            b: bevy::math::Vec2::new(edge.x2, h - edge.y2),
            stroke_mm: edge.edge_type.default_weight_mm(),
        });
    }

    // Flip dimensions the same way so they line up with the drawing.
    let flipped_dims: Vec<Vec<DimPrimitive>> = drawing
        .drafting_primitives
        .iter()
        .map(|dim| dim.iter().cloned().map(|p| flip_y(p, h)).collect())
        .collect();

    export_dxf(
        DxfUnit::Millimetres,
        (0.0, 0.0),
        (drawing.viewport.width, drawing.viewport.height),
        &drawing_primitives,
        &flipped_dims,
    )
}

fn flip_y(prim: DimPrimitive, h: f32) -> DimPrimitive {
    let flip = |p: bevy::math::Vec2| bevy::math::Vec2::new(p.x, h - p.y);
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => DimPrimitive::LineSegment {
            a: flip(a),
            b: flip(b),
            stroke_mm,
        },
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => DimPrimitive::Tick {
            pos: flip(pos),
            rotation_rad: -rotation_rad,
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
            tip: flip(tip),
            tail: flip(tail),
            width_mm,
            filled,
            stroke_mm,
        },
        DimPrimitive::Dot { pos, radius_mm } => DimPrimitive::Dot {
            pos: flip(pos),
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
            anchor: flip(anchor),
            content,
            height_mm,
            rotation_rad: -rotation_rad,
            anchor_mode,
            font_family,
            color_hex,
        },
    }
}

// ─── PDF generation ──────────────────────────────────────────────────────────

pub fn drawing_to_pdf(drawing: &DrawingGeometry) -> Vec<u8> {
    let w = drawing.viewport.width;
    let h = drawing.viewport.height;

    // Build content stream with vector drawing operators
    let mut content = Vec::new();

    // White background
    writeln!(content, "1 1 1 rg").unwrap();
    writeln!(content, "0 0 {w} {h} re f").unwrap();

    // PDF user-unit page is sized in the same canvas units as SVG, so
    // `paper_mm_to_canvas` IS the mm-to-PDF-points factor here.
    let mm_to_pt = drawing.viewport.paper_mm_to_canvas.max(0.5);
    // Section fills (rendered before edges so edges draw on top)
    let pdf_px_per_world = (w / drawing.viewport.world_width).max(0.5);
    for fill in &drawing.section_fills {
        if matches!(fill.pattern, HatchPattern::NoFill) {
            continue;
        }

        // Architectural drafting convention: solid black poché OR pure
        // black hatching. The per-material `fill_color` is ignored on
        // paper output (would read as 3D shading).
        if fill.polygon.len() >= 3 {
            // Save graphics state
            writeln!(content, "q").unwrap();

            // Construct clip path
            let first = &fill.polygon[0];
            writeln!(content, "{:.2} {:.2} m", first[0], h - first[1]).unwrap();
            for pt in &fill.polygon[1..] {
                writeln!(content, "{:.2} {:.2} l", pt[0], h - pt[1]).unwrap();
            }
            writeln!(content, "h W n").unwrap(); // close, clip, no-op paint

            if matches!(fill.pattern, HatchPattern::SolidFill) {
                // Solid black poché
                writeln!(content, "0 0 0 rg").unwrap();
                let first = &fill.polygon[0];
                writeln!(content, "{:.2} {:.2} m", first[0], h - first[1]).unwrap();
                for pt in &fill.polygon[1..] {
                    writeln!(content, "{:.2} {:.2} l", pt[0], h - pt[1]).unwrap();
                }
                writeln!(content, "h f").unwrap();
            } else {
                // Black hatch lines on paper-white background
                let hatch_lines =
                    generate_hatch_lines(&fill.polygon, fill.pattern, pdf_px_per_world);
                if !hatch_lines.is_empty() {
                    writeln!(content, "0 0 0 RG").unwrap();
                    let hatch_w = 0.18 * mm_to_pt;
                    writeln!(content, "{hatch_w:.3} w").unwrap();
                    for line in &hatch_lines {
                        writeln!(
                            content,
                            "{:.2} {:.2} m {:.2} {:.2} l S",
                            line[0],
                            h - line[1],
                            line[2],
                            h - line[3]
                        )
                        .unwrap();
                    }
                }
            }

            // Restore graphics state
            writeln!(content, "Q").unwrap();
        }
    }

    // Draw edges grouped by line weight
    writeln!(content, "0 0 0 RG").unwrap(); // black stroke
    writeln!(content, "1 J").unwrap(); // round line cap

    let edge_groups: &[EdgeType] = &[
        EdgeType::SectionCut,
        EdgeType::Silhouette,
        EdgeType::Crease,
        EdgeType::Boundary,
        EdgeType::Dimension,
    ];

    for edge_type in edge_groups {
        let type_edges: Vec<_> = drawing
            .edges
            .iter()
            .filter(|e| e.edge_type == *edge_type)
            .collect();
        if type_edges.is_empty() {
            continue;
        }
        let weight_pt = edge_type.default_weight_mm() * mm_to_pt;
        writeln!(content, "{weight_pt:.3} w").unwrap();
        for edge in &type_edges {
            // PDF coordinates: origin at bottom-left, Y up
            let y1 = h - edge.y1;
            let y2 = h - edge.y2;
            writeln!(
                content,
                "{:.2} {:.2} m {:.2} {:.2} l S",
                edge.x1, y1, edge.x2, y2
            )
            .unwrap();
        }
    }

    // Dimension labels as text. Coordinates are absolute viewport pixels.
    if !drawing.dimension_labels.is_empty() {
        writeln!(content, "BT").unwrap();
        writeln!(content, "/F1 10 Tf").unwrap();
        writeln!(content, "0 0 0 rg").unwrap();
        // PDF Td is *relative*; emit an absolute matrix per label so each
        // is positioned independently regardless of order.
        for label in &drawing.dimension_labels {
            let x = label.x;
            let y = h - label.y; // flip Y for PDF
            let escaped = pdf_escape_text(&label.text);
            writeln!(content, "1 0 0 1 {x:.1} {y:.1} Tm ({escaped}) Tj").unwrap();
        }
        writeln!(content, "ET").unwrap();
    }

    // Drafting plugin dimensions: stroke lines/ticks, fill arrows, draw text.
    // All primitives arrive in viewport pixel coords (+y down), which we flip
    // to PDF's +y up by mapping y → (h - y).
    writeln!(content, "0 0 0 RG").unwrap();
    writeln!(content, "0 0 0 rg").unwrap();
    for dim in &drawing.drafting_primitives {
        for prim in dim {
            write_pdf_drafting_primitive(&mut content, prim, h);
        }
    }

    let content_bytes = content;

    // Build PDF structure
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xC7\xEC\x8F\xA2\n");

    // Obj 1: Catalog
    write_pdf_obj(
        &mut out,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    // Obj 2: Pages
    write_pdf_obj(
        &mut out,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    // Obj 3: Page
    let page_dict = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
    );
    write_pdf_obj(&mut out, &mut offsets, 3, page_dict.as_bytes());
    // Obj 4: Font
    write_pdf_obj(
        &mut out,
        &mut offsets,
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    // Obj 5: Content stream
    let stream_obj = format!(
        "<< /Length {} >>\nstream\n",
        content_bytes.len()
    );
    offsets.push(out.len());
    write!(out, "5 0 obj\n{stream_obj}").unwrap();
    out.extend_from_slice(&content_bytes);
    out.extend_from_slice(b"endstream\nendobj\n");

    // Cross-reference table
    let xref_offset = out.len();
    write!(out, "xref\n0 {}\n", offsets.len() + 1).unwrap();
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        writeln!(out, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        offsets.len() + 1
    )
    .unwrap();

    out
}

fn write_pdf_obj(out: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, content: &[u8]) {
    offsets.push(out.len());
    write!(out, "{id} 0 obj\n").unwrap();
    out.extend_from_slice(content);
    out.extend_from_slice(b"\nendobj\n");
}

/// Append a single drafting DimPrimitive to a PDF content stream. `h` is the
/// page height used for the +y-down → +y-up flip.
fn write_pdf_drafting_primitive(
    content: &mut Vec<u8>,
    prim: &DimPrimitive,
    h: f32,
) {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            // Primitive arrives already in canvas units (which equal PDF
            // points in this writer) after `scale_primitive_uniform` in
            // `extract_drafting_primitives`. No further unit conversion.
            let w_pt = *stroke_mm;
            writeln!(content, "{w_pt:.3} w").unwrap();
            writeln!(
                content,
                "{:.2} {:.2} m {:.2} {:.2} l S",
                a.x,
                h - a.y,
                b.x,
                h - b.y
            )
            .unwrap();
        }
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => {
            let half = length_mm * 0.5;
            let c = rotation_rad.cos();
            let s = rotation_rad.sin();
            // Primitive arrives already in canvas units (which equal PDF
            // points in this writer) after `scale_primitive_uniform` in
            // `extract_drafting_primitives`. No further unit conversion.
            let w_pt = *stroke_mm;
            writeln!(content, "{w_pt:.3} w").unwrap();
            let x1 = pos.x - half * c;
            let y1 = pos.y - half * s;
            let x2 = pos.x + half * c;
            let y2 = pos.y + half * s;
            writeln!(
                content,
                "{:.2} {:.2} m {:.2} {:.2} l S",
                x1,
                h - y1,
                x2,
                h - y2
            )
            .unwrap();
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm: _,
        } => {
            let axis = *tail - *tip;
            let len = axis.length().max(1e-5);
            let axis_u = axis / len;
            let perp = bevy::math::Vec2::new(-axis_u.y, axis_u.x);
            let half_w = width_mm * 0.5;
            let base_l = *tail + perp * half_w;
            let base_r = *tail - perp * half_w;
            writeln!(
                content,
                "{:.2} {:.2} m {:.2} {:.2} l {:.2} {:.2} l h {}",
                tip.x,
                h - tip.y,
                base_l.x,
                h - base_l.y,
                base_r.x,
                h - base_r.y,
                if *filled { "f" } else { "S" }
            )
            .unwrap();
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            // PDF has no first-class circle; approximate with a filled
            // square (close enough at the sizes used by dot terminators).
            let r = *radius_mm;
            writeln!(
                content,
                "{:.2} {:.2} {:.2} {:.2} re f",
                pos.x - r,
                (h - pos.y) - r,
                r * 2.0,
                r * 2.0
            )
            .unwrap();
        }
        DimPrimitive::Text {
            anchor,
            content: text,
            height_mm,
            rotation_rad,
            anchor_mode: _,
            font_family: _,
            color_hex: _,
        } => {
            // Already in canvas units (= PDF points on this page).
            let size_pt = *height_mm;
            let cos_r = rotation_rad.cos();
            let sin_r = rotation_rad.sin();
            let x = anchor.x;
            let y = h - anchor.y;
            writeln!(content, "BT").unwrap();
            writeln!(content, "/F1 {size_pt:.2} Tf").unwrap();
            writeln!(
                content,
                "{cos_r:.4} {sin_r:.4} {:.4} {cos_r:.4} {x:.2} {y:.2} Tm",
                -sin_r
            )
            .unwrap();
            let escaped = pdf_escape_text(text);
            writeln!(content, "({escaped}) Tj").unwrap();
            writeln!(content, "ET").unwrap();
        }
    }
}

fn pdf_escape_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

// ─── Internal geometry helpers ───────────────────────────────────────────────

pub(crate) struct MeshSubject {
    pub(crate) entity: Entity,
    pub(crate) mesh_handle: Handle<Mesh>,
    pub(crate) mesh_transform: GlobalTransform,
}

#[derive(Clone, Copy)]
pub(crate) struct SceneTriangle {
    pub(crate) entity: Entity,
    pub(crate) a: Vec3,
    pub(crate) b: Vec3,
    pub(crate) c: Vec3,
}

pub(crate) fn drawing_overlay_excluded(type_name: &str) -> bool {
    matches!(
        type_name,
        "dimension_line" | "guide_line" | "scene_light" | "clipping_plane" | "group"
    )
}

fn viewport_dimensions(orbit: &OrbitCamera, projection: &Projection) -> (f32, f32, f32, f32) {
    match projection {
        Projection::Orthographic(ortho) => {
            let ortho_w = ortho.area.width() * orbit.orthographic_scale;
            let ortho_h = ortho.area.height() * orbit.orthographic_scale;
            // Use a reasonable viewport pixel size (matching typical export resolution)
            let pixels_per_unit = 96.0; // 96 DPI baseline
            let vp_w = ortho_w.abs() * pixels_per_unit;
            let vp_h = ortho_h.abs() * pixels_per_unit;
            (vp_w, vp_h, ortho_w.abs(), ortho_h.abs())
        }
        _ => {
            // For perspective/custom, use a fixed viewport size (vector export is orthographic-primary)
            (1920.0, 1080.0, 20.0, 11.25)
        }
    }
}

pub(crate) fn collect_scene_triangles(
    subjects: &[MeshSubject],
    mesh_assets: &Assets<Mesh>,
) -> Vec<SceneTriangle> {
    let mut triangles = Vec::new();
    for subject in subjects {
        let Some(mesh) = mesh_assets.get(&subject.mesh_handle) else {
            continue;
        };
        let Some(positions) = mesh_positions(mesh) else {
            continue;
        };
        let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
            continue;
        };
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
            triangles.push(SceneTriangle {
                entity: subject.entity,
                a: subject.mesh_transform.transform_point(Vec3::from(a)),
                b: subject.mesh_transform.transform_point(Vec3::from(b)),
                c: subject.mesh_transform.transform_point(Vec3::from(c)),
            });
        }
    }
    triangles
}

pub(crate) fn collect_classified_visible_edges(
    mesh: &Mesh,
    mesh_transform: &GlobalTransform,
    entity: Entity,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    scene_triangles: &[SceneTriangle],
) -> Vec<(Vec3, Vec3, EdgeType)> {
    let Some(positions) = mesh_positions(mesh) else {
        return Vec::new();
    };
    let Some(indices) = mesh_triangle_indices(mesh, positions.len()) else {
        return Vec::new();
    };

    let mut edges = HashMap::<EdgeKey, FeatureEdgeState>::new();
    for triangle in indices.chunks(3) {
        if triangle.len() < 3 {
            continue;
        }
        let (Some(local_a), Some(local_b), Some(local_c)) = (
            positions.get(triangle[0] as usize).copied(),
            positions.get(triangle[1] as usize).copied(),
            positions.get(triangle[2] as usize).copied(),
        ) else {
            continue;
        };

        let world_a = mesh_transform.transform_point(Vec3::from(local_a));
        let world_b = mesh_transform.transform_point(Vec3::from(local_b));
        let world_c = mesh_transform.transform_point(Vec3::from(local_c));
        let normal = (world_b - world_a)
            .cross(world_c - world_a)
            .normalize_or_zero();
        if normal.length_squared() <= f32::EPSILON {
            continue;
        }
        let face_center = (world_a + world_b + world_c) / 3.0;
        let view_to_camera = if orthographic {
            -camera_forward
        } else {
            (camera_position - face_center).normalize_or_zero()
        };
        let front_facing = normal.dot(view_to_camera) >= 0.0;

        register_feature_edge(&mut edges, local_a, local_b, world_a, world_b, normal, front_facing);
        register_feature_edge(&mut edges, local_b, local_c, world_b, world_c, normal, front_facing);
        register_feature_edge(&mut edges, local_c, local_a, world_c, world_a, normal, front_facing);
    }

    edges
        .into_values()
        .filter(|edge| edge.is_visible_candidate())
        .filter(|edge| {
            edge_is_visible(
                edge.start_world,
                edge.end_world,
                entity,
                camera_position,
                camera_forward,
                orthographic,
                scene_triangles,
            )
        })
        .map(|edge| (edge.start_world, edge.end_world, edge.classify()))
        .collect()
}

fn project_edge(
    start: Vec3,
    end: Vec3,
    edge_type: EdgeType,
    view_proj: &Mat4,
    vp_width: f32,
    vp_height: f32,
) -> Option<ProjectedEdge> {
    let p1 = *view_proj * start.extend(1.0);
    let p2 = *view_proj * end.extend(1.0);

    // Clip-space to NDC
    if p1.w.abs() < 1e-7 || p2.w.abs() < 1e-7 {
        return None;
    }
    let ndc1 = p1.truncate() / p1.w;
    let ndc2 = p2.truncate() / p2.w;

    // NDC to viewport coordinates (NDC: -1..1 → viewport: 0..width/height)
    let x1 = (ndc1.x * 0.5 + 0.5) * vp_width;
    let y1 = (1.0 - (ndc1.y * 0.5 + 0.5)) * vp_height; // flip Y
    let x2 = (ndc2.x * 0.5 + 0.5) * vp_width;
    let y2 = (1.0 - (ndc2.y * 0.5 + 0.5)) * vp_height;

    Some(ProjectedEdge {
        x1,
        y1,
        x2,
        y2,
        edge_type,
    })
}

// ─── Edge detection (mirrors render_pipeline.rs logic) ───────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct QuantizedPoint(i64, i64, i64);

impl QuantizedPoint {
    fn from_point(point: [f32; 3]) -> Self {
        Self(
            (point[0] * EDGE_QUANTIZATION_SCALE).round() as i64,
            (point[1] * EDGE_QUANTIZATION_SCALE).round() as i64,
            (point[2] * EDGE_QUANTIZATION_SCALE).round() as i64,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EdgeKey(QuantizedPoint, QuantizedPoint);

impl EdgeKey {
    fn from_points(a: [f32; 3], b: [f32; 3]) -> Self {
        let start = QuantizedPoint::from_point(a);
        let end = QuantizedPoint::from_point(b);
        if start <= end {
            Self(start, end)
        } else {
            Self(end, start)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FeatureEdgeState {
    start_world: Vec3,
    end_world: Vec3,
    normals: [Vec3; 2],
    front_facing: [bool; 2],
    total_faces: u8,
}

impl FeatureEdgeState {
    fn is_visible_candidate(&self) -> bool {
        match self.total_faces {
            0 => false,
            1 => true,
            _ => {
                let silhouette = self.front_facing[0] != self.front_facing[1];
                let crease = self.normals[0].dot(self.normals[1]) <= FEATURE_EDGE_COS_THRESHOLD;
                silhouette || crease
            }
        }
    }

    fn classify(&self) -> EdgeType {
        match self.total_faces {
            0 | 1 => EdgeType::Boundary,
            _ => {
                if self.front_facing[0] != self.front_facing[1] {
                    EdgeType::Silhouette
                } else {
                    EdgeType::Crease
                }
            }
        }
    }
}

fn register_feature_edge(
    edges: &mut HashMap<EdgeKey, FeatureEdgeState>,
    local_start: [f32; 3],
    local_end: [f32; 3],
    world_start: Vec3,
    world_end: Vec3,
    normal: Vec3,
    front_facing: bool,
) {
    let key = EdgeKey::from_points(local_start, local_end);
    let state = edges.entry(key).or_insert_with(|| FeatureEdgeState {
        start_world: world_start,
        end_world: world_end,
        normals: [Vec3::ZERO; 2],
        front_facing: [false; 2],
        total_faces: 0,
    });
    let face_index = usize::from(state.total_faces.min(1));
    state.normals[face_index] = normal;
    state.front_facing[face_index] = front_facing;
    state.total_faces = state.total_faces.saturating_add(1);
}

fn edge_is_visible(
    start: Vec3,
    end: Vec3,
    owner_entity: Entity,
    camera_position: Vec3,
    camera_forward: Vec3,
    orthographic: bool,
    scene_triangles: &[SceneTriangle],
) -> bool {
    use crate::plugins::modeling::snapshots::ray_triangle_intersection;

    VISIBLE_EDGE_RAY_SAMPLE_T_VALUES
        .into_iter()
        .map(|t| start.lerp(end, t))
        .any(|sample| {
            let (ray_origin, ray_direction, max_distance) = if orthographic {
                let Some(direction) = Dir3::new(camera_forward).ok() else {
                    return true;
                };
                (
                    sample - direction.as_vec3() * ORTHOGRAPHIC_VISIBILITY_RAY_LENGTH,
                    direction,
                    ORTHOGRAPHIC_VISIBILITY_RAY_LENGTH,
                )
            } else {
                let ray_vector = sample - camera_position;
                let max_distance = ray_vector.length();
                let Some(direction) = Dir3::new(ray_vector).ok() else {
                    return true;
                };
                (camera_position, direction, max_distance)
            };
            let ray = Ray3d::new(ray_origin, ray_direction);
            !scene_triangles.iter().any(|triangle| {
                if triangle.entity == owner_entity {
                    return ray_triangle_intersection(ray, triangle.a, triangle.b, triangle.c)
                        .is_some_and(|d| {
                            d > EDGE_VISIBILITY_EPSILON
                                && d < max_distance - EDGE_VISIBILITY_EPSILON
                        });
                }
                ray_triangle_intersection(ray, triangle.a, triangle.b, triangle.c).is_some_and(
                    |d| d > EDGE_VISIBILITY_EPSILON && d < max_distance - EDGE_VISIBILITY_EPSILON,
                )
            })
        })
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
        None if vertex_count % 3 == 0 => Some((0..vertex_count as u32).collect()),
        None => None,
    }
}

// ─── Dimension geometry (mirrors dimension_line.rs logic) ────────────────────

#[allow(dead_code)]
struct DimensionGeometry {
    axis_dir: Vec3,
    offset_dir: Vec3,
    offset_vec: Vec3,
    dimension_start: Vec3,
    dimension_end: Vec3,
    visible_start: Vec3,
    visible_end: Vec3,
}

impl DimensionGeometry {
    fn line_midpoint(&self) -> Vec3 {
        (self.dimension_start + self.dimension_end) * 0.5
    }
}

fn dimension_geometry(start: Vec3, end: Vec3, line_point: Vec3, extension: f32) -> DimensionGeometry {
    let axis_dir = (end - start).try_normalize().unwrap_or(Vec3::X);
    let midpoint = (start + end) * 0.5;
    let raw_offset_vec = line_point - midpoint;
    let projected_offset_vec = raw_offset_vec - axis_dir * raw_offset_vec.dot(axis_dir);
    let min_offset = 0.2_f32;
    let (offset_dir, offset_vec) =
        if let Some(direction) = projected_offset_vec
            .try_normalize()
            .filter(|_| projected_offset_vec.length_squared() >= min_offset * min_offset)
        {
            (direction, projected_offset_vec)
        } else {
            let direction = stable_offset_direction(axis_dir);
            (direction, direction * min_offset)
        };

    let dimension_start = start + offset_vec;
    let dimension_end = end + offset_vec;
    let visible_start = dimension_start - axis_dir * extension * 0.3;
    let visible_end = dimension_end + axis_dir * extension * 0.3;

    DimensionGeometry {
        axis_dir,
        offset_dir,
        offset_vec,
        dimension_start,
        dimension_end,
        visible_start,
        visible_end,
    }
}

fn stable_offset_direction(axis_dir: Vec3) -> Vec3 {
    if axis_dir.y.abs() < 0.9 {
        Vec3::Y
    } else {
        Vec3::X
    }
}

fn tick_direction(axis_dir: Vec3) -> Vec3 {
    let perp = if axis_dir.y.abs() < 0.9 {
        axis_dir.cross(Vec3::Y)
    } else {
        axis_dir.cross(Vec3::X)
    };
    perp.try_normalize().unwrap_or(Vec3::Y)
}

fn dimension_segments(
    start: Vec3,
    end: Vec3,
    line_point: Vec3,
    extension: f32,
) -> [(Vec3, Vec3); 5] {
    let geometry = dimension_geometry(start, end, line_point, extension);
    let tick_dir = tick_direction(geometry.axis_dir) * DIMENSION_LINE_TICK_HALF;
    [
        (geometry.visible_start, geometry.visible_end),
        (start, geometry.dimension_start),
        (end, geometry.dimension_end),
        (
            geometry.dimension_start - tick_dir,
            geometry.dimension_start + tick_dir,
        ),
        (
            geometry.dimension_end - tick_dir,
            geometry.dimension_end + tick_dir,
        ),
    ]
}

fn dimension_display_text(node: &DimensionLineNode, doc_props: &DocumentProperties) -> String {
    let unit = node
        .display_unit
        .unwrap_or(doc_props.display_unit);
    let precision = node.precision.unwrap_or(doc_props.precision);
    let length = (node.end - node.start).length();
    let value = unit.format_value(length, precision);
    match node.label.as_deref() {
        Some(label) if !label.is_empty() => format!("{label}: {value}"),
        _ => value,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_type_weights_follow_iso_conventions() {
        assert!(EdgeType::SectionCut.default_weight_mm() > EdgeType::Silhouette.default_weight_mm());
        assert!(EdgeType::Silhouette.default_weight_mm() > EdgeType::Dimension.default_weight_mm());
    }

    #[test]
    fn feature_edge_boundary_detected() {
        let edge = FeatureEdgeState {
            start_world: Vec3::ZERO,
            end_world: Vec3::X,
            normals: [Vec3::Y, Vec3::ZERO],
            front_facing: [true, false],
            total_faces: 1,
        };
        assert!(edge.is_visible_candidate());
        assert_eq!(edge.classify(), EdgeType::Boundary);
    }

    #[test]
    fn feature_edge_silhouette_detected() {
        let edge = FeatureEdgeState {
            start_world: Vec3::ZERO,
            end_world: Vec3::X,
            normals: [Vec3::Y, Vec3::NEG_Y],
            front_facing: [true, false],
            total_faces: 2,
        };
        assert!(edge.is_visible_candidate());
        assert_eq!(edge.classify(), EdgeType::Silhouette);
    }

    #[test]
    fn feature_edge_crease_detected() {
        let edge = FeatureEdgeState {
            start_world: Vec3::ZERO,
            end_world: Vec3::X,
            normals: [Vec3::Y, Vec3::Z],
            front_facing: [true, true],
            total_faces: 2,
        };
        assert!(edge.is_visible_candidate());
        assert_eq!(edge.classify(), EdgeType::Crease);
    }

    #[test]
    fn project_edge_ndc_to_viewport() {
        // Identity view_proj: clip space = world space, NDC maps directly
        let identity = Mat4::IDENTITY;
        let edge = project_edge(
            Vec3::new(-0.5, 0.5, 0.0),
            Vec3::new(0.5, -0.5, 0.0),
            EdgeType::Silhouette,
            &identity,
            100.0,
            100.0,
        )
        .unwrap();
        // NDC (-0.5, 0.5) → viewport (25, 25)
        assert!((edge.x1 - 25.0).abs() < 0.1);
        assert!((edge.y1 - 25.0).abs() < 0.1);
        // NDC (0.5, -0.5) → viewport (75, 75)
        assert!((edge.x2 - 75.0).abs() < 0.1);
        assert!((edge.y2 - 75.0).abs() < 0.1);
    }

    #[test]
    fn svg_output_contains_vector_paths() {
        let drawing = DrawingGeometry {
            edges: vec![
                ProjectedEdge {
                    x1: 10.0,
                    y1: 20.0,
                    x2: 90.0,
                    y2: 80.0,
                    edge_type: EdgeType::Silhouette,
                },
                ProjectedEdge {
                    x1: 5.0,
                    y1: 5.0,
                    x2: 50.0,
                    y2: 50.0,
                    edge_type: EdgeType::Dimension,
                },
            ],
            dimension_labels: vec![ProjectedDimensionLabel {
                x: 0.5,
                y: 0.5,
                text: "2.50m".to_string(),
                font_size_pt: 10.0,
            }],
            section_fills: vec![],
            drafting_primitives: vec![],
            viewport: DrawingViewport {
                width: 100.0,
                height: 100.0,
                world_width: 10.0,
                world_height: 10.0,
                scale: 1.0,
                paper_mm_to_canvas: 1.0,
            },
        };
        let svg = String::from_utf8(drawing_to_svg(&drawing)).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains(r#"class="silhouette""#));
        assert!(svg.contains(r#"class="dimension""#));
        assert!(svg.contains("<path d="));
        assert!(svg.contains("2.50m"));
        // Should NOT contain base64 image embedding
        assert!(!svg.contains("data:image/png;base64,"));
    }

    #[test]
    fn pdf_output_contains_vector_operators() {
        let drawing = DrawingGeometry {
            edges: vec![ProjectedEdge {
                x1: 10.0,
                y1: 20.0,
                x2: 90.0,
                y2: 80.0,
                edge_type: EdgeType::Silhouette,
            }],
            dimension_labels: vec![],
            section_fills: vec![],
            drafting_primitives: vec![],
            viewport: DrawingViewport {
                width: 595.0,
                height: 842.0,
                world_width: 10.0,
                world_height: 14.14,
                scale: 1.0,
                paper_mm_to_canvas: 1.0,
            },
        };
        let pdf = drawing_to_pdf(&drawing);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("%PDF-1.4"));
        assert!(text.contains("/Helvetica"));
        // Should contain vector line operators, not image
        assert!(text.contains(" m "));
        assert!(text.contains(" l S"));
        assert!(text.contains(" w\n")); // line width operator
        // Should NOT contain DCTDecode (JPEG image)
        assert!(!text.contains("/DCTDecode"));
    }

    #[test]
    fn svg_escapes_special_characters() {
        assert_eq!(svg_escape("a < b & c"), "a &lt; b &amp; c");
    }

    #[test]
    fn pdf_escapes_parentheses() {
        assert_eq!(pdf_escape_text("test (1)"), "test \\(1\\)");
    }

    #[test]
    fn dimension_display_text_uses_doc_defaults() {
        use crate::plugins::units::DisplayUnit;
        let node = DimensionLineNode {
            start: Vec3::ZERO,
            end: Vec3::new(2.5, 0.0, 0.0),
            line_point: Vec3::new(1.25, -0.5, 0.0),
            extension: 0.15,
            visible: true,
            label: None,
            display_unit: None,
            precision: None,
        };
        let doc_props = DocumentProperties {
            display_unit: DisplayUnit::Metres,
            precision: 2,
            ..Default::default()
        };
        let text = dimension_display_text(&node, &doc_props);
        assert_eq!(text, "2.50m");
    }
}
