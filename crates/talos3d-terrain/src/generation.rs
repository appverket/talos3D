use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use delaunator::{triangulate, Point};
use talos3d_core::plugins::modeling::primitives::TriangleMesh;

use crate::{
    components::{
        ElevationCurve, NeedsTerrainMesh, TerrainMeshCache, TerrainSurface, TerrainSurfaceRole,
    },
    reconstruction::{
        sample_boundary_support_points, sample_curve_points, sample_interior_support_points,
    },
    visualization::{visualization_for_mode, TerrainVisualizationMode, TerrainVisualizationState},
};

const TERRAIN_SURFACE_COLOR: Color = Color::srgb(0.54, 0.62, 0.46);
const PROPOSED_TERRAIN_SURFACE_COLOR: Color = Color::srgb(0.36, 0.58, 0.86);
const ELEVATION_LOW_COLOR: [f32; 3] = [0.22, 0.48, 0.82];
const ELEVATION_MID_COLOR: [f32; 3] = [0.33, 0.72, 0.34];
const ELEVATION_HIGH_COLOR: [f32; 3] = [0.65, 0.43, 0.22];
const ELEVATION_PEAK_COLOR: [f32; 3] = [0.92, 0.92, 0.9];
const TERRAIN_CONTOUR_COLOR: Color = Color::srgba(0.08, 0.09, 0.07, 0.9);
const CONTOUR_EPSILON: f32 = 1.0e-4;
type BreaklineSegment = (usize, usize);

pub struct TerrainGenerationPlugin;

#[derive(Resource, Clone)]
pub struct TerrainSurfaceMaterial {
    pub existing: Handle<StandardMaterial>,
    pub proposed: Handle<StandardMaterial>,
}

impl TerrainSurfaceMaterial {
    fn for_surface(&self, surface: &TerrainSurface) -> Handle<StandardMaterial> {
        match surface.role {
            TerrainSurfaceRole::Existing => self.existing.clone(),
            TerrainSurfaceRole::Proposed => self.proposed.clone(),
        }
    }
}

type TerrainSurfaceMeshQueryItem<'a> = (
    Entity,
    &'a TerrainSurface,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);

impl Plugin for TerrainGenerationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainVisualizationState>()
            .add_systems(Startup, setup_terrain_material)
            .add_systems(
                Update,
                (
                    mark_terrain_surfaces_dirty_on_curve_changes,
                    regenerate_terrain_meshes,
                    draw_elevation_curves,
                    draw_terrain_contours,
                ),
            );
    }
}

fn setup_terrain_material(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(TerrainSurfaceMaterial {
        existing: materials.add(terrain_surface_material(TERRAIN_SURFACE_COLOR)),
        proposed: materials.add(terrain_surface_material(PROPOSED_TERRAIN_SURFACE_COLOR)),
    });
}

fn mark_terrain_surfaces_dirty_on_curve_changes(
    changed_curves: Query<&talos3d_core::plugins::identity::ElementId, Changed<ElevationCurve>>,
    mut surfaces: Query<(&TerrainSurface, Option<&NeedsTerrainMesh>, Entity)>,
    mut commands: Commands,
) {
    let changed_ids = changed_curves.iter().copied().collect::<Vec<_>>();
    if changed_ids.is_empty() {
        return;
    }

    for (surface, needs_mesh, entity) in &mut surfaces {
        if needs_mesh.is_some() {
            continue;
        }
        if surface
            .source_curve_ids
            .iter()
            .any(|element_id| changed_ids.contains(element_id))
        {
            commands.entity(entity).insert(NeedsTerrainMesh);
        }
    }
}

fn regenerate_terrain_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    terrain_material: Res<TerrainSurfaceMaterial>,
    visualization_state: Res<TerrainVisualizationState>,
    surfaces: Query<TerrainSurfaceMeshQueryItem<'_>, With<NeedsTerrainMesh>>,
    curves: Query<(&talos3d_core::plugins::identity::ElementId, &ElevationCurve)>,
) {
    for (entity, surface, mesh_handle, material_handle) in &surfaces {
        let cache = generate_terrain_mesh_cache(surface, &curves);
        upsert_terrain_mesh_entity(
            &mut commands,
            &mut meshes,
            entity,
            mesh_handle,
            material_handle,
            &cache.mesh,
            terrain_material.for_surface(surface),
            *visualization_state,
        );
        commands
            .entity(entity)
            .try_insert(cache)
            .try_remove::<NeedsTerrainMesh>();
    }
}

fn terrain_surface_material(base_color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color,
        perceptual_roughness: 0.95,
        metallic: 0.02,
        cull_mode: None,
        ..default()
    }
}

fn draw_elevation_curves(
    curves: Query<(&ElevationCurve, Option<&Visibility>)>,
    mut gizmos: Gizmos,
) {
    let mut min_elevation = f32::INFINITY;
    let mut max_elevation = f32::NEG_INFINITY;
    for (curve, visibility) in &curves {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        min_elevation = min_elevation.min(curve.elevation);
        max_elevation = max_elevation.max(curve.elevation);
    }

    for (curve, visibility) in &curves {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        let color = elevation_curve_color(curve.elevation, min_elevation, max_elevation);
        for segment in curve.points.windows(2) {
            gizmos.line(segment[0], segment[1], color);
        }
    }
}

fn draw_terrain_contours(
    surfaces: Query<(&TerrainMeshCache, Option<&Visibility>)>,
    mut gizmos: Gizmos,
) {
    for (cache, visibility) in &surfaces {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        for [start, end] in &cache.contour_segments {
            gizmos.line(*start, *end, TERRAIN_CONTOUR_COLOR);
        }
    }
}

pub fn generate_terrain_mesh_cache(
    surface: &TerrainSurface,
    curves: &Query<(&talos3d_core::plugins::identity::ElementId, &ElevationCurve)>,
) -> TerrainMeshCache {
    let effective_spacing = adaptive_sampling_spacing(surface);
    let mut source_points = Vec::new();
    let mut sampled_curves = Vec::<Vec<Vec3>>::new();
    for source_id in &surface.source_curve_ids {
        let Some((_, curve)) = curves
            .iter()
            .find(|(element_id, _)| *element_id == source_id)
        else {
            continue;
        };
        let sampled_curve = sample_curve_points(&curve.points, effective_spacing)
            .into_iter()
            .map(|point| point + surface.offset)
            .collect::<Vec<_>>();
        source_points.extend(sampled_curve.iter().copied());
        sampled_curves.push(sampled_curve);
    }

    if !surface.boundary.is_empty() && !source_points.is_empty() {
        source_points.extend(sample_boundary_support_points(
            &surface.boundary,
            effective_spacing,
            &source_points,
        ));
        source_points.extend(sample_interior_support_points(
            &surface.boundary,
            effective_spacing,
            &source_points,
        ));
    }

    let vertices = dedupe_vertices(source_points);
    if vertices.len() < 3 {
        return TerrainMeshCache::default();
    }

    let breakline_segments = build_breakline_segments(&vertices, &sampled_curves);
    let faces = build_terrain_faces(surface, &vertices, &breakline_segments);
    let mesh = TriangleMesh {
        vertices: vertices.clone(),
        faces: faces.clone(),
        normals: None,
        name: Some(surface.name.clone()),
    };
    let contour_segments = generate_contour_segments(&vertices, &faces, surface.contour_interval);
    TerrainMeshCache {
        mesh,
        contour_segments,
    }
}

fn build_terrain_faces(
    surface: &TerrainSurface,
    vertices: &[Vec3],
    breakline_segments: &[BreaklineSegment],
) -> Vec<[u32; 3]> {
    let triangulation = triangulate(
        &vertices
            .iter()
            .map(|point| Point {
                x: f64::from(point.x),
                y: f64::from(point.z),
            })
            .collect::<Vec<_>>(),
    );

    let mut faces = Vec::new();
    for triangle in triangulation.triangles.chunks_exact(3) {
        let face = [triangle[0] as u32, triangle[1] as u32, triangle[2] as u32];
        let Some((a, b, c)) = face_points(vertices, face) else {
            continue;
        };
        if (b - a).cross(c - a).length_squared() <= f32::EPSILON {
            continue;
        }
        if triangle_area_xz(a, b, c) > surface.max_triangle_area.max(CONTOUR_EPSILON) {
            continue;
        }
        if minimum_triangle_angle_degrees(a, b, c) < surface.minimum_angle.max(0.0) {
            continue;
        }
        if !surface.boundary.is_empty() {
            let centroid = Vec2::new((a.x + b.x + c.x) / 3.0, (a.z + b.z + c.z) / 3.0);
            if !point_in_polygon(centroid, &surface.boundary) {
                continue;
            }
        }
        if triangle_crosses_breakline(vertices, face, breakline_segments) {
            continue;
        }
        faces.push(face);
    }
    faces
}

fn adaptive_sampling_spacing(surface: &TerrainSurface) -> f32 {
    let contour_guided_spacing =
        (surface.contour_interval.max(CONTOUR_EPSILON) * 1.5).max(CONTOUR_EPSILON);
    surface
        .drape_sample_spacing
        .min(contour_guided_spacing)
        .max(CONTOUR_EPSILON)
}

pub fn sample_surface_elevation(mesh: &TriangleMesh, x: f32, z: f32) -> Option<f32> {
    let point = Vec2::new(x, z);
    mesh.faces
        .iter()
        .filter_map(|face| {
            let (a, b, c) = face_points(&mesh.vertices, *face)?;
            barycentric_height(point, a, b, c)
        })
        .next()
}

pub fn clip_mesh_to_boundary(mesh: &TriangleMesh, boundary: &[Vec2]) -> Option<TriangleMesh> {
    if boundary.is_empty() {
        return Some(mesh.clone());
    }
    let faces = mesh
        .faces
        .iter()
        .copied()
        .filter(|face| {
            let Some((a, b, c)) = face_points(&mesh.vertices, *face) else {
                return false;
            };
            point_in_polygon(
                Vec2::new((a.x + b.x + c.x) / 3.0, (a.z + b.z + c.z) / 3.0),
                boundary,
            )
        })
        .collect::<Vec<_>>();
    (!faces.is_empty()).then_some(TriangleMesh {
        vertices: mesh.vertices.clone(),
        faces,
        normals: mesh.normals.clone(),
        name: mesh.name.clone(),
    })
}

pub fn volume_above_datum(mesh: &TriangleMesh, datum_y: f32) -> Option<f64> {
    let mut volume = 0.0f64;
    for face in &mesh.faces {
        let (a, b, c) = face_points(&mesh.vertices, *face)?;
        let area_xz =
            0.5 * f64::from(((b.x - a.x) * (c.z - a.z) - (b.z - a.z) * (c.x - a.x)).abs());
        let mean_height = f64::from(((a.y + b.y + c.y) / 3.0) - datum_y);
        volume += area_xz * mean_height;
    }
    Some(volume)
}

fn upsert_terrain_mesh_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    entity: Entity,
    mesh_handle: Option<&Mesh3d>,
    material_handle: Option<&MeshMaterial3d<StandardMaterial>>,
    primitive: &TriangleMesh,
    material: Handle<StandardMaterial>,
    visualization_state: TerrainVisualizationState,
) {
    let mut entity_commands = commands.entity(entity);
    let mesh = terrain_mesh_asset(primitive, visualization_state);
    if let Some(mesh_handle) = mesh_handle {
        if let Some(existing_mesh) = meshes.get_mut(mesh_handle.id()) {
            *existing_mesh = mesh;
        } else {
            entity_commands.try_insert(Mesh3d(meshes.add(mesh)));
        }
    } else {
        entity_commands.try_insert(Mesh3d(meshes.add(mesh)));
    }

    if material_handle.is_none() {
        entity_commands.try_insert(MeshMaterial3d(material));
    }
    entity_commands.try_insert(Transform::IDENTITY);
}

fn terrain_mesh_asset(
    primitive: &TriangleMesh,
    visualization_state: TerrainVisualizationState,
) -> Mesh {
    if visualization_state.mode != TerrainVisualizationMode::Standard {
        if let Some(mesh) = visualized_terrain_mesh_asset(primitive, visualization_state) {
            return mesh;
        }
    }

    let positions: Vec<[f32; 3]> = primitive
        .vertices
        .iter()
        .map(|vertex| [vertex.x, vertex.y, vertex.z])
        .collect();
    let normals = compute_triangle_mesh_normals(primitive);
    let normals: Vec<[f32; 3]> = normals
        .into_iter()
        .map(|normal| [normal.x, normal.y, normal.z])
        .collect();
    let uvs = planar_uvs(&primitive.vertices);
    let indices: Vec<u32> = primitive
        .faces
        .iter()
        .flat_map(|face| face.iter().copied())
        .collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn visualized_terrain_mesh_asset(
    primitive: &TriangleMesh,
    visualization_state: TerrainVisualizationState,
) -> Option<Mesh> {
    let visualizations = visualization_for_mode(primitive, visualization_state);
    if visualizations.is_empty() {
        return None;
    }

    let source_uvs = planar_uvs(&primitive.vertices);
    let mut positions = Vec::with_capacity(visualizations.len() * 3);
    let mut normals = Vec::with_capacity(visualizations.len() * 3);
    let mut uvs = Vec::with_capacity(visualizations.len() * 3);
    let mut colors = Vec::with_capacity(visualizations.len() * 3);
    for visualization in visualizations {
        let Some((a, b, c)) = face_points(&primitive.vertices, visualization.face) else {
            continue;
        };
        let normal = (b - a).cross(c - a).normalize_or_zero();
        let normal = if normal.length_squared() <= f32::EPSILON {
            Vec3::Y
        } else {
            normal.normalize()
        };
        for vertex_index in visualization.face {
            let Some(vertex) = primitive.vertices.get(vertex_index as usize) else {
                continue;
            };
            positions.push([vertex.x, vertex.y, vertex.z]);
            normals.push([normal.x, normal.y, normal.z]);
            uvs.push(
                source_uvs
                    .get(vertex_index as usize)
                    .copied()
                    .unwrap_or([0.0, 0.0]),
            );
            colors.push(visualization.color);
        }
    }
    if positions.is_empty() {
        return None;
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    Some(mesh)
}

fn compute_triangle_mesh_normals(primitive: &TriangleMesh) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; primitive.vertices.len()];
    for face in &primitive.faces {
        let Some((a, b, c)) = face_points(&primitive.vertices, *face) else {
            continue;
        };
        let normal = (b - a).cross(c - a).normalize_or_zero();
        for vertex_index in face {
            if let Some(accumulator) = normals.get_mut(*vertex_index as usize) {
                *accumulator += normal;
            }
        }
    }
    normals
        .into_iter()
        .map(|normal| {
            if normal.length_squared() <= f32::EPSILON {
                Vec3::Y
            } else {
                normal.normalize()
            }
        })
        .collect()
}

fn dedupe_vertices(points: Vec<Vec3>) -> Vec<Vec3> {
    let mut unique = Vec::<Vec3>::new();
    for point in points {
        if unique
            .iter()
            .any(|existing| existing.distance_squared(point) <= CONTOUR_EPSILON)
        {
            continue;
        }
        unique.push(point);
    }
    unique
}

fn build_breakline_segments(
    vertices: &[Vec3],
    sampled_curves: &[Vec<Vec3>],
) -> Vec<BreaklineSegment> {
    let mut segments = Vec::new();
    for curve in sampled_curves {
        for segment in curve.windows(2) {
            if segment[0].distance_squared(segment[1]) <= CONTOUR_EPSILON {
                continue;
            }
            let Some(start) = find_vertex_index(vertices, segment[0]) else {
                continue;
            };
            let Some(end) = find_vertex_index(vertices, segment[1]) else {
                continue;
            };
            if start != end {
                segments.push((start, end));
            }
        }
    }
    segments
}

fn find_vertex_index(vertices: &[Vec3], point: Vec3) -> Option<usize> {
    vertices
        .iter()
        .position(|candidate| candidate.distance_squared(point) <= CONTOUR_EPSILON)
}

fn planar_uvs(vertices: &[Vec3]) -> Vec<[f32; 2]> {
    if vertices.is_empty() {
        return Vec::new();
    }
    let mut min = vertices[0].xz();
    let mut max = vertices[0].xz();
    for vertex in vertices {
        min = min.min(vertex.xz());
        max = max.max(vertex.xz());
    }
    let extent = (max - min).max(Vec2::splat(CONTOUR_EPSILON));
    vertices
        .iter()
        .map(|vertex| {
            let xz = vertex.xz();
            [(xz.x - min.x) / extent.x, (xz.y - min.y) / extent.y]
        })
        .collect()
}

fn face_points(vertices: &[Vec3], face: [u32; 3]) -> Option<(Vec3, Vec3, Vec3)> {
    Some((
        *vertices.get(face[0] as usize)?,
        *vertices.get(face[1] as usize)?,
        *vertices.get(face[2] as usize)?,
    ))
}

fn point_in_polygon(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return true;
    }
    let mut inside = false;
    let mut previous = *polygon.last().unwrap_or(&Vec2::ZERO);
    for current in polygon {
        let crosses = ((current.y > point.y) != (previous.y > point.y))
            && (point.x
                < (previous.x - current.x) * (point.y - current.y)
                    / ((previous.y - current.y).abs().max(f32::EPSILON))
                    + current.x);
        if crosses {
            inside = !inside;
        }
        previous = *current;
    }
    inside
}

fn triangle_area_xz(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    ((b.x - a.x) * (c.z - a.z) - (b.z - a.z) * (c.x - a.x)).abs() * 0.5
}

fn minimum_triangle_angle_degrees(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    angle_degrees_xz(a.xz(), b.xz(), c.xz())
        .min(angle_degrees_xz(b.xz(), c.xz(), a.xz()))
        .min(angle_degrees_xz(c.xz(), a.xz(), b.xz()))
}

fn angle_degrees_xz(origin: Vec2, left: Vec2, right: Vec2) -> f32 {
    let left = left - origin;
    let right = right - origin;
    if left.length_squared() <= CONTOUR_EPSILON || right.length_squared() <= CONTOUR_EPSILON {
        return 0.0;
    }
    let dot = left.normalize().dot(right.normalize()).clamp(-1.0, 1.0);
    dot.acos().to_degrees()
}

fn triangle_crosses_breakline(
    vertices: &[Vec3],
    face: [u32; 3],
    breakline_segments: &[BreaklineSegment],
) -> bool {
    let edges = [
        (face[0] as usize, face[1] as usize),
        (face[1] as usize, face[2] as usize),
        (face[2] as usize, face[0] as usize),
    ];
    edges.iter().any(|edge| {
        breakline_segments.iter().any(|breakline| {
            !shares_endpoint(*edge, *breakline)
                && segments_properly_intersect_xz(
                    vertices[edge.0],
                    vertices[edge.1],
                    vertices[breakline.0],
                    vertices[breakline.1],
                )
        })
    })
}

fn shares_endpoint(left: BreaklineSegment, right: BreaklineSegment) -> bool {
    left.0 == right.0 || left.0 == right.1 || left.1 == right.0 || left.1 == right.1
}

fn segments_properly_intersect_xz(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> bool {
    let a = a.xz();
    let b = b.xz();
    let c = c.xz();
    let d = d.xz();
    let o1 = orientation_xz(a, b, c);
    let o2 = orientation_xz(a, b, d);
    let o3 = orientation_xz(c, d, a);
    let o4 = orientation_xz(c, d, b);
    o1 * o2 < -CONTOUR_EPSILON && o3 * o4 < -CONTOUR_EPSILON
}

fn orientation_xz(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn generate_contour_segments(
    vertices: &[Vec3],
    faces: &[[u32; 3]],
    contour_interval: f32,
) -> Vec<[Vec3; 2]> {
    if contour_interval <= CONTOUR_EPSILON {
        return Vec::new();
    }

    let mut segments = Vec::new();
    for face in faces {
        let Some((a, b, c)) = face_points(vertices, *face) else {
            continue;
        };
        let min_y = a.y.min(b.y).min(c.y);
        let max_y = a.y.max(b.y).max(c.y);
        let start = (min_y / contour_interval).ceil() as i32;
        let end = (max_y / contour_interval).floor() as i32;
        for step in start..=end {
            let elevation = step as f32 * contour_interval;
            let intersections = triangle_contour_intersections(a, b, c, elevation);
            if intersections.len() == 2 {
                segments.push([intersections[0], intersections[1]]);
            }
        }
    }
    segments
}

fn triangle_contour_intersections(a: Vec3, b: Vec3, c: Vec3, elevation: f32) -> Vec<Vec3> {
    let mut intersections = Vec::new();
    for (start, end) in [(a, b), (b, c), (c, a)] {
        let start_delta = start.y - elevation;
        let end_delta = end.y - elevation;
        if start_delta.abs() <= CONTOUR_EPSILON && end_delta.abs() <= CONTOUR_EPSILON {
            continue;
        }
        if start_delta.abs() <= CONTOUR_EPSILON {
            intersections.push(start);
            continue;
        }
        if end_delta.abs() <= CONTOUR_EPSILON {
            intersections.push(end);
            continue;
        }
        if (start_delta > 0.0) == (end_delta > 0.0) {
            continue;
        }
        let t = (elevation - start.y) / (end.y - start.y);
        intersections.push(start.lerp(end, t));
    }
    intersections.truncate(2);
    intersections
}

fn barycentric_height(point: Vec2, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let v0 = Vec2::new(b.x - a.x, b.z - a.z);
    let v1 = Vec2::new(c.x - a.x, c.z - a.z);
    let v2 = point - Vec2::new(a.x, a.z);
    let denominator = v0.x * v1.y - v1.x * v0.y;
    if denominator.abs() <= CONTOUR_EPSILON {
        return None;
    }
    let inv_denominator = 1.0 / denominator;
    let u = (v2.x * v1.y - v1.x * v2.y) * inv_denominator;
    let v = (v0.x * v2.y - v2.x * v0.y) * inv_denominator;
    if u < -CONTOUR_EPSILON || v < -CONTOUR_EPSILON || (u + v) > 1.0 + CONTOUR_EPSILON {
        return None;
    }
    Some(a.y + u * (b.y - a.y) + v * (c.y - a.y))
}

fn elevation_curve_color(elevation: f32, min_elevation: f32, max_elevation: f32) -> Color {
    let range = (max_elevation - min_elevation).max(CONTOUR_EPSILON);
    let t = ((elevation - min_elevation) / range).clamp(0.0, 1.0);
    let color = if t < 0.33 {
        lerp_color(ELEVATION_LOW_COLOR, ELEVATION_MID_COLOR, t / 0.33)
    } else if t < 0.66 {
        lerp_color(ELEVATION_MID_COLOR, ELEVATION_HIGH_COLOR, (t - 0.33) / 0.33)
    } else {
        lerp_color(
            ELEVATION_HIGH_COLOR,
            ELEVATION_PEAK_COLOR,
            (t - 0.66) / 0.34,
        )
    };
    Color::srgb(color[0], color[1], color[2])
}

fn lerp_color(start: [f32; 3], end: [f32; 3], t: f32) -> [f32; 3] {
    [
        start[0] + (end[0] - start[0]) * t,
        start[1] + (end[1] - start[1]) * t,
        start[2] + (end[2] - start[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;
    use std::time::Instant;
    use talos3d_core::plugins::identity::ElementId;

    #[test]
    fn samples_triangle_height_inside_face() {
        let mesh = TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 2.0, 1.0),
            ],
            faces: vec![[0, 1, 2]],
            normals: None,
            name: None,
        };

        let elevation =
            sample_surface_elevation(&mesh, 0.25, 0.25).expect("point inside face should sample");
        assert!(elevation >= 0.0);
        assert!(elevation <= 2.0);
    }

    #[test]
    fn clips_mesh_faces_by_boundary_centroid() {
        let mesh = TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(5.0, 0.0, 5.0),
            ],
            faces: vec![[0, 1, 2], [1, 2, 3]],
            normals: None,
            name: None,
        };
        let boundary = vec![
            Vec2::new(-1.0, -1.0),
            Vec2::new(2.0, -1.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(-1.0, 2.0),
        ];
        let clipped = clip_mesh_to_boundary(&mesh, &boundary).expect("clipped mesh should exist");
        assert_eq!(clipped.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn planar_uvs_follow_xz_extent() {
        let uvs = planar_uvs(&[
            Vec3::new(10.0, 1.0, 20.0),
            Vec3::new(20.0, 1.0, 20.0),
            Vec3::new(10.0, 2.0, 40.0),
        ]);

        assert_eq!(uvs[0], [0.0, 0.0]);
        assert_eq!(uvs[1], [1.0, 0.0]);
        assert_eq!(uvs[2], [0.0, 1.0]);
    }

    #[test]
    fn changing_source_curve_marks_terrain_surface_dirty() {
        let mut app = App::new();
        app.add_systems(Update, mark_terrain_surfaces_dirty_on_curve_changes);
        let curve_entity = app
            .world_mut()
            .spawn((
                ElementId(10),
                ElevationCurve {
                    points: vec![Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 0.0)],
                    elevation: 1.0,
                    source_layer: "Contour".to_string(),
                    curve_type: crate::components::ElevationCurveType::Major,
                    survey_source_id: None,
                },
            ))
            .id();
        let surface_entity = app
            .world_mut()
            .spawn(TerrainSurface::new(
                "Surface".to_string(),
                vec![ElementId(10)],
            ))
            .id();

        app.world_mut().clear_trackers();
        app.world_mut()
            .entity_mut(curve_entity)
            .get_mut::<ElevationCurve>()
            .expect("curve exists")
            .elevation = 2.0;
        app.update();

        assert!(app
            .world()
            .entity(surface_entity)
            .contains::<NeedsTerrainMesh>());
    }

    #[test]
    fn active_visualization_mode_adds_vertex_colors_to_terrain_mesh() {
        let terrain = TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 0.5, 1.0),
            ],
            faces: vec![[0, 1, 2]],
            normals: None,
            name: None,
        };

        let standard = terrain_mesh_asset(&terrain, TerrainVisualizationState::default());
        assert!(standard.attribute(Mesh::ATTRIBUTE_COLOR).is_none());

        let visualized = terrain_mesh_asset(
            &terrain,
            TerrainVisualizationState {
                mode: TerrainVisualizationMode::Slope,
                elevation_band_width: 1.0,
            },
        );
        let colors = visualized
            .attribute(Mesh::ATTRIBUTE_COLOR)
            .expect("slope visualization should add vertex colors");
        match colors {
            VertexAttributeValues::Float32x4(values) => assert_eq!(values.len(), 3),
            other => panic!("unexpected color attribute format: {other:?}"),
        }
    }

    #[test]
    fn adaptive_sampling_spacing_tracks_dense_contour_intervals() {
        let surface = TerrainSurface {
            name: "Test".to_string(),
            source_curve_ids: vec![],
            role: TerrainSurfaceRole::Existing,
            datum_elevation: 0.0,
            boundary: vec![],
            max_triangle_area: 25.0,
            minimum_angle: 10.0,
            drape_sample_spacing: 1.5,
            contour_interval: 0.5,
            offset: Vec3::ZERO,
        };

        assert!((adaptive_sampling_spacing(&surface) - 0.75).abs() <= f32::EPSILON);
    }

    #[test]
    fn minimum_angle_filter_rejects_sliver_triangles() {
        let min_angle = minimum_triangle_angle_degrees(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(0.01, 0.0, 0.01),
        );

        assert!(min_angle < 1.0, "sliver min angle was {min_angle}");
    }

    #[test]
    fn breakline_filter_rejects_triangle_edges_that_cross_source_segments() {
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, 1.0),
        ];
        let breaklines = vec![(4, 5)];

        assert!(triangle_crosses_breakline(
            &vertices,
            [0, 1, 3],
            &breaklines
        ));
        assert!(!triangle_crosses_breakline(
            &vertices,
            [0, 4, 3],
            &breaklines
        ));
    }

    #[test]
    fn generation_filters_breakline_crossings_and_low_quality_triangles() {
        let surface = TerrainSurface {
            name: "Filtered".to_string(),
            source_curve_ids: vec![],
            role: TerrainSurfaceRole::Existing,
            datum_elevation: 0.0,
            boundary: vec![],
            max_triangle_area: 10.0,
            minimum_angle: 10.0,
            drape_sample_spacing: 1.0,
            contour_interval: 1.0,
            offset: Vec3::ZERO,
        };
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, 1.0),
        ];
        let breaklines = vec![(4, 5)];

        let faces = build_terrain_faces(&surface, &vertices, &breaklines);

        assert!(!faces.is_empty());
        assert!(faces.iter().all(|face| !triangle_crosses_breakline(
            &vertices,
            *face,
            &breaklines
        )));
        assert!(faces.iter().all(|face| {
            let (a, b, c) = face_points(&vertices, *face).expect("face indices are valid");
            minimum_triangle_angle_degrees(a, b, c) >= surface.minimum_angle
        }));
    }

    #[test]
    fn ten_thousand_vertex_terrain_generation_stays_under_two_seconds() {
        let mut vertices = Vec::with_capacity(10_000);
        for z in 0..100 {
            for x in 0..100 {
                let x = x as f32;
                let z = z as f32;
                vertices.push(Vec3::new(x, (x * 0.03 + z * 0.02).sin(), z));
            }
        }
        let surface = TerrainSurface {
            name: "Performance".to_string(),
            source_curve_ids: vec![],
            role: TerrainSurfaceRole::Existing,
            datum_elevation: 0.0,
            boundary: vec![],
            max_triangle_area: 2.0,
            minimum_angle: 0.1,
            drape_sample_spacing: 1.0,
            contour_interval: 1.0,
            offset: Vec3::ZERO,
        };

        let start = Instant::now();
        let faces = build_terrain_faces(&surface, &vertices, &[]);
        let elapsed = start.elapsed();

        assert!(!faces.is_empty());
        assert!(
            elapsed.as_secs_f32() < 2.0,
            "10k vertex terrain generation took {elapsed:?}"
        );
    }
}
