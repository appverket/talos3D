//! Invalidation-bounded live presentation of the canonical [`DrawingScene`].
//!
//! Model projection happens only in [`build_drawing_scene`]. This backend
//! caches that result, maps its normalized line batch back onto the captured
//! orthographic view plane once per rebuild, and submits the cached vertices
//! through Bevy's batched gizmo renderer. Idle frames never inspect meshes or
//! reclassify edges.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use bevy::{
    asset::{AssetEvent, AssetId},
    gizmos::config::{GizmoConfigGroup, GizmoConfigStore},
    prelude::*,
    transform::TransformSystems,
};

use crate::{
    curation::MaterialSpecRegistry,
    plugins::{
        camera::OrbitCamera,
        clipping_planes::ClipPlaneNode,
        dimension_line::{DimensionLineNode, DimensionLineVisibility},
        drafting::{
            DimensionAnnotationNode, DimensionStyleRegistry, DraftNode, DraftingVisibility,
            DraftingWorkspaceState,
        },
        identity::ElementId,
        materials::{MaterialAssignment, MaterialRegistry},
        registry_generation::RegistryGeneration,
    },
};

use super::{
    build_drawing_scene, drawing_normalized_to_world, sheet_view_from_active_camera, DrawingScene,
    DrawingSceneLineSpan, DEFAULT_MARGIN_MM, DEFAULT_SCALE_DENOMINATOR,
};

/// Full-scene rebuilds are coalesced during continuous camera/model gestures.
/// This is a hard upper bound, not a target frame rate for CPU projection.
const MIN_REBUILD_INTERVAL: Duration = Duration::from_millis(33);
const INVALIDATE_WORKSPACE: u32 = 1 << 0;
const INVALIDATE_DRAFTING_VISIBILITY: u32 = 1 << 1;
const INVALIDATE_DIMENSION_VISIBILITY: u32 = 1 << 2;
const INVALIDATE_STYLES: u32 = 1 << 3;
const INVALIDATE_MATERIALS: u32 = 1 << 4;
const INVALIDATE_MATERIAL_SPECS: u32 = 1 << 5;
const INVALIDATE_CAMERA_TRANSFORM: u32 = 1 << 6;
const INVALIDATE_CAMERA_PROJECTION: u32 = 1 << 7;
const INVALIDATE_MESH_ADDED: u32 = 1 << 8;
const INVALIDATE_MESH_HANDLE: u32 = 1 << 9;
const INVALIDATE_MESH_TRANSFORM: u32 = 1 << 10;
const INVALIDATE_MESH_VISIBILITY: u32 = 1 << 11;
const INVALIDATE_MESH_IDENTITY: u32 = 1 << 12;
const INVALIDATE_MESH_MATERIAL: u32 = 1 << 13;
const INVALIDATE_ANNOTATION: u32 = 1 << 14;
const INVALIDATE_CLIP: u32 = 1 << 15;
const INVALIDATE_REMOVAL: u32 = 1 << 16;
const INVALIDATE_MESH_ASSET: u32 = 1 << 17;
const INVALIDATE_DRAFT: u32 = 1 << 18;

/// Dedicated live line group. Bevy collects calls into one GPU submission and
/// the negative depth bias keeps exact black drafting linework above surfaces.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct DrawingSceneGizmos;

#[derive(Debug, Clone)]
struct DrawingSceneWorldLineBatch {
    vertices: Vec<Vec3>,
    spans: Vec<DrawingSceneLineSpan>,
}

/// Inspectable evidence that the live backend is invalidation-bounded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrawingSceneLiveStats {
    pub rebuild_count: u64,
    pub invalidation_count: u64,
    pub last_rebuild_micros: u64,
    pub last_line_count: usize,
    pub source_model_revision: u64,
}

/// Derived live state. It is intentionally transient and never persisted.
#[derive(Resource, Debug)]
pub struct DrawingSceneLiveCache {
    scene: Option<DrawingScene>,
    world_lines: Option<DrawingSceneWorldLineBatch>,
    dirty: bool,
    last_invalidation_mask: u32,
    observed_material_generation: Option<RegistryGeneration>,
    observed_mesh_handles: HashMap<Entity, AssetId<Mesh>>,
    last_rebuild_at: Option<Instant>,
    stats: DrawingSceneLiveStats,
}

impl Default for DrawingSceneLiveCache {
    fn default() -> Self {
        Self {
            scene: None,
            world_lines: None,
            dirty: true,
            last_invalidation_mask: 0,
            observed_material_generation: None,
            observed_mesh_handles: HashMap::new(),
            last_rebuild_at: None,
            stats: DrawingSceneLiveStats::default(),
        }
    }
}

impl DrawingSceneLiveCache {
    #[must_use]
    pub fn stats(&self) -> DrawingSceneLiveStats {
        self.stats
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[must_use]
    pub fn last_invalidation_reasons(&self) -> Vec<&'static str> {
        [
            (INVALIDATE_WORKSPACE, "workspace"),
            (INVALIDATE_DRAFTING_VISIBILITY, "drafting_visibility"),
            (INVALIDATE_DIMENSION_VISIBILITY, "dimension_visibility"),
            (INVALIDATE_STYLES, "dimension_styles"),
            (INVALIDATE_MATERIALS, "materials"),
            (INVALIDATE_MATERIAL_SPECS, "material_specs"),
            (INVALIDATE_CAMERA_TRANSFORM, "camera_transform"),
            (INVALIDATE_CAMERA_PROJECTION, "camera_projection"),
            (INVALIDATE_MESH_ADDED, "mesh_added"),
            (INVALIDATE_MESH_HANDLE, "mesh_handle"),
            (INVALIDATE_MESH_TRANSFORM, "mesh_transform"),
            (INVALIDATE_MESH_VISIBILITY, "mesh_visibility"),
            (INVALIDATE_MESH_IDENTITY, "mesh_identity"),
            (INVALIDATE_MESH_MATERIAL, "mesh_material"),
            (INVALIDATE_ANNOTATION, "annotation"),
            (INVALIDATE_CLIP, "clip"),
            (INVALIDATE_REMOVAL, "removal"),
            (INVALIDATE_MESH_ASSET, "mesh_asset"),
            (INVALIDATE_DRAFT, "draft"),
        ]
        .into_iter()
        .filter_map(|(flag, label)| (self.last_invalidation_mask & flag != 0).then_some(label))
        .collect()
    }

    fn invalidate(&mut self, reasons: u32) {
        self.dirty = true;
        self.last_invalidation_mask = reasons;
        self.stats.invalidation_count = self.stats.invalidation_count.saturating_add(1);
    }

    fn clear(&mut self) {
        self.scene = None;
        self.world_lines = None;
        self.dirty = true;
        self.last_rebuild_at = None;
        self.last_invalidation_mask = 0;
        self.observed_material_generation = None;
        self.observed_mesh_handles.clear();
        self.stats.last_line_count = 0;
        self.stats.source_model_revision = 0;
    }

    fn rebuild_due(&self, now: Instant) -> bool {
        self.dirty
            && self
                .last_rebuild_at
                .is_none_or(|last| now.duration_since(last) >= MIN_REBUILD_INTERVAL)
    }

    fn observe_mesh_handle(&mut self, entity: Entity, handle: AssetId<Mesh>) -> bool {
        self.observed_mesh_handles
            .insert(entity, handle)
            .is_none_or(|previous| previous != handle)
    }

    fn record_rebuild(
        &mut self,
        scene: Option<DrawingScene>,
        world_lines: Option<DrawingSceneWorldLineBatch>,
        elapsed: Duration,
        now: Instant,
    ) {
        self.stats.rebuild_count = self.stats.rebuild_count.saturating_add(1);
        self.stats.last_rebuild_micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.stats.last_line_count = world_lines.as_ref().map_or(0, |batch| batch.spans.len());
        self.stats.source_model_revision = scene
            .as_ref()
            .map_or(0, |scene| scene.source_model_revision);
        self.scene = scene;
        self.world_lines = world_lines;
        self.dirty = false;
        self.last_rebuild_at = Some(now);
    }
}

pub struct DrawingSceneLivePlugin;

impl Plugin for DrawingSceneLivePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DrawingSceneLiveCache>()
            .init_gizmo_group::<DrawingSceneGizmos>()
            .add_systems(Startup, configure_drawing_scene_gizmos)
            .add_systems(
                PostUpdate,
                (
                    invalidate_live_drawing_scene,
                    rebuild_live_drawing_scene,
                    draw_live_drawing_scene,
                )
                    .chain()
                    .after(TransformSystems::Propagate),
            );
    }
}

fn configure_drawing_scene_gizmos(mut config_store: ResMut<GizmoConfigStore>) {
    let (config, _extension) = config_store.config_mut::<DrawingSceneGizmos>();
    config.depth_bias = -1.0;
}

#[allow(clippy::too_many_arguments)]
fn invalidate_live_drawing_scene(
    workspace: Option<Res<DraftingWorkspaceState>>,
    drafting_visibility: Option<Res<DraftingVisibility>>,
    dimension_visibility: Option<Res<DimensionLineVisibility>>,
    styles: Option<Res<DimensionStyleRegistry>>,
    materials: Option<Res<MaterialRegistry>>,
    material_specs: Option<Res<MaterialSpecRegistry>>,
    mut scene_changes: ParamSet<(
        Query<(), (With<OrbitCamera>, Changed<GlobalTransform>)>,
        Query<(), (With<OrbitCamera>, Changed<Projection>)>,
        Query<(), (With<ElementId>, Added<Mesh3d>)>,
        Query<(Entity, &Mesh3d), (With<ElementId>, Changed<Mesh3d>)>,
        Query<(), (With<ElementId>, Changed<GlobalTransform>)>,
        Query<(), (With<ElementId>, Changed<Visibility>)>,
        Query<(), (With<Mesh3d>, Changed<ElementId>)>,
        Query<(), (With<ElementId>, Changed<MaterialAssignment>)>,
    )>,
    mut semantic_changes: ParamSet<(
        Query<
            (),
            Or<(
                Added<DimensionAnnotationNode>,
                Changed<DimensionAnnotationNode>,
                Added<DimensionLineNode>,
                Changed<DimensionLineNode>,
            )>,
        >,
        Query<(), Or<(Added<ClipPlaneNode>, Changed<ClipPlaneNode>)>>,
        Query<(), Or<(Added<DraftNode>, Changed<DraftNode>)>>,
    )>,
    mut removed_meshes: RemovedComponents<Mesh3d>,
    mut removed_annotations: RemovedComponents<DimensionAnnotationNode>,
    mut removed_legacy_dimensions: RemovedComponents<DimensionLineNode>,
    mut removed_clip_planes: RemovedComponents<ClipPlaneNode>,
    mut removed_drafts: RemovedComponents<DraftNode>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut cache: ResMut<DrawingSceneLiveCache>,
) {
    let active = workspace.as_ref().is_some_and(|state| state.is_active());
    let workspace_changed = workspace.as_ref().is_some_and(|state| state.is_changed());
    let drafting_visibility_changed = drafting_visibility
        .as_ref()
        .is_some_and(|value| value.is_changed());
    let dimension_visibility_changed = dimension_visibility
        .as_ref()
        .is_some_and(|value| value.is_changed());
    let styles_changed = styles.as_ref().is_some_and(|value| value.is_changed());
    // MaterialRegistry already exposes a process-unique semantic generation.
    // Use it instead of Bevy's coarse resource change tick: UI systems may
    // borrow the registry mutably without changing its contents.
    let material_generation = materials.as_ref().map(|value| value.generation());
    let materials_changed = material_generation != cache.observed_material_generation;
    cache.observed_material_generation = material_generation;
    let material_specs_changed = material_specs
        .as_ref()
        .is_some_and(|value| value.is_changed());
    let camera_transform_changed = !scene_changes.p0().is_empty();
    let camera_projection_changed = !scene_changes.p1().is_empty();
    let mesh_added = !scene_changes.p2().is_empty();
    // Bevy intentionally marks Mesh3d changed when the referenced mesh asset or
    // render material changes, even though the handle itself is identical.
    // DrawingScene already observes semantic mesh/material inputs separately,
    // so only a real handle identity change belongs on this invalidation path.
    let mesh_handle_changed = scene_changes
        .p3()
        .iter()
        .any(|(entity, mesh)| cache.observe_mesh_handle(entity, mesh.id()));
    let mesh_transform_changed = !scene_changes.p4().is_empty();
    let mesh_visibility_changed = !scene_changes.p5().is_empty();
    let mesh_identity_changed = !scene_changes.p6().is_empty();
    let mesh_material_changed = !scene_changes.p7().is_empty();
    let annotation_changed = !semantic_changes.p0().is_empty();
    let clip_changed = !semantic_changes.p1().is_empty();
    let draft_changed = !semantic_changes.p2().is_empty();
    let removed_mesh_entities: Vec<Entity> = removed_meshes.read().collect();
    for entity in &removed_mesh_entities {
        cache.observed_mesh_handles.remove(entity);
    }
    let removed_annotations = removed_annotations.read().count() > 0;
    let removed_legacy_dimensions = removed_legacy_dimensions.read().count() > 0;
    let removed_clip_planes = removed_clip_planes.read().count() > 0;
    let removed_drafts = removed_drafts.read().count() > 0;
    let removed = !removed_mesh_entities.is_empty()
        || removed_annotations
        || removed_legacy_dimensions
        || removed_clip_planes
        || removed_drafts;
    let mesh_asset_changed = mesh_events.read().count() > 0;

    if !active {
        if cache.scene.is_some() || !cache.dirty {
            cache.clear();
        }
        return;
    }
    let mut reasons = 0;
    if workspace_changed {
        reasons |= INVALIDATE_WORKSPACE;
    }
    if drafting_visibility_changed {
        reasons |= INVALIDATE_DRAFTING_VISIBILITY;
    }
    if dimension_visibility_changed {
        reasons |= INVALIDATE_DIMENSION_VISIBILITY;
    }
    if styles_changed {
        reasons |= INVALIDATE_STYLES;
    }
    if materials_changed {
        reasons |= INVALIDATE_MATERIALS;
    }
    if material_specs_changed {
        reasons |= INVALIDATE_MATERIAL_SPECS;
    }
    if camera_transform_changed {
        reasons |= INVALIDATE_CAMERA_TRANSFORM;
    }
    if camera_projection_changed {
        reasons |= INVALIDATE_CAMERA_PROJECTION;
    }
    if mesh_added {
        reasons |= INVALIDATE_MESH_ADDED;
    }
    if mesh_handle_changed {
        reasons |= INVALIDATE_MESH_HANDLE;
    }
    if mesh_transform_changed {
        reasons |= INVALIDATE_MESH_TRANSFORM;
    }
    if mesh_visibility_changed {
        reasons |= INVALIDATE_MESH_VISIBILITY;
    }
    if mesh_identity_changed {
        reasons |= INVALIDATE_MESH_IDENTITY;
    }
    if mesh_material_changed {
        reasons |= INVALIDATE_MESH_MATERIAL;
    }
    if annotation_changed {
        reasons |= INVALIDATE_ANNOTATION;
    }
    if clip_changed {
        reasons |= INVALIDATE_CLIP;
    }
    if removed {
        reasons |= INVALIDATE_REMOVAL;
    }
    if mesh_asset_changed {
        reasons |= INVALIDATE_MESH_ASSET;
    }
    if draft_changed || removed_drafts {
        reasons |= INVALIDATE_DRAFT;
    }
    if reasons != 0 {
        cache.invalidate(reasons);
    }
}

fn rebuild_live_drawing_scene(world: &mut World) {
    let active = world
        .get_resource::<DraftingWorkspaceState>()
        .is_some_and(DraftingWorkspaceState::is_active);
    if !active {
        return;
    }

    let now = Instant::now();
    let rebuild_due = world
        .get_resource::<DrawingSceneLiveCache>()
        .is_some_and(|cache| cache.rebuild_due(now));
    if !rebuild_due {
        return;
    }

    let started = Instant::now();
    let layout = crate::plugins::drafting::active_draft_layout(world);
    let scale = layout
        .as_ref()
        .map_or(DEFAULT_SCALE_DENOMINATOR, |layout| layout.scale_denominator);
    let margin = layout
        .as_ref()
        .map_or(DEFAULT_MARGIN_MM, |layout| layout.margin_mm);
    let scene = sheet_view_from_active_camera(world, scale, margin)
        .and_then(|view| build_drawing_scene(world, &view));
    let world_lines = scene.as_ref().and_then(scene_world_line_batch);
    let elapsed = started.elapsed();

    if let Some(mut cache) = world.get_resource_mut::<DrawingSceneLiveCache>() {
        cache.record_rebuild(scene, world_lines, elapsed, now);
    }
}

fn scene_world_line_batch(scene: &DrawingScene) -> Option<DrawingSceneWorldLineBatch> {
    let paper_size = Vec2::new(
        scene.view.frustum_width_mm(),
        scene.view.frustum_height_mm(),
    );
    let paper = scene.presentation_line_batch(paper_size);
    let vertices = paper
        .vertices
        .iter()
        .map(|point| drawing_normalized_to_world(&scene.view, *point / paper_size))
        .collect::<Option<Vec<_>>>()?;
    Some(DrawingSceneWorldLineBatch {
        vertices,
        spans: paper.spans,
    })
}

fn draw_live_drawing_scene(
    workspace: Option<Res<DraftingWorkspaceState>>,
    cache: Res<DrawingSceneLiveCache>,
    mut gizmos: Gizmos<DrawingSceneGizmos>,
) {
    if !workspace.as_ref().is_some_and(|state| state.is_active()) {
        return;
    }
    let Some(batch) = cache.world_lines.as_ref() else {
        return;
    };
    for span in &batch.spans {
        let start = span.vertices.start as usize;
        let end = span.vertices.end as usize;
        let Some(vertices) = batch.vertices.get(start..end) else {
            continue;
        };
        if let [a, b] = vertices {
            gizmos.line(*a, *b, Color::BLACK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting_sheet::{
        DrawingPrimitiveId, DrawingPrimitiveRole, DrawingSceneLine, SheetStroke, SheetView,
    };

    #[test]
    fn idle_cache_does_not_rebuild_until_invalidated() {
        let now = Instant::now();
        let mut cache = DrawingSceneLiveCache::default();
        assert!(cache.rebuild_due(now));
        cache.record_rebuild(None, None, Duration::from_micros(50), now);
        assert!(!cache.rebuild_due(now + MIN_REBUILD_INTERVAL));
        cache.invalidate(INVALIDATE_CAMERA_TRANSFORM);
        assert!(!cache.rebuild_due(now + MIN_REBUILD_INTERVAL / 2));
        assert!(cache.rebuild_due(now + MIN_REBUILD_INTERVAL));
    }

    #[test]
    fn renderer_change_ticks_do_not_masquerade_as_handle_changes() {
        let mut cache = DrawingSceneLiveCache::default();
        let entity = Entity::from_bits(7);
        let first = Handle::<Mesh>::default().id();
        let second = AssetId::<Mesh>::invalid();

        assert!(cache.observe_mesh_handle(entity, first));
        assert!(!cache.observe_mesh_handle(entity, first));
        assert!(cache.observe_mesh_handle(entity, second));
    }

    #[test]
    fn world_batch_preserves_scene_primitive_identity() {
        let view = SheetView {
            eye: Vec3::new(0.0, 0.0, 10.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 2.0,
            aspect: 2.0,
            scale_denominator: 50.0,
            margin_mm: 0.0,
        };
        let owner = ElementId(9);
        let id = DrawingPrimitiveId {
            owner,
            role: DrawingPrimitiveRole::VisibleEdge,
            ordinal: 0,
        };
        let mut scene = DrawingScene::new(view);
        scene.lines.push(DrawingSceneLine {
            id,
            owner,
            a: Vec2::new(0.25, 0.5),
            b: Vec2::new(0.75, 0.5),
            stroke: SheetStroke::Silhouette,
        });

        let batch = scene_world_line_batch(&scene).expect("valid orthographic view");
        assert_eq!(batch.spans[0].id, id);
        assert!((batch.vertices[0] - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5);
        assert!((batch.vertices[1] - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5);
    }
}
