//! Derived cut-face geometry. Intersections run only for invalidated source
//! meshes, with a fixed triangle budget; steady frames reuse one GPU mesh.
use std::collections::{HashMap, HashSet, VecDeque};

use bevy::{
    asset::RenderAssetUsages,
    math::DVec2,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    transform::TransformSystems,
};

use super::{
    clipping_planes::ClipPlaneNode,
    definition_preview_scene::PreviewOnly,
    identity::ElementId,
    modeling::{csg::CsgOperand, occurrence::GeneratedOccurrencePart},
    section_fill::intersect_plane_mesh,
};

// Oversized individual sources are explicitly omitted, rather than letting a
// single mesh bypass the live budget. They remain GPU-clipped and can be
// sectioned by the unrestricted offline DrawingScene/export path.
const TRIANGLES_PER_UPDATE: usize = 32_768;

#[derive(Component)]
struct SectionCapMesh;

#[derive(Clone)]
struct Source {
    mesh: AssetId<Mesh>,
    transform: GlobalTransform,
    visible: bool,
    material: Option<AssetId<StandardMaterial>>,
}

#[derive(Default, Resource)]
struct CapCache {
    planes: Vec<(Entity, Vec3, Vec3)>,
    sources: HashMap<Entity, Source>,
    pending: VecDeque<Entity>,
    queued: HashSet<Entity>,
    parts: HashMap<Entity, Vec<CapVertex>>,
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
    material: Option<Handle<StandardMaterial>>,
    uploads: u64,
    oversized_sources: usize,
}

impl CapCache {
    fn queue(&mut self, entity: Entity) {
        if self.queued.insert(entity) {
            self.pending.push_back(entity);
        }
    }
}

#[derive(Clone, Copy)]
struct CapVertex {
    point: Vec3,
    normal: Vec3,
    color: [f32; 4],
}

pub struct SectionCapsPlugin;
impl Plugin for SectionCapsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CapCache>()
            .add_message::<AssetEvent<Mesh>>()
            .add_message::<AssetEvent<StandardMaterial>>()
            .add_systems(
                PostUpdate,
                update_caps
                    .after(TransformSystems::Propagate)
                    .after(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
            );
    }
}

type SourceFilter = (
    Or<(With<ElementId>, With<GeneratedOccurrencePart>)>,
    Without<PreviewOnly>,
    Without<CsgOperand>,
    Without<SectionCapMesh>,
);

type SourceData<'a> = (
    Entity,
    &'a Mesh3d,
    &'a GlobalTransform,
    Option<&'a Visibility>,
    Option<&'a InheritedVisibility>,
    Option<&'a MeshMaterial3d<StandardMaterial>>,
);

#[allow(clippy::too_many_arguments)]
fn update_caps(
    mut commands: Commands,
    planes: Query<(Entity, &ClipPlaneNode)>,
    changed: Query<
        SourceData,
        (
            SourceFilter,
            Or<(
                Changed<Mesh3d>,
                Changed<GlobalTransform>,
                Changed<Visibility>,
                Changed<InheritedVisibility>,
                Changed<MeshMaterial3d<StandardMaterial>>,
                Added<ElementId>,
                Added<GeneratedOccurrencePart>,
            )>,
        ),
    >,
    eligible_sources: Query<SourceData, SourceFilter>,
    exclusion_changes: Query<Entity, Or<(Added<PreviewOnly>, Added<CsgOperand>)>>,
    mut removed: (
        RemovedComponents<Mesh3d>,
        RemovedComponents<ElementId>,
        RemovedComponents<GeneratedOccurrencePart>,
        RemovedComponents<PreviewOnly>,
        RemovedComponents<CsgOperand>,
        RemovedComponents<MeshMaterial3d<StandardMaterial>>,
        RemovedComponents<Visibility>,
        RemovedComponents<InheritedVisibility>,
    ),
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut material_events: MessageReader<AssetEvent<StandardMaterial>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    mut cache: ResMut<CapCache>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    // Plane lists are tiny authored controls, not a model/mesh walk.
    let mut active: Vec<_> = planes
        .iter()
        .filter_map(|(id, p)| {
            let normal = p.normal.normalize_or_zero();
            (p.active && normal != Vec3::ZERO).then_some((id, p.origin, normal))
        })
        .collect();
    active.sort_by_key(|(id, _, _)| id.to_bits());
    let mut upload = false;
    if active != cache.planes {
        cache.planes = active;
        cache.parts.clear();
        cache.pending.clear();
        cache.queued.clear();
        cache.oversized_sources = 0;
        let sources: Vec<_> = cache.sources.keys().copied().collect();
        for entity in sources {
            cache.queue(entity);
        }
        upload = true;
    }
    for entity in removed.0.read() {
        if cache.entity == Some(entity) {
            cache.entity = None;
            cache.mesh = None;
            cache.material = None;
            upload = true;
        }
        cache.sources.remove(&entity);
        upload |= cache.parts.remove(&entity).is_some();
        cache.queued.remove(&entity);
    }
    for entity in removed.5.read() {
        if let Some(source) = cache.sources.get_mut(&entity) {
            source.material = None;
            cache.queue(entity);
        }
    }
    let lifecycle: HashSet<_> = removed
        .1
        .read()
        .chain(removed.2.read())
        .chain(removed.3.read())
        .chain(removed.4.read())
        .chain(removed.6.read())
        .chain(removed.7.read())
        .chain(exclusion_changes.iter())
        .collect();
    for entity in lifecycle {
        if let Ok((_, mesh, transform, visibility, inherited, material)) =
            eligible_sources.get(entity)
        {
            cache.sources.insert(
                entity,
                Source {
                    mesh: mesh.id(),
                    transform: *transform,
                    visible: visibility.is_none_or(|v| *v != Visibility::Hidden)
                        && inherited.is_none_or(|v| v.get()),
                    material: material.map(|m| m.id()),
                },
            );
            cache.queue(entity);
        } else {
            cache.sources.remove(&entity);
            cache.queued.remove(&entity);
            upload |= cache.parts.remove(&entity).is_some();
        }
    }
    for (entity, mesh, transform, visibility, inherited, material) in &changed {
        let source = Source {
            mesh: mesh.id(),
            transform: *transform,
            visible: visibility.is_none_or(|v| *v != Visibility::Hidden)
                && inherited.is_none_or(|v| v.get()),
            material: material.map(|m| m.id()),
        };
        // Bevy also marks Mesh3d changed when only render preparation changes.
        // Compare actual inputs so derived material updates do not loop.
        let differs = cache.sources.get(&entity).is_none_or(|old| {
            old.mesh != source.mesh
                || old.transform != source.transform
                || old.visible != source.visible
                || old.material != source.material
        });
        if differs {
            cache.sources.insert(entity, source);
            cache.queue(entity);
            upload |= cache.parts.remove(&entity).is_some();
        }
    }
    let cap_mesh = cache.mesh.as_ref().map(Handle::id);
    let changed_meshes: HashSet<_> = mesh_events
        .read()
        .filter_map(|event| match event {
            AssetEvent::Modified { id } | AssetEvent::Added { id } | AssetEvent::Removed { id }
                if Some(*id) != cap_mesh =>
            {
                Some(*id)
            }
            _ => None,
        })
        .collect();
    let cap_material = cache.material.as_ref().map(Handle::id);
    let changed_materials: HashSet<_> = material_events
        .read()
        .filter_map(|event| match event {
            AssetEvent::Modified { id } | AssetEvent::Added { id } | AssetEvent::Removed { id }
                if Some(*id) != cap_material =>
            {
                Some(*id)
            }
            _ => None,
        })
        .collect();
    if !changed_meshes.is_empty() || !changed_materials.is_empty() {
        let invalid: Vec<_> = cache
            .sources
            .iter()
            .filter_map(|(entity, source)| {
                (changed_meshes.contains(&source.mesh)
                    || source
                        .material
                        .is_some_and(|id| changed_materials.contains(&id)))
                .then_some(*entity)
            })
            .collect();
        for entity in invalid {
            cache.queue(entity);
        }
    }
    if cache.planes.is_empty() {
        cache.pending.clear();
        cache.queued.clear();
    }
    let mut remaining = TRIANGLES_PER_UPDATE;
    while let Some(entity) = cache.pending.front().copied() {
        let source = cache.sources.get(&entity).cloned();
        let mesh = source.as_ref().and_then(|source| meshes.get(source.mesh));
        let triangles = mesh
            .map(triangle_count)
            .unwrap_or(0)
            .saturating_mul(cache.planes.len());
        if triangles <= TRIANGLES_PER_UPDATE && triangles > remaining {
            break;
        }
        cache.pending.pop_front();
        cache.queued.remove(&entity);
        upload |= cache.parts.remove(&entity).is_some();
        let (Some(source), Some(mesh)) = (source, mesh) else {
            continue;
        };
        if triangles > TRIANGLES_PER_UPDATE {
            cache.oversized_sources += 1;
            warn!(?entity, triangles, "Section cap source exceeds live triangle budget; GPU clipping remains active; use offline section export for this mesh");
            continue;
        }
        remaining -= triangles;
        if !source.visible {
            continue;
        }
        let material = source.material.and_then(|id| materials.get(id));
        // Transparent glass is a surface, not opaque poche.
        if material.is_some_and(|m| m.base_color.alpha() < 0.95) {
            continue;
        }
        let mut color = material
            .map(|m| m.base_color.to_linear().to_f32_array())
            .unwrap_or([0.75, 0.75, 0.75, 1.0]);
        // Match the canonical section-fill convention (40% base RGB), while
        // keeping the physical cut face opaque rather than a paper hatch wash.
        for channel in &mut color[..3] {
            *channel *= 0.4;
        }
        color[3] = 1.0;
        let mut vertices = Vec::new();
        for (_, origin, normal) in &cache.planes {
            let loops = intersect_plane_mesh(*origin, *normal, mesh, &source.transform);
            vertices.extend(triangulate_cap(&loops, *origin, *normal, color));
        }
        if !vertices.is_empty() {
            cache.parts.insert(entity, vertices);
            upload = true;
        }
    }
    if !upload {
        return;
    }
    let vertices: Vec<_> = cache.parts.values().flatten().copied().collect();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vertices
            .iter()
            .map(|v| v.point.to_array())
            .collect::<Vec<_>>(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vertices
            .iter()
            .map(|v| v.normal.to_array())
            .collect::<Vec<_>>(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_COLOR,
        vertices.iter().map(|v| v.color).collect::<Vec<_>>(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; vertices.len()]);
    mesh.insert_indices(Indices::U32((0..vertices.len() as u32).collect()));
    if let Some(handle) = &cache.mesh {
        if let Some(mut existing) = meshes.get_mut(handle) {
            *existing = mesh;
        }
    } else {
        let handle = meshes.add(mesh);
        let material = materials.add(StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            cull_mode: None,
            ..default()
        });
        let entity = commands
            .spawn((
                SectionCapMesh,
                PreviewOnly,
                Mesh3d(handle.clone()),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                Visibility::Visible,
            ))
            .id();
        cache.mesh = Some(handle);
        cache.material = Some(material);
        cache.entity = Some(entity);
    }
    cache.uploads += 1;
}

fn triangle_count(mesh: &Mesh) -> usize {
    mesh.indices()
        .map(|i| i.len())
        .unwrap_or(mesh.count_vertices())
        / 3
}

/// Even-odd slab tessellation handles nested holes and disconnected polygons.
/// Every slab boundary is an input vertex ordinate, so edge ordering cannot
/// change inside a slab for closed non-self-intersecting mesh sections.
fn triangulate_cap(
    loops: &[Vec<Vec3>],
    origin: Vec3,
    normal: Vec3,
    color: [f32; 4],
) -> Vec<CapVertex> {
    let normal = normal.normalize_or_zero();
    if normal == Vec3::ZERO {
        return Vec::new();
    }
    let u = normal.any_orthonormal_vector();
    let v = normal.cross(u);
    let mut edges = Vec::new();
    let mut levels = Vec::new();
    for polygon in loops {
        if polygon.len() < 4 || polygon[0].distance_squared(*polygon.last().unwrap()) > 1e-6 {
            continue;
        }
        let points: Vec<_> = polygon
            .iter()
            .map(|point| {
                let offset = (*point - origin).as_dvec3();
                DVec2::new(offset.dot(u.as_dvec3()), offset.dot(v.as_dvec3()))
            })
            .collect();
        for pair in points.windows(2) {
            levels.push(pair[0].y);
            if (pair[1].y - pair[0].y).abs() > 1e-10 {
                edges.push((pair[0], pair[1]));
            }
        }
    }
    levels.sort_by(f64::total_cmp);
    levels.dedup_by(|a, b| (*a - *b).abs() < 1e-8);
    let mut result = Vec::new();
    for slab in levels.windows(2) {
        let (lo, hi) = (slab[0], slab[1]);
        let mid = (lo + hi) * 0.5;
        let x_at = |edge: &(DVec2, DVec2), y: f64| {
            edge.0.x + (edge.1.x - edge.0.x) * (y - edge.0.y) / (edge.1.y - edge.0.y)
        };
        let mut active: Vec<_> = edges
            .iter()
            .filter(|(a, b)| mid > a.y.min(b.y) && mid < a.y.max(b.y))
            .collect();
        active.sort_by(|a, b| x_at(a, mid).total_cmp(&x_at(b, mid)));
        if active.len() % 2 != 0 {
            continue;
        }
        for pair in active.chunks_exact(2) {
            let a = DVec2::new(x_at(pair[0], lo), lo);
            let b = DVec2::new(x_at(pair[1], lo), lo);
            let c = DVec2::new(x_at(pair[1], hi), hi);
            let d = DVec2::new(x_at(pair[0], hi), hi);
            for triangle in [[a, b, c], [a, c, d]] {
                if (triangle[1] - triangle[0])
                    .perp_dot(triangle[2] - triangle[0])
                    .abs()
                    < 1e-12
                {
                    continue;
                }
                for point in triangle {
                    result.push(CapVertex {
                        point: origin + u * point.x as f32 + v * point.y as f32 + normal * 0.00002,
                        normal: -normal,
                        color,
                    });
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn closed(x0: f32, x1: f32, z0: f32, z1: f32) -> Vec<Vec3> {
        vec![
            Vec3::new(x0, 0., z0),
            Vec3::new(x1, 0., z0),
            Vec3::new(x1, 0., z1),
            Vec3::new(x0, 0., z1),
            Vec3::new(x0, 0., z0),
        ]
    }
    fn area(vertices: &[CapVertex]) -> f32 {
        vertices
            .chunks_exact(3)
            .map(|tri| {
                (tri[1].point - tri[0].point)
                    .cross(tri[2].point - tri[0].point)
                    .length()
                    * 0.5
            })
            .sum()
    }
    #[test]
    fn even_odd_caps_keep_disconnected_door_gap_and_nested_hole() {
        let caps = triangulate_cap(
            &[closed(-2., -0.5, -0.1, 0.1), closed(0.5, 2., -0.1, 0.1)],
            Vec3::ZERO,
            Vec3::NEG_Y,
            [1.; 4],
        );
        assert!((area(&caps) - 0.6).abs() < 1e-5);
        for tri in caps.chunks_exact(3) {
            let center = (tri[0].point + tri[1].point + tri[2].point) / 3.;
            assert!(center.x.abs() >= 0.5);
        }
        let caps = triangulate_cap(
            &[closed(-2., 2., -2., 2.), closed(-1., 1., -1., 1.)],
            Vec3::ZERO,
            Vec3::Y,
            [1.; 4],
        );
        assert!((area(&caps) - 12.).abs() < 1e-5);
        for tri in caps.chunks_exact(3) {
            let center = (tri[0].point + tri[1].point + tri[2].point) / 3.;
            assert!(center.x.abs() >= 1. || center.z.abs() >= 1.);
        }
        let mut open = closed(-2., 2., -2., 2.);
        open.pop();
        assert!(triangulate_cap(&[open], Vec3::ZERO, Vec3::Y, [1.; 4]).is_empty());
    }
    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .add_message::<AssetEvent<Mesh>>()
            .add_message::<AssetEvent<StandardMaterial>>()
            .add_plugins(SectionCapsPlugin);
        app
    }
    fn source(app: &mut App, id: u64, mesh: Mesh) -> Entity {
        let handle = app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh);
        app.world_mut()
            .spawn((
                ElementId(id),
                Mesh3d(handle),
                GlobalTransform::IDENTITY,
                Visibility::Visible,
                InheritedVisibility::VISIBLE,
            ))
            .id()
    }
    #[test]
    fn actual_mesh_caps_cache_idle_and_invalidate_edit_visibility_plane_and_removal() {
        let mut app = app();
        let plane = app
            .world_mut()
            .spawn(ClipPlaneNode {
                name: "cut".into(),
                origin: Vec3::ZERO,
                normal: Vec3::NEG_Y,
                active: true,
            })
            .id();
        let source = source(&mut app, 1, Mesh::from(Cuboid::new(2., 2., 0.2)));
        app.update();
        let (uploads, cap) = {
            let cache = app.world().resource::<CapCache>();
            assert!(!cache.parts[&source].is_empty());
            (cache.uploads, cache.mesh.clone().unwrap())
        };
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(app.world().resource::<CapCache>().uploads, uploads);
        assert_eq!(
            app.world().resource::<CapCache>().mesh.as_ref().unwrap(),
            &cap
        );
        // Presentation transform updates, without an authored history change.
        *app.world_mut().get_mut::<GlobalTransform>(source).unwrap() =
            GlobalTransform::from_translation(Vec3::X);
        app.update();
        assert!(app.world().resource::<CapCache>().parts[&source]
            .iter()
            .all(|v| v.point.x >= -1e-5));
        *app.world_mut().get_mut::<Visibility>(source).unwrap() = Visibility::Hidden;
        app.update();
        assert!(app.world().resource::<CapCache>().parts.is_empty());
        *app.world_mut().get_mut::<Visibility>(source).unwrap() = Visibility::Visible;
        app.update();
        assert!(!app.world().resource::<CapCache>().parts.is_empty());
        app.world_mut()
            .get_mut::<ClipPlaneNode>(plane)
            .unwrap()
            .active = false;
        app.update();
        assert!(app.world().resource::<CapCache>().parts.is_empty());
        app.world_mut()
            .get_mut::<ClipPlaneNode>(plane)
            .unwrap()
            .active = true;
        app.update();
        assert!(!app.world().resource::<CapCache>().parts.is_empty());
        app.world_mut().despawn(source);
        app.update();
        assert!(app.world().resource::<CapCache>().parts.is_empty());
        assert!(app.world().resource::<CapCache>().sources.is_empty());
    }
    #[test]
    fn mesh_and_material_asset_events_refresh_caps_without_authored_changes() {
        let mut app = app();
        app.world_mut().spawn(ClipPlaneNode::at_y(0.));
        let entity = source(&mut app, 1, Mesh::from(Cuboid::new(2., 2., 0.2)));
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.world_mut()
            .entity_mut(entity)
            .insert(MeshMaterial3d(material.clone()));
        app.update();
        assert!((area(&app.world().resource::<CapCache>().parts[&entity]) - 0.4).abs() < 1e-5);
        assert!(app.world().resource::<CapCache>().parts[&entity]
            .iter()
            .all(|vertex| vertex.color == [0.4, 0.4, 0.4, 1.0]));
        let mesh = app.world().get::<Mesh3d>(entity).unwrap().0.clone();
        *app.world_mut()
            .resource_mut::<Assets<Mesh>>()
            .get_mut(&mesh)
            .unwrap() = Mesh::from(Cuboid::new(4., 2., 0.2));
        app.world_mut()
            .write_message(AssetEvent::<Mesh>::Modified { id: mesh.id() });
        app.update();
        assert!((area(&app.world().resource::<CapCache>().parts[&entity]) - 0.8).abs() < 1e-5);
        app.world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .get_mut(&material)
            .unwrap()
            .base_color = Color::srgba(1., 1., 1., 0.2);
        app.world_mut()
            .write_message(AssetEvent::<StandardMaterial>::Modified { id: material.id() });
        app.update();
        assert!(!app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
        app.world_mut()
            .entity_mut(entity)
            .remove::<MeshMaterial3d<StandardMaterial>>();
        app.update();
        assert!(app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
    }

    #[test]
    fn headless_plugin_and_source_filter_changes_are_safe() {
        let mut headless = App::new();
        headless.add_plugins(SectionCapsPlugin);
        headless.update();
        let mut app = app();
        app.world_mut().spawn(ClipPlaneNode::at_y(0.));
        let entity = source(&mut app, 1, Mesh::from(Cuboid::new(2., 2., 0.2)));
        app.update();
        assert!(app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
        app.world_mut().entity_mut(entity).insert(PreviewOnly);
        app.update();
        assert!(!app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
        app.world_mut().entity_mut(entity).remove::<PreviewOnly>();
        app.update();
        assert!(app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
        app.world_mut().entity_mut(entity).remove::<ElementId>();
        app.update();
        assert!(!app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
    }

    #[test]
    fn inherited_parent_visibility_propagates_before_cap_rebuild() {
        let mut app = app();
        app.add_plugins(MinimalPlugins)
            .init_resource::<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>()
            .add_plugins(bevy::camera::visibility::VisibilityPlugin);
        app.world_mut().spawn(ClipPlaneNode::at_y(0.));
        let parent = app.world_mut().spawn(Visibility::Visible).id();
        let entity = source(&mut app, 1, Mesh::from(Cuboid::new(2., 2., 0.2)));
        app.world_mut()
            .entity_mut(entity)
            .insert((Visibility::Inherited, ChildOf(parent)));
        app.update();
        assert!(app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
        *app.world_mut().get_mut::<Visibility>(parent).unwrap() = Visibility::Hidden;
        app.update();
        assert!(!app
            .world()
            .get::<InheritedVisibility>(entity)
            .unwrap()
            .get());
        assert!(!app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
        *app.world_mut().get_mut::<Visibility>(parent).unwrap() = Visibility::Visible;
        app.update();
        assert!(app
            .world()
            .get::<InheritedVisibility>(entity)
            .unwrap()
            .get());
        assert!(app
            .world()
            .resource::<CapCache>()
            .parts
            .contains_key(&entity));
    }

    #[test]
    fn triangle_budget_defers_remaining_sources_and_rejects_oversized_single_source() {
        let mut app = app();
        app.world_mut().spawn(ClipPlaneNode::at_y(0.));
        // Degenerate triangles isolate the scheduling budget from polygon cost.
        let mesh = |triangles| {
            let mut m = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::MAIN_WORLD,
            );
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0., 1., 0.]; triangles * 3]);
            m
        };
        source(&mut app, 1, mesh(TRIANGLES_PER_UPDATE / 2 + 1));
        source(&mut app, 2, mesh(TRIANGLES_PER_UPDATE / 2 + 1));
        app.update();
        assert_eq!(app.world().resource::<CapCache>().pending.len(), 1);
        app.update();
        assert!(app.world().resource::<CapCache>().pending.is_empty());
        source(&mut app, 3, mesh(TRIANGLES_PER_UPDATE + 1));
        app.update();
        assert_eq!(app.world().resource::<CapCache>().oversized_sources, 1);
        assert!(app.world().resource::<CapCache>().pending.is_empty());
    }
}
