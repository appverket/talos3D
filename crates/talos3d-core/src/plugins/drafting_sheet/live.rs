//! Invalidation-bounded live presentation of the canonical [`DrawingScene`].
//!
//! Model projection happens only in [`build_drawing_scene`]. This backend
//! caches that result, maps its normalized line batch back onto the captured
//! orthographic view plane once per rebuild, and submits the cached vertices
//! through Bevy's batched gizmo renderer. Idle frames never inspect meshes or
//! reclassify edges.

use std::time::{Duration, Instant};

use bevy::{
    asset::AssetEvent,
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
            DimensionAnnotationNode, DimensionStyleRegistry, DraftingVisibility,
            DraftingWorkspaceState,
        },
        identity::ElementId,
        materials::{MaterialAssignment, MaterialRegistry},
    },
};

use super::{
    build_drawing_scene, drawing_normalized_to_world, sheet_view_from_active_camera, DrawingScene,
    DrawingSceneLineSpan, DEFAULT_MARGIN_MM, DEFAULT_SCALE_DENOMINATOR,
};

/// Full-scene rebuilds are coalesced during continuous camera/model gestures.
/// This is a hard upper bound, not a target frame rate for CPU projection.
const MIN_REBUILD_INTERVAL: Duration = Duration::from_millis(33);

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
    last_rebuild_at: Option<Instant>,
    stats: DrawingSceneLiveStats,
}

impl Default for DrawingSceneLiveCache {
    fn default() -> Self {
        Self {
            scene: None,
            world_lines: None,
            dirty: true,
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

    fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn clear(&mut self) {
        self.scene = None;
        self.world_lines = None;
        self.dirty = true;
        self.last_rebuild_at = None;
        self.stats.last_line_count = 0;
        self.stats.source_model_revision = 0;
    }

    fn rebuild_due(&self, now: Instant) -> bool {
        self.dirty
            && self
                .last_rebuild_at
                .is_none_or(|last| now.duration_since(last) >= MIN_REBUILD_INTERVAL)
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
    camera_changes: Query<
        (),
        (
            With<OrbitCamera>,
            Or<(Changed<GlobalTransform>, Changed<Projection>)>,
        ),
    >,
    mesh_changes: Query<
        (),
        (
            With<ElementId>,
            Or<(
                Added<Mesh3d>,
                Changed<Mesh3d>,
                Changed<GlobalTransform>,
                Changed<Visibility>,
                Changed<ElementId>,
                Changed<MaterialAssignment>,
            )>,
        ),
    >,
    annotation_changes: Query<
        (),
        Or<(
            Added<DimensionAnnotationNode>,
            Changed<DimensionAnnotationNode>,
            Added<DimensionLineNode>,
            Changed<DimensionLineNode>,
        )>,
    >,
    clip_changes: Query<(), Or<(Added<ClipPlaneNode>, Changed<ClipPlaneNode>)>>,
    mut removed_meshes: RemovedComponents<Mesh3d>,
    mut removed_annotations: RemovedComponents<DimensionAnnotationNode>,
    mut removed_legacy_dimensions: RemovedComponents<DimensionLineNode>,
    mut removed_clip_planes: RemovedComponents<ClipPlaneNode>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut cache: ResMut<DrawingSceneLiveCache>,
) {
    let active = workspace.as_ref().is_some_and(|state| state.is_active());
    let workspace_changed = workspace.as_ref().is_some_and(|state| state.is_changed());
    let resource_changed = drafting_visibility
        .as_ref()
        .is_some_and(|value| value.is_changed())
        || dimension_visibility
            .as_ref()
            .is_some_and(|value| value.is_changed())
        || styles.as_ref().is_some_and(|value| value.is_changed())
        || materials.as_ref().is_some_and(|value| value.is_changed())
        || material_specs
            .as_ref()
            .is_some_and(|value| value.is_changed());
    let component_changed = !camera_changes.is_empty()
        || !mesh_changes.is_empty()
        || !annotation_changes.is_empty()
        || !clip_changes.is_empty();
    let removed = removed_meshes.read().next().is_some()
        || removed_annotations.read().next().is_some()
        || removed_legacy_dimensions.read().next().is_some()
        || removed_clip_planes.read().next().is_some();
    let mesh_asset_changed = mesh_events.read().next().is_some();

    if !active {
        if cache.scene.is_some() || !cache.dirty {
            cache.clear();
        }
        return;
    }
    if workspace_changed || resource_changed || component_changed || removed || mesh_asset_changed {
        cache.invalidate();
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
    let scene = sheet_view_from_active_camera(world, DEFAULT_SCALE_DENOMINATOR, DEFAULT_MARGIN_MM)
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
        cache.invalidate();
        assert!(!cache.rebuild_due(now + MIN_REBUILD_INTERVAL / 2));
        assert!(cache.rebuild_due(now + MIN_REBUILD_INTERVAL));
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
