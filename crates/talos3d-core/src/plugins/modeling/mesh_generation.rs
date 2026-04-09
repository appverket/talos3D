use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};

use crate::plugins::modeling::{
    array::EvaluatedArray,
    csg::EvaluatedCsg,
    editable_mesh::EditableMesh,
    group::GroupEditMuted,
    mirror::EvaluatedMirror,
    primitive_trait::{MeshGenerator, MeshMaterialKind},
    primitives::{
        BoxPrimitive, CylinderPrimitive, ElevationMetadata, PlanePrimitive, Polyline,
        ShapeRotation, SpherePrimitive, TriangleMesh,
    },
    profile::{ProfileExtrusion, ProfileRevolve, ProfileSweep},
    profile_feature::EvaluatedFeature,
};
#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, add_mesh_regen_count, PerfStats};

const PRIMITIVE_COLOR: Color = Color::srgb(0.78, 0.82, 0.88);
const PLANE_COLOR: Color = Color::srgba(0.72, 0.8, 0.92, 0.75);
const POLYLINE_COLOR: Color = Color::srgb(0.85, 0.95, 1.0);
const MUTED_POLYLINE_ALPHA: f32 = 0.15;
const ELEVATION_BAND_HEIGHT: f32 = 2.0;
const ELEVATION_BAND_COLORS: &[[f32; 3]] = &[
    [0.19, 0.73, 0.93],
    [0.16, 0.82, 0.63],
    [0.44, 0.85, 0.35],
    [0.82, 0.85, 0.28],
    [0.95, 0.63, 0.18],
    [0.92, 0.36, 0.22],
];

type PrimitiveMeshQueryItem<'a, P> = (
    Entity,
    &'a P,
    Option<&'a ShapeRotation>,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);
type PolylineDrawQueryItem<'a> = (
    &'a Polyline,
    Option<&'a Visibility>,
    Option<&'a ElevationMetadata>,
    Has<GroupEditMuted>,
);
type EvaluatedCsgQueryItem<'a> = (
    Entity,
    &'a EvaluatedCsg,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);
type EvaluatedFeatureQueryItem<'a> = (
    Entity,
    &'a EvaluatedFeature,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);
type EvaluatedMirrorQueryItem<'a> = (
    Entity,
    &'a EvaluatedMirror,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);
type EvaluatedArrayQueryItem<'a> = (
    Entity,
    &'a EvaluatedArray,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);

/// System set for the evaluation pipeline (CSG, constraints, etc.).
/// Runs after history commands are applied, before mesh generation.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvaluationSet {
    /// Run evaluation (CSG booleans, constraint solving, etc.).
    Evaluate,
}

/// System set for mesh generation from authored/evaluated data.
/// Runs after evaluation, produces renderable Bevy meshes.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshGenerationSet {
    /// Generate meshes from primitive components and evaluated bodies.
    Generate,
}

pub struct ModelingMeshPlugin;

impl Plugin for ModelingMeshPlugin {
    fn build(&self, app: &mut App) {
        use crate::plugins::history::HistorySet;

        // Order: HistorySet::Apply → EvaluationSet → MeshGenerationSet
        app.configure_sets(
            Update,
            EvaluationSet::Evaluate
                .after(HistorySet::Apply)
                .before(MeshGenerationSet::Generate),
        );
        app.configure_sets(
            Update,
            MeshGenerationSet::Generate.after(EvaluationSet::Evaluate),
        );

        app.add_systems(Startup, setup_modeling_materials)
            .add_systems(
                Update,
                (
                    spawn_primitive_meshes::<BoxPrimitive>,
                    spawn_primitive_meshes::<CylinderPrimitive>,
                    spawn_primitive_meshes::<SpherePrimitive>,
                    spawn_primitive_meshes::<PlanePrimitive>,
                    spawn_primitive_meshes::<ProfileExtrusion>,
                    spawn_primitive_meshes::<ProfileSweep>,
                    spawn_primitive_meshes::<ProfileRevolve>,
                    spawn_triangle_meshes,
                    spawn_editable_meshes,
                    spawn_evaluated_csg_meshes,
                    spawn_evaluated_feature_meshes,
                    spawn_evaluated_mirror_meshes,
                    spawn_evaluated_array_meshes,
                    draw_polylines,
                )
                    .in_set(MeshGenerationSet::Generate),
            );
    }
}

/// Marker: this entity needs re-evaluation before mesh generation.
/// Used by the CSG pipeline and future features (fillets, constraints).
#[derive(Component)]
pub struct NeedsEvaluation;

#[derive(Component)]
pub struct NeedsMesh;

#[derive(Resource, Clone)]
pub struct PrimitiveMaterial(pub Handle<StandardMaterial>);

#[derive(Resource, Clone)]
pub struct PlaneMaterial(pub Handle<StandardMaterial>);

type TriangleMeshQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static TriangleMesh,
        Option<&'static Mesh3d>,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
    ),
    With<NeedsMesh>,
>;

fn setup_modeling_materials(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(PrimitiveMaterial(materials.add(StandardMaterial {
        base_color: PRIMITIVE_COLOR,
        perceptual_roughness: 0.85,
        ..default()
    })));
    commands.insert_resource(PlaneMaterial(materials.add(StandardMaterial {
        base_color: PLANE_COLOR,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    })));
}

fn spawn_primitive_meshes<P: MeshGenerator>(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    plane_material: Res<PlaneMaterial>,
    query: Query<PrimitiveMeshQueryItem<P>, With<NeedsMesh>>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, prim, rotation, mesh_handle, material_handle) in &query {
        let rot = rotation.copied().unwrap_or_default();
        let mesh = prim.to_bevy_mesh(rot.0);
        let transform = prim.entity_transform(rot.0);
        let material = match P::MATERIAL_KIND {
            MeshMaterialKind::Primitive => primitive_material.0.clone(),
            MeshMaterialKind::Plane => plane_material.0.clone(),
        };
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            mesh,
            material,
            transform,
        );
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

fn draw_polylines(
    polylines: Query<PolylineDrawQueryItem>,
    mut gizmos: Gizmos,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut line_count = 0usize;
    for (polyline, visibility, elevation_metadata, is_muted) in &polylines {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        let base_color = elevation_metadata
            .map(|metadata| elevation_band_color(metadata.elevation))
            .unwrap_or(POLYLINE_COLOR);
        let line_color = if is_muted {
            base_color.with_alpha(MUTED_POLYLINE_ALPHA)
        } else {
            base_color
        };
        for segment in polyline.points.windows(2) {
            gizmos.line(segment[0], segment[1], line_color);
            #[cfg(feature = "perf-stats")]
            {
                line_count += 1;
            }
        }
    }
    #[cfg(feature = "perf-stats")]
    if line_count > 0 {
        add_gizmo_line_count(&mut perf_stats, line_count);
    }
}

fn elevation_band_color(elevation: f32) -> Color {
    let band_index = (elevation / ELEVATION_BAND_HEIGHT).floor() as i32;
    let palette_index = band_index.rem_euclid(ELEVATION_BAND_COLORS.len() as i32) as usize;
    let [r, g, b] = ELEVATION_BAND_COLORS[palette_index];
    Color::srgb(r, g, b)
}

fn spawn_triangle_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    query: TriangleMeshQuery,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, primitive, mesh_handle, material_handle) in &query {
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            triangle_mesh_asset(primitive),
            primitive_material.0.clone(),
            Transform::IDENTITY,
        );
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

type EditableMeshQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static EditableMesh,
        Option<&'static Mesh3d>,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
    ),
    With<NeedsMesh>,
>;

fn spawn_editable_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    query: EditableMeshQuery,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, editable, mesh_handle, material_handle) in &query {
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            editable_mesh_asset(editable),
            primitive_material.0.clone(),
            Transform::IDENTITY,
        );
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

fn spawn_evaluated_csg_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    query: Query<EvaluatedCsgQueryItem, With<NeedsMesh>>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, evaluated, mesh_handle, material_handle) in &query {
        if evaluated.vertices.is_empty() {
            commands.entity(entity).remove::<NeedsMesh>();
            continue;
        }
        let mesh = evaluated_csg_to_mesh(evaluated);
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            mesh,
            primitive_material.0.clone(),
            Transform::IDENTITY,
        );
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

fn spawn_evaluated_feature_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    query: Query<EvaluatedFeatureQueryItem, With<NeedsMesh>>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, evaluated, mesh_handle, material_handle) in &query {
        if evaluated.vertices.is_empty() {
            commands.entity(entity).remove::<NeedsMesh>();
            continue;
        }
        let mesh = evaluated_feature_to_mesh(evaluated);
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            mesh,
            primitive_material.0.clone(),
            Transform::IDENTITY,
        );
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

fn evaluated_csg_to_mesh(evaluated: &EvaluatedCsg) -> Mesh {
    let positions: Vec<[f32; 3]> = evaluated.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let normals: Vec<[f32; 3]> = evaluated.normals.iter().map(|n| [n.x, n.y, n.z]).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; positions.len()]);
    mesh.insert_indices(Indices::U32(evaluated.indices.clone()));
    mesh
}

fn evaluated_feature_to_mesh(evaluated: &EvaluatedFeature) -> Mesh {
    let positions: Vec<[f32; 3]> = evaluated.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let normals: Vec<[f32; 3]> = evaluated.normals.iter().map(|n| [n.x, n.y, n.z]).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; positions.len()]);
    mesh.insert_indices(Indices::U32(evaluated.indices.clone()));
    mesh
}

fn spawn_evaluated_mirror_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    query: Query<EvaluatedMirrorQueryItem, With<NeedsMesh>>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, evaluated, mesh_handle, material_handle) in &query {
        if evaluated.vertices.is_empty() {
            commands.entity(entity).remove::<NeedsMesh>();
            continue;
        }
        let mesh = evaluated_mirror_to_mesh(evaluated);
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            mesh,
            primitive_material.0.clone(),
            Transform::IDENTITY,
        );
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

fn evaluated_mirror_to_mesh(evaluated: &EvaluatedMirror) -> Mesh {
    let positions: Vec<[f32; 3]> = evaluated.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let normals: Vec<[f32; 3]> = evaluated.normals.iter().map(|n| [n.x, n.y, n.z]).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; positions.len()]);
    mesh.insert_indices(Indices::U32(evaluated.indices.clone()));
    mesh
}

fn spawn_evaluated_array_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    primitive_material: Res<PrimitiveMaterial>,
    query: Query<EvaluatedArrayQueryItem, With<NeedsMesh>>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    #[cfg(feature = "perf-stats")]
    let mut regenerated = 0usize;
    for (entity, evaluated, mesh_handle, material_handle) in &query {
        if evaluated.vertices.is_empty() {
            commands.entity(entity).remove::<NeedsMesh>();
            continue;
        }
        let mesh = evaluated_array_to_mesh(evaluated);
        upsert_mesh_entity(
            &mut commands,
            &mut meshes,
            (entity, mesh_handle, material_handle),
            mesh,
            primitive_material.0.clone(),
            Transform::IDENTITY,
        );
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

fn evaluated_array_to_mesh(evaluated: &EvaluatedArray) -> Mesh {
    let positions: Vec<[f32; 3]> = evaluated.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let normals: Vec<[f32; 3]> = evaluated.normals.iter().map(|n| [n.x, n.y, n.z]).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; positions.len()]);
    mesh.insert_indices(Indices::U32(evaluated.indices.clone()));
    mesh
}

fn editable_mesh_asset(editable: &EditableMesh) -> Mesh {
    let (positions, triangles, normals) = editable.triangulate_all();
    let pos_arr: Vec<[f32; 3]> = positions.iter().map(|v| [v.x, v.y, v.z]).collect();
    let norm_arr: Vec<[f32; 3]> = normals.iter().map(|n| [n.x, n.y, n.z]).collect();
    let indices: Vec<u32> = triangles.iter().flat_map(|t| t.iter().copied()).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos_arr.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, norm_arr);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; pos_arr.len()]);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn upsert_mesh_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    existing: (
        Entity,
        Option<&Mesh3d>,
        Option<&MeshMaterial3d<StandardMaterial>>,
    ),
    mesh: Mesh,
    material: Handle<StandardMaterial>,
    transform: Transform,
) {
    let (entity, mesh_handle, material_handle) = existing;
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
        entity_commands.insert(MeshMaterial3d(material));
    }

    entity_commands.remove::<NeedsMesh>().insert(transform);
}

fn triangle_mesh_asset(primitive: &TriangleMesh) -> Mesh {
    let positions: Vec<[f32; 3]> = primitive
        .vertices
        .iter()
        .map(|vertex| [vertex.x, vertex.y, vertex.z])
        .collect();
    let normals = primitive
        .normals
        .clone()
        .filter(|normals| normals.len() == primitive.vertices.len())
        .unwrap_or_else(|| compute_triangle_mesh_normals(primitive));
    let normals: Vec<[f32; 3]> = normals
        .into_iter()
        .map(|normal| [normal.x, normal.y, normal.z])
        .collect();
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
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0]; primitive.vertices.len()],
    );
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn compute_triangle_mesh_normals(primitive: &TriangleMesh) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; primitive.vertices.len()];

    for face in &primitive.faces {
        let [a, b, c] = *face;
        let (Some(a), Some(b), Some(c)) = (
            primitive.vertices.get(a as usize).copied(),
            primitive.vertices.get(b as usize).copied(),
            primitive.vertices.get(c as usize).copied(),
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
