use bevy::prelude::*;
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
};

use talos3d_core::plugins::modeling::mesh_generation::NeedsMesh;
#[cfg(feature = "perf-stats")]
use talos3d_core::plugins::perf_stats::{add_mesh_regen_count, PerfStats};

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
    let length = wall.start.distance(wall.end);
    let thickness = wall.thickness;
    let mut x_breakpoints = vec![0.0, length];
    let mut y_breakpoints = vec![0.0, wall.height];
    let vertical_origin = wall.height * 0.5;

    for opening in openings {
        x_breakpoints.push(opening.x_min);
        x_breakpoints.push(opening.x_max);
        y_breakpoints.push(opening.y_min);
        y_breakpoints.push(opening.y_max);
    }

    sort_and_dedup_breakpoints(&mut x_breakpoints);
    sort_and_dedup_breakpoints(&mut y_breakpoints);

    let x_cells = x_breakpoints.len().saturating_sub(1);
    let y_cells = y_breakpoints.len().saturating_sub(1);
    let mut solid_cells = vec![false; x_cells * y_cells];

    for x_index in 0..x_cells {
        let x_min = x_breakpoints[x_index];
        let x_max = x_breakpoints[x_index + 1];
        if x_max - x_min <= f32::EPSILON {
            continue;
        }

        for y_index in 0..y_cells {
            let y_min = y_breakpoints[y_index];
            let y_max = y_breakpoints[y_index + 1];
            if y_max - y_min <= f32::EPSILON {
                continue;
            }

            let center_x = (x_min + x_max) * 0.5;
            let center_y = (y_min + y_max) * 0.5;
            let inside_opening = openings.iter().any(|opening| {
                center_x > opening.x_min
                    && center_x < opening.x_max
                    && center_y > opening.y_min
                    && center_y < opening.y_max
            });
            solid_cells[cell_index(x_index, y_index, y_cells)] = !inside_opening;
        }
    }

    let mut mesh = MeshBuilder::new();
    let front_z = -thickness * 0.5;
    let back_z = thickness * 0.5;

    for x_index in 0..x_cells {
        for y_index in 0..y_cells {
            if !solid_cells[cell_index(x_index, y_index, y_cells)] {
                continue;
            }

            let x_min = x_breakpoints[x_index] - length * 0.5;
            let x_max = x_breakpoints[x_index + 1] - length * 0.5;
            let y_min = y_breakpoints[y_index] - vertical_origin;
            let y_max = y_breakpoints[y_index + 1] - vertical_origin;

            mesh.add_quad(
                [
                    [x_min, y_min, front_z],
                    [x_min, y_max, front_z],
                    [x_max, y_max, front_z],
                    [x_max, y_min, front_z],
                ],
                [0.0, 0.0, -1.0],
            );
            mesh.add_quad(
                [
                    [x_min, y_min, back_z],
                    [x_max, y_min, back_z],
                    [x_max, y_max, back_z],
                    [x_min, y_max, back_z],
                ],
                [0.0, 0.0, 1.0],
            );

            if !is_solid_cell(
                &solid_cells,
                x_cells,
                y_cells,
                x_index as isize - 1,
                y_index as isize,
            ) {
                mesh.add_quad(
                    [
                        [x_min, y_min, front_z],
                        [x_min, y_min, back_z],
                        [x_min, y_max, back_z],
                        [x_min, y_max, front_z],
                    ],
                    [-1.0, 0.0, 0.0],
                );
            }
            if !is_solid_cell(
                &solid_cells,
                x_cells,
                y_cells,
                x_index as isize + 1,
                y_index as isize,
            ) {
                mesh.add_quad(
                    [
                        [x_max, y_min, front_z],
                        [x_max, y_max, front_z],
                        [x_max, y_max, back_z],
                        [x_max, y_min, back_z],
                    ],
                    [1.0, 0.0, 0.0],
                );
            }
            if !is_solid_cell(
                &solid_cells,
                x_cells,
                y_cells,
                x_index as isize,
                y_index as isize - 1,
            ) {
                mesh.add_quad(
                    [
                        [x_min, y_min, front_z],
                        [x_max, y_min, front_z],
                        [x_max, y_min, back_z],
                        [x_min, y_min, back_z],
                    ],
                    [0.0, -1.0, 0.0],
                );
            }
            if !is_solid_cell(
                &solid_cells,
                x_cells,
                y_cells,
                x_index as isize,
                y_index as isize + 1,
            ) {
                mesh.add_quad(
                    [
                        [x_min, y_max, front_z],
                        [x_min, y_max, back_z],
                        [x_max, y_max, back_z],
                        [x_max, y_max, front_z],
                    ],
                    [0.0, 1.0, 0.0],
                );
            }
        }
    }

    mesh.finish()
}

fn cell_index(x_index: usize, y_index: usize, y_cells: usize) -> usize {
    x_index * y_cells + y_index
}

fn is_solid_cell(
    solid_cells: &[bool],
    x_cells: usize,
    y_cells: usize,
    x_index: isize,
    y_index: isize,
) -> bool {
    if x_index < 0 || y_index < 0 {
        return false;
    }
    let (x_index, y_index) = (x_index as usize, y_index as usize);
    if x_index >= x_cells || y_index >= y_cells {
        return false;
    }
    solid_cells[cell_index(x_index, y_index, y_cells)]
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

fn sort_and_dedup_breakpoints(values: &mut Vec<f32>) {
    values.sort_by(|a, b| a.total_cmp(b));
    values.dedup_by(|a, b| (*a - *b).abs() < f32::EPSILON);
}

struct MeshBuilder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    fn new() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
        }
    }

    fn add_quad(&mut self, vertices: [[f32; 3]; 4], normal: [f32; 3]) {
        let base = self.positions.len() as u32;
        self.positions.extend(vertices);
        self.normals.extend([normal; 4]);
        self.uvs
            .extend([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        self.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn finish(self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

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
}
