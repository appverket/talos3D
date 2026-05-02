use bevy::prelude::*;
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
};

use talos3d_core::plugins::modeling::mesh_generation::NeedsMesh;
#[cfg(feature = "perf-stats")]
use talos3d_core::plugins::perf_stats::{add_mesh_regen_count, PerfStats};
use talos3d_core::plugins::{
    identity::ElementId,
    modeling::{
        host_chart::{
            evaluate_chart_host_mesh_with_openings, ChartDomain, ChartSpaceOpeningFeature,
            ChartSpaceProfileLoop, HostChart, PlanarHostChart,
        },
        primitives::TriangleMesh,
    },
};

use crate::components::{Opening, ParentWall, Wall};

pub struct ArchitecturalMeshPlugin;

impl Plugin for ArchitecturalMeshPlugin {
    fn build(&self, app: &mut App) {
        use talos3d_core::plugins::modeling::mesh_generation::MeshGenerationSet;
        app.add_systems(Startup, setup_wall_material).add_systems(
            Update,
            spawn_wall_meshes.in_set(MeshGenerationSet::Generate),
        );
    }
}

#[derive(Resource, Clone)]
pub struct WallMaterial(pub Handle<StandardMaterial>);

type WallMeshQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Wall,
        Option<&'static Mesh3d>,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
    ),
    With<NeedsMesh>,
>;

type OpeningQuery<'w, 's> =
    Query<'w, 's, (&'static Opening, &'static ParentWall), Without<NeedsMesh>>;

#[derive(Debug, Clone, Copy)]
struct OpeningRect {
    x_min: f32,
    x_max: f32,
    y_min: f32,
    y_max: f32,
}

fn setup_wall_material(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(WallMaterial(materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.82, 0.78),
        perceptual_roughness: 0.9,
        ..default()
    })));
}

fn spawn_wall_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    wall_material: Res<WallMaterial>,
    query: WallMeshQuery,
    openings: OpeningQuery,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, wall, mesh_handle, material_handle) in &query {
        let wall_openings: Vec<OpeningRect> = openings
            .iter()
            .filter_map(|(opening, parent_wall)| {
                (parent_wall.wall_entity == entity).then_some(opening_rect(
                    wall,
                    opening,
                    parent_wall,
                ))
            })
            .flatten()
            .collect();
        let mesh = if wall_openings.is_empty() {
            wall_mesh(wall)
        } else {
            wall_mesh_with_openings(wall, &wall_openings)
        };
        let transform = wall_transform(wall);
        let mut entity_commands = commands.entity(entity);

        if let Some(mesh_handle) = mesh_handle {
            if let Some(existing_mesh) = meshes.get_mut(mesh_handle.id()) {
                *existing_mesh = mesh;
            } else {
                entity_commands.insert(Mesh3d(meshes.add(mesh)));
            }
        } else {
            entity_commands.insert(Mesh3d(meshes.add(mesh)));
        }

        if material_handle.is_none() {
            entity_commands.insert(MeshMaterial3d(wall_material.0.clone()));
        }

        entity_commands.remove::<NeedsMesh>().insert(transform);
        #[cfg(feature = "perf-stats")]
        {
            regenerated += 1;
        }
    }
    #[cfg(feature = "perf-stats")]
    if regenerated > 0 {
        add_mesh_regen_count(&mut perf_stats, regenerated);
    }
}

pub fn wall_center(wall: &Wall) -> Vec3 {
    let mid = (wall.start + wall.end) * 0.5;
    Vec3::new(mid.x, wall.height * 0.5, mid.y)
}

pub fn wall_rotation(wall: &Wall) -> Quat {
    let dir = wall.end - wall.start;
    let angle = dir.y.atan2(dir.x);
    Quat::from_rotation_y(-angle)
}

pub fn wall_transform(wall: &Wall) -> Transform {
    Transform::from_translation(wall_center(wall)).with_rotation(wall_rotation(wall))
}

pub fn wall_mesh(wall: &Wall) -> Mesh {
    let length = wall.start.distance(wall.end);
    Mesh::from(Cuboid::new(length, wall.height, wall.thickness))
}

fn wall_mesh_with_openings(wall: &Wall, openings: &[OpeningRect]) -> Mesh {
    triangle_mesh_to_bevy_mesh(&wall_triangle_mesh_with_openings(wall, openings))
}

fn wall_triangle_mesh_with_openings(wall: &Wall, openings: &[OpeningRect]) -> TriangleMesh {
    let length = wall.start.distance(wall.end);
    let chart = HostChart::planar(
        "wall_local",
        PlanarHostChart::new(
            Vec3::new(-length * 0.5, -wall.height * 0.5, 0.0),
            Vec3::X,
            Vec3::Y,
            ChartDomain::new(0.0, length),
            ChartDomain::new(0.0, wall.height),
            wall.thickness,
        ),
    );
    let opening_features: Vec<ChartSpaceOpeningFeature> = openings
        .iter()
        .map(|opening| {
            ChartSpaceOpeningFeature::new(
                ElementId(0),
                "wall_local",
                ChartSpaceProfileLoop::rectangle(
                    Vec2::new(opening.x_min, opening.y_min),
                    Vec2::new(opening.x_max, opening.y_max),
                ),
            )
        })
        .collect();

    evaluate_chart_host_mesh_with_openings(&chart, &opening_features)
        .expect("wall openings are clamped into the wall chart domain")
}

fn opening_rect(wall: &Wall, opening: &Opening, parent_wall: &ParentWall) -> Option<OpeningRect> {
    let wall_length = wall.start.distance(wall.end);
    if wall_length <= f32::EPSILON || opening.width <= 0.0 || opening.height <= 0.0 {
        return None;
    }

    let center_x = (parent_wall.position_along_wall.clamp(0.0, 1.0)) * wall_length;
    let half_width = opening.width * 0.5;
    let x_min = (center_x - half_width).clamp(0.0, wall_length);
    let x_max = (center_x + half_width).clamp(0.0, wall_length);
    let y_min = opening.sill_height.clamp(0.0, wall.height);
    let y_max = (opening.sill_height + opening.height).clamp(0.0, wall.height);

    (x_max - x_min > f32::EPSILON && y_max - y_min > f32::EPSILON).then_some(OpeningRect {
        x_min,
        x_max,
        y_min,
        y_max,
    })
}

fn triangle_mesh_to_bevy_mesh(triangle_mesh: &TriangleMesh) -> Mesh {
    let positions: Vec<[f32; 3]> = triangle_mesh
        .vertices
        .iter()
        .map(|vertex| [vertex.x, vertex.y, vertex.z])
        .collect();
    let normals: Vec<[f32; 3]> = compute_triangle_mesh_normals(triangle_mesh)
        .into_iter()
        .map(|normal| [normal.x, normal.y, normal.z])
        .collect();
    let indices: Vec<u32> = triangle_mesh
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
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0]; triangle_mesh.vertices.len()],
    );
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn compute_triangle_mesh_normals(triangle_mesh: &TriangleMesh) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; triangle_mesh.vertices.len()];

    for face in &triangle_mesh.faces {
        let [a, b, c] = *face;
        let (Some(a), Some(b), Some(c)) = (
            triangle_mesh.vertices.get(a as usize).copied(),
            triangle_mesh.vertices.get(b as usize).copied(),
            triangle_mesh.vertices.get(c as usize).copied(),
        ) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;
    use talos3d_core::plugins::modeling::geometry_health::check_triangle_mesh_health;

    fn mesh_y_bounds(mesh: &Mesh) -> (f32, f32) {
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("mesh should contain positions");
        };

        positions
            .iter()
            .map(|position| position[1])
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min_y, max_y), y| {
                (min_y.min(y), max_y.max(y))
            })
    }

    fn triangle_count(mesh: &Mesh) -> usize {
        match mesh.indices() {
            Some(Indices::U32(indices)) => indices.len() / 3,
            Some(Indices::U16(indices)) => indices.len() / 3,
            None => 0,
        }
    }

    #[test]
    fn wall_mesh_with_openings_stays_centered_in_local_space() {
        let wall = Wall {
            start: Vec2::ZERO,
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let openings = [OpeningRect {
            x_min: 1.4,
            x_max: 2.6,
            y_min: 0.9,
            y_max: 2.4,
        }];

        let mesh = wall_mesh_with_openings(&wall, &openings);
        let (min_y, max_y) = mesh_y_bounds(&mesh);

        assert!(
            (min_y + wall.height * 0.5).abs() < 1e-5,
            "min_y was {min_y}"
        );
        assert!(
            (max_y - wall.height * 0.5).abs() < 1e-5,
            "max_y was {max_y}"
        );
    }

    #[test]
    fn wall_mesh_with_openings_omits_internal_block_faces() {
        let wall = Wall {
            start: Vec2::ZERO,
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let openings = [OpeningRect {
            x_min: 1.4,
            x_max: 2.6,
            y_min: 0.9,
            y_max: 2.4,
        }];

        let mesh = wall_mesh_with_openings(&wall, &openings);

        // One centered opening creates eight solid elevation cells. A cuboid
        // per cell would emit 96 triangles and duplicate internal faces. The
        // wall surface only needs 64 triangles: front/back cells plus exterior
        // perimeter and opening reveal faces.
        assert_eq!(triangle_count(&mesh), 64);
    }

    #[test]
    fn wall_mesh_with_openings_has_clean_cae_health() {
        let wall = Wall {
            start: Vec2::ZERO,
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let openings = [OpeningRect {
            x_min: 1.4,
            x_max: 2.6,
            y_min: 0.9,
            y_max: 2.4,
        }];

        let triangle_mesh = wall_triangle_mesh_with_openings(&wall, &openings);
        let report = check_triangle_mesh_health(&triangle_mesh);

        assert!(report.is_clean(), "{report:#?}");
    }
}
