use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use delaunator::{triangulate, Point};
use talos3d_core::plugins::modeling::primitives::TriangleMesh;

use crate::{
    components::{ElevationCurve, NeedsTerrainMesh, TerrainMeshCache, TerrainSurface},
    reconstruction::{
        sample_boundary_support_points, sample_curve_points, sample_interior_support_points,
    },
};

const TERRAIN_SURFACE_COLOR: Color = Color::srgb(0.54, 0.62, 0.46);
const ELEVATION_LOW_COLOR: [f32; 3] = [0.22, 0.48, 0.82];
const ELEVATION_MID_COLOR: [f32; 3] = [0.33, 0.72, 0.34];
const ELEVATION_HIGH_COLOR: [f32; 3] = [0.65, 0.43, 0.22];
const ELEVATION_PEAK_COLOR: [f32; 3] = [0.92, 0.92, 0.9];
const TERRAIN_CONTOUR_COLOR: Color = Color::srgba(0.08, 0.09, 0.07, 0.9);
const CONTOUR_EPSILON: f32 = 1.0e-4;

pub struct TerrainGenerationPlugin;

#[derive(Resource, Clone)]
pub struct TerrainSurfaceMaterial(pub Handle<StandardMaterial>);

type TerrainSurfaceMeshQueryItem<'a> = (
    Entity,
    &'a TerrainSurface,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);

impl Plugin for TerrainGenerationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_terrain_material)
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
    commands.insert_resource(TerrainSurfaceMaterial(materials.add(StandardMaterial {
        base_color: TERRAIN_SURFACE_COLOR,
        perceptual_roughness: 0.95,
        metallic: 0.02,
        cull_mode: None,
        ..default()
    })));
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
            terrain_material.0.clone(),
        );
        commands
            .entity(entity)
            .try_insert(cache)
            .try_remove::<NeedsTerrainMesh>();
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
    let mut source_points = Vec::new();
    for source_id in &surface.source_curve_ids {
        let Some((_, curve)) = curves
            .iter()
            .find(|(element_id, _)| *element_id == source_id)
        else {
            continue;
        };
        source_points.extend(
            sample_curve_points(&curve.points, surface.drape_sample_spacing)
                .into_iter()
                .map(|point| point + surface.offset),
        );
    }

    if !surface.boundary.is_empty() && !source_points.is_empty() {
        source_points.extend(sample_boundary_support_points(
            &surface.boundary,
            surface.drape_sample_spacing,
            &source_points,
        ));
        source_points.extend(sample_interior_support_points(
            &surface.boundary,
            (surface.drape_sample_spacing * 3.0).max(surface.drape_sample_spacing),
            &source_points,
        ));
    }

    let vertices = dedupe_vertices(source_points);
    if vertices.len() < 3 {
        return TerrainMeshCache::default();
    }

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
        let Some((a, b, c)) = face_points(&vertices, face) else {
            continue;
        };
        if (b - a).cross(c - a).length_squared() <= f32::EPSILON {
            continue;
        }
        if triangle_area_xz(a, b, c) > surface.max_triangle_area.max(CONTOUR_EPSILON) {
            continue;
        }
        if !surface.boundary.is_empty() {
            let centroid = Vec2::new((a.x + b.x + c.x) / 3.0, (a.z + b.z + c.z) / 3.0);
            if !point_in_polygon(centroid, &surface.boundary) {
                continue;
            }
        }
        faces.push(face);
    }

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
) {
    let mut entity_commands = commands.entity(entity);
    let mesh = terrain_mesh_asset(primitive);
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

fn terrain_mesh_asset(primitive: &TriangleMesh) -> Mesh {
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
}
