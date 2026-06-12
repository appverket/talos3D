use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use delaunator::{triangulate, Point};
use talos3d_core::plugins::{
    identity::ElementId, layers::LayerRegistry, modeling::primitives::TriangleMesh,
};

/// Layer the draped terrain surface lives on (kept in sync with the snapshot
/// loader, which assigns the same layer at spawn).
pub const TERRAIN_LAYER_NAME: &str = "Terrain";

use crate::{
    components::{
        ElevationCurve, NeedsTerrainMesh, TerrainMeshCache, TerrainSurface, TerrainSurfaceRole,
    },
    fairing::fair_heights_thin_plate,
    heightfield::TerrainHeightfield,
    reconstruction::{
        sample_boundary_support_points, sample_curve_points, sample_interior_support_points,
    },
    visualization::{
        visualization_for_mode, CutFillVisualizationState, CutFillVisualizationTarget,
        TerrainVisualizationMode, TerrainVisualizationState, TriangleVisualization,
    },
};

const TERRAIN_SURFACE_COLOR: Color = Color::srgb(0.54, 0.62, 0.46);
const PROPOSED_TERRAIN_SURFACE_COLOR: Color = Color::srgb(0.36, 0.58, 0.86);
const ELEVATION_LOW_COLOR: [f32; 3] = [0.22, 0.48, 0.82];
const ELEVATION_MID_COLOR: [f32; 3] = [0.33, 0.72, 0.34];
const ELEVATION_HIGH_COLOR: [f32; 3] = [0.65, 0.43, 0.22];
const ELEVATION_PEAK_COLOR: [f32; 3] = [0.92, 0.92, 0.9];
const TERRAIN_CONTOUR_COLOR: Color = Color::srgba(0.08, 0.09, 0.07, 0.9);
const SHALLOW_CUT_COLOR: [f32; 4] = [0.95, 0.48, 0.14, 1.0];
const DEEP_CUT_COLOR: [f32; 4] = [0.82, 0.12, 0.10, 1.0];
const SHALLOW_FILL_COLOR: [f32; 4] = [0.20, 0.70, 0.32, 1.0];
const DEEP_FILL_COLOR: [f32; 4] = [0.12, 0.38, 0.86, 1.0];
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
    Option<&'a ElementId>,
    Option<&'a Mesh3d>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);

#[derive(Clone, Copy)]
enum CutFillVisualizationComparison<'a> {
    ProposedSurface(&'a TriangleMesh),
    Datum(f32),
}

#[derive(Clone, Copy)]
struct CutFillVisualizationInput<'a> {
    comparison: CutFillVisualizationComparison<'a>,
    boundary: &'a [Vec2],
}

/// Whether the terrain's *generated* iso-contour overlay is drawn. These lines
/// are sliced from the (gridded) surface mesh, so they staircase; they are also
/// redundant with the imported survey contour layers (e.g. `HOJDKURVA`). Off by
/// default — toggle via `terrain.toggle_generated_contours`.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ShowGeneratedContours(pub bool);

impl Plugin for TerrainGenerationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainVisualizationState>()
            .init_resource::<ShowGeneratedContours>()
            .add_systems(Startup, setup_terrain_material)
            .add_systems(
                Update,
                (
                    mark_terrain_surfaces_dirty_on_curve_changes,
                    mark_terrain_surfaces_dirty_on_surface_changes,
                    regenerate_terrain_meshes,
                    // Enforce the Terrain layer's visibility on the surface mesh
                    // every frame, after (re)generation, so the layer panel can
                    // reliably show/hide the draped surface.
                    apply_terrain_layer_visibility.after(regenerate_terrain_meshes),
                    draw_elevation_curves,
                    draw_terrain_contours,
                ),
            );
    }
}

/// Drive the draped surface's visibility from the `Terrain` layer. The terrain
/// mesh is (re)built on the surface entity by `regenerate_terrain_meshes`, which
/// can reset its `Visibility`; enforcing it here every frame keeps the layer
/// toggle authoritative for the surface, matching how authored primitives honor
/// their layer.
fn apply_terrain_layer_visibility(
    registry: Res<LayerRegistry>,
    mut surfaces: Query<&mut Visibility, With<TerrainSurface>>,
) {
    let target = if registry.is_visible(TERRAIN_LAYER_NAME) {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut surfaces {
        if *visibility != target {
            *visibility = target;
        }
    }
}

fn setup_terrain_material(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(TerrainSurfaceMaterial {
        existing: materials.add(terrain_surface_material(TERRAIN_SURFACE_COLOR)),
        proposed: materials.add(terrain_surface_material(PROPOSED_TERRAIN_SURFACE_COLOR)),
    });
}

/// Re-drape a surface when its own `TerrainSurface` component changes — e.g. an
/// agent edits `smoothing` or `drape_sample_spacing` via `set_entity_property`
/// (MCP) or the inspector. Without this, those edits would be stored but never
/// take visible effect (regen only watched curve changes). `Changed` also covers
/// the spawn frame, so newly created surfaces still build. `regenerate_terrain_meshes`
/// does not mutate `TerrainSurface`, so this does not self-retrigger.
fn mark_terrain_surfaces_dirty_on_surface_changes(
    surfaces: Query<(Entity, Option<&NeedsTerrainMesh>), Changed<TerrainSurface>>,
    mut commands: Commands,
) {
    for (entity, needs_mesh) in &surfaces {
        if needs_mesh.is_none() {
            commands.entity(entity).insert(NeedsTerrainMesh);
        }
    }
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

#[allow(clippy::too_many_arguments)]
fn regenerate_terrain_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    terrain_material: Res<TerrainSurfaceMaterial>,
    visualization_state: Res<TerrainVisualizationState>,
    cut_fill_visualization: Option<Res<CutFillVisualizationState>>,
    surfaces: Query<TerrainSurfaceMeshQueryItem<'_>, With<NeedsTerrainMesh>>,
    surface_caches: Query<(&ElementId, &TerrainMeshCache)>,
    curves: Query<(&ElementId, &ElevationCurve)>,
) {
    for (entity, surface, element_id, mesh_handle, material_handle) in &surfaces {
        let cache = generate_terrain_mesh_cache(surface, &curves);
        // Refresh the O(1) height-field query layer alongside the render mesh
        // (ADR-059, PP-PLANT-A) so conforming placement always reads current grade.
        match generate_terrain_heightfield(surface, &curves) {
            Some(heightfield) => {
                commands.entity(entity).try_insert(heightfield);
            }
            None => {
                commands.entity(entity).try_remove::<TerrainHeightfield>();
            }
        }
        let cut_fill_input = cut_fill_visualization_input(
            cut_fill_visualization.as_deref(),
            element_id,
            &surface_caches,
        );
        upsert_terrain_mesh_entity(
            &mut commands,
            &mut meshes,
            entity,
            mesh_handle,
            material_handle,
            &cache.mesh,
            terrain_material.for_surface(surface),
            *visualization_state,
            cut_fill_input,
        );
        commands
            .entity(entity)
            .try_insert(cache)
            .try_remove::<NeedsTerrainMesh>();
    }
}

fn cut_fill_visualization_input<'a>(
    state: Option<&'a CutFillVisualizationState>,
    element_id: Option<&ElementId>,
    surface_caches: &'a Query<(&ElementId, &TerrainMeshCache)>,
) -> Option<CutFillVisualizationInput<'a>> {
    let state = state?;
    if element_id.copied() != Some(state.existing_surface_id) {
        return None;
    }
    let comparison = match state.target {
        CutFillVisualizationTarget::ProposedSurface(proposed_id) => {
            let (_, cache) = surface_caches
                .iter()
                .find(|(element_id, _)| **element_id == proposed_id)?;
            CutFillVisualizationComparison::ProposedSurface(&cache.mesh)
        }
        CutFillVisualizationTarget::Datum(datum_y) => {
            CutFillVisualizationComparison::Datum(datum_y)
        }
    };
    Some(CutFillVisualizationInput {
        comparison,
        boundary: &state.boundary,
    })
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
    show: Res<ShowGeneratedContours>,
    surfaces: Query<(&TerrainMeshCache, Option<&Visibility>)>,
    mut gizmos: Gizmos,
) {
    if !show.0 {
        return;
    }
    for (cache, visibility) in &surfaces {
        if visibility.is_some_and(|visibility| *visibility == Visibility::Hidden) {
            continue;
        }
        for [start, end] in &cache.contour_segments {
            gizmos.line(*start, *end, TERRAIN_CONTOUR_COLOR);
        }
    }
}

/// Build the [`TerrainHeightfield`] query layer for a surface (ADR-059,
/// PP-PLANT-A). Samples the same source contour points the mesh uses and grids
/// the IDW field over the surface bounds (clipped to the boundary mask when one
/// is authored). Returns `None` for surfaces with too few resolved points.
pub fn generate_terrain_heightfield(
    surface: &TerrainSurface,
    curves: &Query<(&talos3d_core::plugins::identity::ElementId, &ElevationCurve)>,
) -> Option<TerrainHeightfield> {
    let effective_spacing = adaptive_sampling_spacing(surface);
    let mut source_points = Vec::new();
    for source_id in &surface.source_curve_ids {
        let Some((_, curve)) = curves.iter().find(|(element_id, _)| *element_id == source_id) else {
            continue;
        };
        source_points.extend(
            sample_curve_points(&curve.points, effective_spacing)
                .into_iter()
                .map(|point| point + surface.offset),
        );
    }
    TerrainHeightfield::build(
        &source_points,
        &surface.boundary,
        effective_spacing,
        surface.smoothing,
    )
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

    let mut vertices = dedupe_vertices(source_points);
    if vertices.len() < 3 {
        return TerrainMeshCache::default();
    }

    let breakline_segments = build_breakline_segments(&vertices, &sampled_curves);
    let faces = build_terrain_faces(surface, &vertices, &breakline_segments);
    smooth_terrain_heights(&mut vertices, &faces, &sampled_curves, surface.smoothing);
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

    // Index breaklines spatially so the per-triangle crossing test only checks
    // segments near the triangle, instead of all of them (O(faces*breaklines)).
    let breakline_grid = BreaklineGrid::build(vertices, breakline_segments);

    // Only contour-to-contour edges can be a "breakline crossing": those are the
    // real artefact (a triangle bridging across a concave contour gap). An edge
    // that touches a draped support point is on-surface fill and must never be
    // deleted — otherwise the interior support grid that boundary clipping adds
    // punches a pinhole everywhere a grid triangle straddles a contour. The
    // contour vertices are exactly the endpoints that appear in the breakline
    // (sampled-contour) segments.
    let contour_vertices: std::collections::HashSet<usize> = breakline_segments
        .iter()
        .flat_map(|&(start, end)| [start, end])
        .collect();

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
        if breakline_grid.triangle_crosses(vertices, face, breakline_segments, &contour_vertices) {
            continue;
        }
        faces.push(face);
    }
    faces
}

/// Uniform-grid spatial index of breakline segments keyed by XZ cell. A breakline
/// is registered in every cell its XZ bounding box touches; a triangle queries
/// only the cells its own bounding box touches. Because any proper intersection
/// lies inside both bounding boxes, this yields exactly the same answer as
/// scanning every breakline (just far fewer tests).
struct BreaklineGrid {
    cell: f32,
    cells: std::collections::HashMap<(i32, i32), Vec<usize>>,
}

impl BreaklineGrid {
    const CELL: f32 = 4.0;

    fn build(vertices: &[Vec3], segments: &[BreaklineSegment]) -> Self {
        let cell = Self::CELL;
        let mut cells: std::collections::HashMap<(i32, i32), Vec<usize>> =
            std::collections::HashMap::new();
        for (index, &(start, end)) in segments.iter().enumerate() {
            let (Some(a), Some(b)) = (vertices.get(start), vertices.get(end)) else {
                continue;
            };
            let (cx0, cx1, cz0, cz1) = Self::cell_range(a.x.min(b.x), a.x.max(b.x), a.z.min(b.z), a.z.max(b.z), cell);
            for cx in cx0..=cx1 {
                for cz in cz0..=cz1 {
                    cells.entry((cx, cz)).or_default().push(index);
                }
            }
        }
        Self { cell, cells }
    }

    fn cell_range(minx: f32, maxx: f32, minz: f32, maxz: f32, cell: f32) -> (i32, i32, i32, i32) {
        (
            (minx / cell).floor() as i32,
            (maxx / cell).floor() as i32,
            (minz / cell).floor() as i32,
            (maxz / cell).floor() as i32,
        )
    }

    fn triangle_crosses(
        &self,
        vertices: &[Vec3],
        face: [u32; 3],
        segments: &[BreaklineSegment],
        contour_vertices: &std::collections::HashSet<usize>,
    ) -> bool {
        let Some((a, b, c)) = face_points(vertices, face) else {
            return false;
        };
        let minx = a.x.min(b.x).min(c.x);
        let maxx = a.x.max(b.x).max(c.x);
        let minz = a.z.min(b.z).min(c.z);
        let maxz = a.z.max(b.z).max(c.z);
        let (cx0, cx1, cz0, cz1) = Self::cell_range(minx, maxx, minz, maxz, self.cell);
        let edges = [
            (face[0] as usize, face[1] as usize),
            (face[1] as usize, face[2] as usize),
            (face[2] as usize, face[0] as usize),
        ];
        for cx in cx0..=cx1 {
            for cz in cz0..=cz1 {
                let Some(bucket) = self.cells.get(&(cx, cz)) else {
                    continue;
                };
                for &index in bucket {
                    let breakline = segments[index];
                    for edge in &edges {
                        if !contour_vertices.contains(&edge.0)
                            || !contour_vertices.contains(&edge.1)
                        {
                            continue;
                        }
                        if !shares_endpoint(*edge, breakline)
                            && segments_properly_intersect_xz(
                                vertices[edge.0],
                                vertices[edge.1],
                                vertices[breakline.0],
                                vertices[breakline.1],
                            )
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
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

#[allow(clippy::too_many_arguments)]
fn upsert_terrain_mesh_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    entity: Entity,
    mesh_handle: Option<&Mesh3d>,
    material_handle: Option<&MeshMaterial3d<StandardMaterial>>,
    primitive: &TriangleMesh,
    material: Handle<StandardMaterial>,
    visualization_state: TerrainVisualizationState,
    cut_fill_visualization: Option<CutFillVisualizationInput<'_>>,
) {
    let mut entity_commands = commands.entity(entity);
    let mesh = terrain_mesh_asset(primitive, visualization_state, cut_fill_visualization);
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
    cut_fill_visualization: Option<CutFillVisualizationInput<'_>>,
) -> Mesh {
    if visualization_state.mode != TerrainVisualizationMode::Standard {
        if let Some(mesh) =
            visualized_terrain_mesh_asset(primitive, visualization_state, cut_fill_visualization)
        {
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
    cut_fill_visualization: Option<CutFillVisualizationInput<'_>>,
) -> Option<Mesh> {
    let visualizations = if visualization_state.mode == TerrainVisualizationMode::CutFill {
        cut_fill_visualization
            .map(|input| cut_fill_visualization_for_mesh(primitive, input))
            .unwrap_or_default()
    } else {
        visualization_for_mode(primitive, visualization_state)
    };
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

fn cut_fill_visualization_for_mesh(
    existing: &TriangleMesh,
    input: CutFillVisualizationInput<'_>,
) -> Vec<TriangleVisualization> {
    let mut deltas = Vec::new();
    for face in &existing.faces {
        let Some((a, b, c)) = face_points(&existing.vertices, *face) else {
            continue;
        };
        let centroid = Vec2::new((a.x + b.x + c.x) / 3.0, (a.z + b.z + c.z) / 3.0);
        if input.boundary.len() >= 3 && !point_in_polygon(centroid, input.boundary) {
            continue;
        }
        let existing_y = (a.y + b.y + c.y) / 3.0;
        let Some(proposed_y) = comparison_elevation(input.comparison, centroid.x, centroid.y)
        else {
            continue;
        };
        let delta = proposed_y - existing_y;
        if delta.abs() <= CONTOUR_EPSILON {
            continue;
        }
        deltas.push((*face, delta));
    }

    let max_delta = deltas
        .iter()
        .map(|(_, delta)| delta.abs())
        .fold(0.0_f32, f32::max)
        .max(CONTOUR_EPSILON);
    deltas
        .into_iter()
        .map(|(face, delta)| TriangleVisualization {
            face,
            value: delta,
            color: cut_fill_delta_color(delta, max_delta),
        })
        .collect()
}

fn comparison_elevation(
    comparison: CutFillVisualizationComparison<'_>,
    x: f32,
    z: f32,
) -> Option<f32> {
    match comparison {
        CutFillVisualizationComparison::ProposedSurface(mesh) => {
            sample_surface_elevation(mesh, x, z)
        }
        CutFillVisualizationComparison::Datum(datum_y) => Some(datum_y),
    }
}

fn cut_fill_delta_color(delta: f32, max_delta: f32) -> [f32; 4] {
    let deep = delta.abs() >= max_delta * 0.5;
    match (delta < 0.0, deep) {
        (true, false) => SHALLOW_CUT_COLOR,
        (true, true) => DEEP_CUT_COLOR,
        (false, false) => SHALLOW_FILL_COLOR,
        (false, true) => DEEP_FILL_COLOR,
    }
}

/// Constrained thin-plate fairing of terrain vertex heights. Relaxes the sharp
/// inter-contour terraces/creases that raw IDW reconstruction produces while
/// surveyed contour vertices stay exact: the curvature-minimising surface
/// passes C1-smoothly through the pinned curves, instead of staying "tense"
/// over them the way a membrane (Laplacian-with-attachment) relaxation does.
/// Operates on the already-triangulated mesh, so it never reopens holes (those
/// came from the sparse contour TIN, not the dense surface). One-time at mesh
/// (re)generation.
fn smooth_terrain_heights(
    vertices: &mut [Vec3],
    faces: &[[u32; 3]],
    sampled_curves: &[Vec<Vec3>],
    smoothing: f32,
) {
    if smoothing <= 0.0 || vertices.len() < 3 || faces.is_empty() {
        return;
    }

    let pinned = contour_pinned_mask(vertices, sampled_curves);

    let mut neighbors: Vec<Vec<u32>> = vec![Vec::new(); vertices.len()];
    for face in faces {
        let [a, b, c] = *face;
        neighbors[a as usize].extend_from_slice(&[b, c]);
        neighbors[b as usize].extend_from_slice(&[a, c]);
        neighbors[c as usize].extend_from_slice(&[a, b]);
    }
    // An undirected edge is visited once per incident face; dedupe so shared
    // edges don't double-weight the neighbor mean.
    for adj in &mut neighbors {
        adj.sort_unstable();
        adj.dedup();
    }

    let mut heights: Vec<f32> = vertices.iter().map(|v| v.y).collect();
    fair_heights_thin_plate(&mut heights, &pinned, smoothing, |index| {
        neighbors[index].iter().map(|&j| j as usize)
    });
    for (vertex, height) in vertices.iter_mut().zip(heights) {
        vertex.y = height;
    }
}

/// Mark vertices coincident (within `CONTOUR_EPSILON`) with a surveyed contour
/// sample point, via an O(1) spatial hash over the contour samples.
fn contour_pinned_mask(vertices: &[Vec3], sampled_curves: &[Vec<Vec3>]) -> Vec<bool> {
    let cell = CONTOUR_EPSILON.sqrt().max(f32::MIN_POSITIVE);
    let key = |p: Vec2| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64);
    let mut grid: std::collections::HashMap<(i64, i64), Vec<Vec2>> =
        std::collections::HashMap::new();
    for curve in sampled_curves {
        for point in curve {
            let xz = Vec2::new(point.x, point.z);
            grid.entry(key(xz)).or_default().push(xz);
        }
    }
    vertices
        .iter()
        .map(|vertex| {
            let xz = Vec2::new(vertex.x, vertex.z);
            let (kx, ky) = key(xz);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(bucket) = grid.get(&(kx + dx, ky + dy)) {
                        if bucket
                            .iter()
                            .any(|c| c.distance_squared(xz) <= CONTOUR_EPSILON)
                        {
                            return true;
                        }
                    }
                }
            }
            false
        })
        .collect()
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
    // Keep the first occurrence of each point (within CONTOUR_EPSILON), but use a
    // 3D spatial hash of kept points so the duplicate test is O(1) instead of a
    // scan over all kept vertices (O(n^2) overall). Cell size == the distance
    // threshold, so any earlier point within range lies in one of the 27
    // neighbouring cells. Output (and order) is identical to the naive scan.
    let cell = CONTOUR_EPSILON.sqrt().max(f32::MIN_POSITIVE);
    let key = |p: Vec3| {
        (
            (p.x / cell).floor() as i64,
            (p.y / cell).floor() as i64,
            (p.z / cell).floor() as i64,
        )
    };
    let mut grid: std::collections::HashMap<(i64, i64, i64), Vec<Vec3>> =
        std::collections::HashMap::new();
    let mut unique = Vec::<Vec3>::new();
    for point in points {
        let (kx, ky, kz) = key(point);
        let mut duplicate = false;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(bucket) = grid.get(&(kx + dx, ky + dy, kz + dz)) {
                        if bucket
                            .iter()
                            .any(|existing| existing.distance_squared(point) <= CONTOUR_EPSILON)
                        {
                            duplicate = true;
                            break 'search;
                        }
                    }
                }
            }
        }
        if duplicate {
            continue;
        }
        grid.entry((kx, ky, kz)).or_default().push(point);
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
        // Standard even-odd ray cast. The denominator must keep its sign — the
        // earlier `.abs()` mirrored the x-intercept for downward edges, which
        // misclassified interior points and punched holes in clipped surfaces.
        // It is only evaluated when the edge straddles `point.y` (so it is
        // never zero thanks to the short-circuiting `&&`).
        let crosses = ((current.y > point.y) != (previous.y > point.y))
            && (point.x
                < (previous.x - current.x) * (point.y - current.y)
                    / (previous.y - current.y)
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

/// Reference (linear-scan) breakline crossing test. Production code uses
/// [`BreaklineGrid::triangle_crosses`]; this remains as the test oracle the
/// grid is validated against.
#[cfg(test)]
fn triangle_crosses_breakline(
    vertices: &[Vec3],
    face: [u32; 3],
    breakline_segments: &[BreaklineSegment],
    contour_vertices: &std::collections::HashSet<usize>,
) -> bool {
    let edges = [
        (face[0] as usize, face[1] as usize),
        (face[1] as usize, face[2] as usize),
        (face[2] as usize, face[0] as usize),
    ];
    edges.iter().any(|edge| {
        // Only contour-to-contour edges can be deleted; edges touching a draped
        // support vertex are on-surface fill, not a crossing artefact.
        contour_vertices.contains(&edge.0)
            && contour_vertices.contains(&edge.1)
            && breakline_segments.iter().any(|breakline| {
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
    fn terrain_faces_wind_upward_for_lighting() {
        // Mirror the importer's X-negated layout (East -> -X). Delaunator outputs a
        // fixed winding; a reflection in X flips the resulting 3D normal sign, so the
        // generated faces must be normalized to face +Y or directional lighting (the
        // daylight rig shines from above) produces N.L <= 0 and the surface renders
        // black. This guards that regression.
        let mut vertices = Vec::new();
        for ix in 0..4 {
            for iz in 0..4 {
                vertices.push(Vec3::new(-(ix as f32) * 10.0, 0.5, iz as f32 * 10.0));
            }
        }
        let surface = TerrainSurface {
            name: "wind-test".to_string(),
            source_curve_ids: vec![],
            role: Default::default(),
            datum_elevation: 0.0,
            boundary: vec![],
            max_triangle_area: 1.0e9,
            minimum_angle: 0.0,
            contour_interval: 0.5,
            drape_sample_spacing: 1.0,
            smoothing: 0.0,
            offset: Vec3::ZERO,
        };
        let faces = build_terrain_faces(&surface, &vertices, &[]);
        assert!(!faces.is_empty(), "expected a triangulated surface");
        for face in &faces {
            let (a, b, c) = face_points(&vertices, *face).expect("valid face");
            let normal = (b - a).cross(c - a);
            assert!(
                normal.y > 0.0,
                "terrain face normal must point up (+Y) for lighting, got {normal:?}",
            );
        }
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

        let standard = terrain_mesh_asset(&terrain, TerrainVisualizationState::default(), None);
        assert!(standard.attribute(Mesh::ATTRIBUTE_COLOR).is_none());

        let visualized = terrain_mesh_asset(
            &terrain,
            TerrainVisualizationState {
                mode: TerrainVisualizationMode::Slope,
                elevation_band_width: 1.0,
            },
            None,
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
    fn cut_fill_visualization_colors_cut_and_fill_faces() {
        let existing = TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 1.0),
                Vec3::new(2.0, 1.0, 0.0),
                Vec3::new(3.0, 1.0, 0.0),
                Vec3::new(2.0, 1.0, 1.0),
            ],
            faces: vec![[0, 1, 2], [3, 4, 5]],
            normals: None,
            name: None,
        };
        let proposed = TriangleMesh {
            vertices: vec![
                Vec3::new(0.0, 0.5, 0.0),
                Vec3::new(1.0, 0.5, 0.0),
                Vec3::new(0.0, 0.5, 1.0),
                Vec3::new(2.0, 1.5, 0.0),
                Vec3::new(3.0, 1.5, 0.0),
                Vec3::new(2.0, 1.5, 1.0),
            ],
            faces: vec![[0, 1, 2], [3, 4, 5]],
            normals: None,
            name: None,
        };

        let map = cut_fill_visualization_for_mesh(
            &existing,
            CutFillVisualizationInput {
                comparison: CutFillVisualizationComparison::ProposedSurface(&proposed),
                boundary: &[],
            },
        );

        assert_eq!(map.len(), 2);
        assert!(map[0].value < 0.0);
        assert!(map[1].value > 0.0);
        assert_eq!(map[0].color, DEEP_CUT_COLOR);
        assert_eq!(map[1].color, DEEP_FILL_COLOR);
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
            smoothing: 0.0,
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
        // All corner vertices are contour-sourced here.
        let contour: std::collections::HashSet<usize> = (0..vertices.len()).collect();

        // A contour-to-contour edge (1->3) crossing the source segment is removed.
        assert!(triangle_crosses_breakline(
            &vertices,
            [0, 1, 3],
            &breaklines,
            &contour
        ));
        // An edge that only touches the segment endpoints does not "cross".
        assert!(!triangle_crosses_breakline(
            &vertices,
            [0, 4, 3],
            &breaklines,
            &contour
        ));
    }

    #[test]
    fn breakline_filter_keeps_draped_support_point_triangles() {
        // Same geometry, but vertices 0 and 3 are draped *support* points (not on
        // any contour). The triangle still crosses the segment geometrically, but
        // it must be kept — deleting it is what punches pinholes in the support
        // grid. Only contour vertices {1, 2, 4, 5} are contour-sourced here.
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, 1.0),
        ];
        let breaklines = vec![(4, 5)];
        let contour: std::collections::HashSet<usize> = [1usize, 2, 4, 5].into_iter().collect();

        // Edge 1->3 crosses the segment, but vertex 3 is a support point, so the
        // triangle is fill and must not be removed.
        assert!(!triangle_crosses_breakline(
            &vertices,
            [0, 1, 3],
            &breaklines,
            &contour
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
            smoothing: 0.0,
            offset: Vec3::ZERO,
        };
        // Three stacked contour rows (z = 0, 1, 2); every vertex is contour-sourced.
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, 1.0),
        ];
        let breaklines = vec![(0, 1), (4, 5), (3, 2)];
        let contour: std::collections::HashSet<usize> =
            breaklines.iter().flat_map(|&(a, b)| [a, b]).collect();

        let faces = build_terrain_faces(&surface, &vertices, &breaklines);

        assert!(!faces.is_empty());
        assert!(faces.iter().all(|face| !triangle_crosses_breakline(
            &vertices,
            *face,
            &breaklines,
            &contour
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
            smoothing: 0.0,
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
