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
        render_pipeline::RenderSettings,
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
    pub x: f32,
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
}

#[derive(Debug, Clone)]
pub struct DrawingGeometry {
    pub edges: Vec<ProjectedEdge>,
    pub dimension_labels: Vec<ProjectedDimensionLabel>,
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
            if let Some(screen_pos) = camera.world_to_viewport(camera_gt, midpoint).ok() {
                let text = dimension_display_text(node, doc_props);
                dimension_labels.push(ProjectedDimensionLabel {
                    x: screen_pos.x / vp_width,
                    y: screen_pos.y / vp_height,
                    text,
                    font_size_pt: 10.0,
                });
            }
        }
    }
    edges.extend(dim_edges);

    let viewport = DrawingViewport {
        width: vp_width,
        height: vp_height,
        world_width,
        world_height,
        scale: 1.0,
    };

    Some(DrawingGeometry {
        edges,
        dimension_labels,
        viewport,
    })
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

    // Group edges by type for consistent line weights
    let edge_groups: &[(EdgeType, &str)] = &[
        (EdgeType::SectionCut, "section-cut"),
        (EdgeType::Silhouette, "silhouette"),
        (EdgeType::Crease, "crease"),
        (EdgeType::Boundary, "boundary"),
        (EdgeType::Dimension, "dimension"),
    ];

    for (edge_type, class_name) in edge_groups {
        let weight_px = edge_type.default_weight_mm() * (w / drawing.viewport.world_width).max(0.5);
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

    // Dimension labels
    if !drawing.dimension_labels.is_empty() {
        writeln!(
            out,
            r#"  <g class="dimension-labels" font-family="Helvetica, Arial, sans-serif" text-anchor="middle">"#
        )
        .unwrap();
        for label in &drawing.dimension_labels {
            let x = label.x * w;
            let y = label.y * h;
            let fs = label.font_size_pt;
            writeln!(
                out,
                r#"    <text x="{x:.1}" y="{y:.1}" font-size="{fs}" fill="black">{}</text>"#,
                svg_escape(&label.text)
            )
            .unwrap();
        }
        writeln!(out, "  </g>").unwrap();
    }

    writeln!(out, "</svg>").unwrap();
    out
}

fn svg_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
        // Convert mm to points: mm * (72pt / 25.4mm)
        let weight_pt = edge_type.default_weight_mm() * (72.0 / 25.4);
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

    // Dimension labels as text
    if !drawing.dimension_labels.is_empty() {
        writeln!(content, "BT").unwrap();
        writeln!(content, "/F1 10 Tf").unwrap();
        writeln!(content, "0 0 0 rg").unwrap();
        for label in &drawing.dimension_labels {
            let x = label.x * w;
            let y = h - (label.y * h); // flip Y for PDF
            let escaped = pdf_escape_text(&label.text);
            writeln!(content, "{x:.1} {y:.1} Td ({escaped}) Tj").unwrap();
        }
        writeln!(content, "ET").unwrap();
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

fn pdf_escape_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

// ─── Internal geometry helpers ───────────────────────────────────────────────

struct MeshSubject {
    entity: Entity,
    mesh_handle: Handle<Mesh>,
    mesh_transform: GlobalTransform,
}

#[derive(Clone, Copy)]
struct SceneTriangle {
    entity: Entity,
    a: Vec3,
    b: Vec3,
    c: Vec3,
}

fn drawing_overlay_excluded(type_name: &str) -> bool {
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

fn collect_scene_triangles(
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

fn collect_classified_visible_edges(
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
            viewport: DrawingViewport {
                width: 100.0,
                height: 100.0,
                world_width: 10.0,
                world_height: 10.0,
                scale: 1.0,
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
            viewport: DrawingViewport {
                width: 595.0,
                height: 842.0,
                world_width: 10.0,
                world_height: 14.14,
                scale: 1.0,
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
