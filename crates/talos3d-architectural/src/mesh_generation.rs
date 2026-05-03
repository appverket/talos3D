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
            evaluate_chart_host_with_provenance, CaeFaceProvenanceMap, ChartDomain,
            ChartHostEvaluation, ChartSpaceOpeningFeature, HostChart, PlanarHostChart,
        },
        primitives::TriangleMesh,
    },
};
#[cfg(test)]
use talos3d_core::plugins::modeling::host_chart::ChartSpaceProfileLoop;

use crate::components::{OpeningFeature, OpeningFeatureOperation, Wall};

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
        &'static ElementId,
        &'static Wall,
        Option<&'static Mesh3d>,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
    ),
    With<NeedsMesh>,
>;

type OpeningFeatureQuery<'w, 's> =
    Query<'w, 's, (&'static ElementId, &'static OpeningFeature), Without<NeedsMesh>>;

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct OpeningRect {
    feature_ref: ElementId,
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
    opening_features: OpeningFeatureQuery,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, wall_element_id, wall, mesh_handle, material_handle) in &query {
        let wall_openings: Vec<ChartSpaceOpeningFeature> = opening_features
            .iter()
            .filter_map(|(opening_element_id, opening_feature)| {
                opening_feature_to_chart_space(
                    *wall_element_id,
                    *opening_element_id,
                    opening_feature,
                )
            })
            .collect();
        let (mesh, provenance_map) = if wall_openings.is_empty() {
            (wall_mesh(wall), None)
        } else {
            let evaluation =
                wall_chart_evaluation_with_opening_features(wall, *wall_element_id, &wall_openings);
            let mesh = triangle_mesh_to_bevy_mesh(&evaluation.mesh);
            (
                mesh,
                Some(CaeFaceProvenanceMap::new(evaluation.face_provenance)),
            )
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

        if let Some(provenance_map) = provenance_map {
            entity_commands.insert(provenance_map);
        } else {
            entity_commands.remove::<CaeFaceProvenanceMap>();
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

#[cfg(test)]
fn wall_mesh_with_openings(wall: &Wall, openings: &[OpeningRect]) -> Mesh {
    triangle_mesh_to_bevy_mesh(&wall_triangle_mesh_with_openings(
        wall,
        ElementId(0),
        openings,
    ))
}

#[cfg(test)]
fn wall_triangle_mesh_with_openings(
    wall: &Wall,
    wall_element_id: ElementId,
    openings: &[OpeningRect],
) -> TriangleMesh {
    wall_chart_evaluation_with_openings(wall, wall_element_id, openings).mesh
}

#[cfg(test)]
fn wall_chart_evaluation_with_openings(
    wall: &Wall,
    wall_element_id: ElementId,
    openings: &[OpeningRect],
) -> ChartHostEvaluation {
    let opening_features: Vec<ChartSpaceOpeningFeature> = openings
        .iter()
        .map(|opening| {
            ChartSpaceOpeningFeature::new(
                wall_element_id,
                "wall_local",
                ChartSpaceProfileLoop::rectangle(
                    Vec2::new(opening.x_min, opening.y_min),
                    Vec2::new(opening.x_max, opening.y_max),
                ),
            )
            .with_feature_ref(opening.feature_ref)
        })
        .collect();
    wall_chart_evaluation_with_opening_features(wall, wall_element_id, &opening_features)
}

fn wall_chart_evaluation_with_opening_features(
    wall: &Wall,
    wall_element_id: ElementId,
    openings: &[ChartSpaceOpeningFeature],
) -> ChartHostEvaluation {
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

    assert!(
        openings
            .iter()
            .all(|opening| opening.host_ref == wall_element_id),
        "wall opening features must reference the wall being evaluated"
    );

    evaluate_chart_host_with_provenance(&chart, openings)
        .expect("wall openings are clamped into the wall chart domain")
}

fn opening_feature_to_chart_space(
    wall_element_id: ElementId,
    feature_ref: ElementId,
    opening_feature: &OpeningFeature,
) -> Option<ChartSpaceOpeningFeature> {
    if opening_feature.host_ref != wall_element_id
        || opening_feature.operation != OpeningFeatureOperation::Cut
        || opening_feature.chart_anchor != OpeningFeature::WALL_EXTERIOR_CHART_ANCHOR
    {
        return None;
    }

    Some(
        ChartSpaceOpeningFeature::new(
            wall_element_id,
            "wall_local",
            opening_feature.profile_loop_2d.clone(),
        )
        .with_feature_ref(feature_ref)
        .with_depth(opening_feature.depth_policy)
        .with_clearance(opening_feature.clearance_policy),
    )
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
    use crate::components::{Opening, OpeningKind};
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
            feature_ref: ElementId(8),
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
            feature_ref: ElementId(8),
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
            feature_ref: ElementId(8),
            x_min: 1.4,
            x_max: 2.6,
            y_min: 0.9,
            y_max: 2.4,
        }];

        let triangle_mesh = wall_triangle_mesh_with_openings(&wall, ElementId(7), &openings);
        let report = check_triangle_mesh_health(&triangle_mesh);

        assert!(report.is_clean(), "{report:#?}");
    }

    #[test]
    fn wall_chart_evaluation_maps_reveals_to_opening_feature() {
        let wall = Wall {
            start: Vec2::ZERO,
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let openings = [OpeningRect {
            feature_ref: ElementId(8),
            x_min: 1.4,
            x_max: 2.6,
            y_min: 0.9,
            y_max: 2.4,
        }];

        let evaluation = wall_chart_evaluation_with_openings(&wall, ElementId(7), &openings);

        assert!(
            evaluation.face_provenance.iter().any(|provenance| {
                matches!(
                provenance.role,
                talos3d_core::plugins::modeling::host_chart::CaeGeneratedFaceRole::OpeningReveal {
                    ..
                }
            ) && provenance.owner_ref == Some(ElementId(8))
                    && provenance.host_ref == Some(ElementId(7))
            })
        );
    }

    #[test]
    fn wall_chart_evaluation_accepts_authored_opening_feature() {
        let wall = Wall {
            start: Vec2::ZERO,
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let opening = Opening {
            width: 1.2,
            height: 1.5,
            sill_height: 0.9,
            kind: OpeningKind::Window,
        };
        let authored_feature =
            OpeningFeature::rectangular_wall(ElementId(7), &wall, &opening, 0.5).unwrap();
        let chart_feature =
            opening_feature_to_chart_space(ElementId(7), ElementId(8), &authored_feature)
                .expect("feature belongs to the wall");

        let evaluation =
            wall_chart_evaluation_with_opening_features(&wall, ElementId(7), &[chart_feature]);
        let report = check_triangle_mesh_health(&evaluation.mesh);

        assert!(report.is_clean(), "{report:#?}");
        assert!(
            evaluation.face_provenance.iter().any(|provenance| {
                matches!(
                provenance.role,
                talos3d_core::plugins::modeling::host_chart::CaeGeneratedFaceRole::OpeningReveal {
                    ..
                }
            ) && provenance.owner_ref == Some(ElementId(8))
                    && provenance.host_ref == Some(ElementId(7))
            })
        );
    }

    #[test]
    fn opening_feature_to_chart_space_rejects_other_hosts() {
        let wall = Wall {
            start: Vec2::ZERO,
            end: Vec2::new(4.0, 0.0),
            height: 3.0,
            thickness: 0.2,
        };
        let opening = Opening {
            width: 1.2,
            height: 1.5,
            sill_height: 0.9,
            kind: OpeningKind::Window,
        };
        let authored_feature =
            OpeningFeature::rectangular_wall(ElementId(7), &wall, &opening, 0.5).unwrap();

        assert!(
            opening_feature_to_chart_space(ElementId(99), ElementId(8), &authored_feature)
                .is_none()
        );
    }
}
