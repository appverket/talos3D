//! Invalidation-bounded live presentation of the canonical [`DrawingScene`].
//!
//! Model projection happens only in [`build_drawing_scene`]. This backend
//! caches that result, maps its normalized line batch back onto the captured
//! orthographic view plane once per rebuild, and submits the cached vertices
//! through Bevy's batched gizmo renderer. Idle frames never inspect meshes or
//! reclassify edges.

use crate::time::{Duration, Instant};
use std::collections::HashMap;

use bevy::{
    asset::{AssetEvent, AssetId},
    gizmos::config::{GizmoConfigGroup, GizmoConfigStore},
    math::Rot2,
    prelude::*,
    text::{ComputedTextBlock, TextLayoutInfo},
    transform::TransformSystems,
    window::PrimaryWindow,
};

use crate::{
    curation::MaterialSpecRegistry,
    plugins::{
        camera::OrbitCamera,
        clipping_planes::ClipPlaneNode,
        cursor::ViewportUiInset,
        dimension_line::{DimensionLineNode, DimensionLineVisibility},
        drafting::{
            DimensionAnnotationNode, DimensionStyleRegistry, DraftNode, DraftPrimitiveNode,
            DraftingVisibility, DraftingWorkspaceState, TextAnchor,
        },
        identity::ElementId,
        materials::{MaterialAssignment, MaterialRegistry},
        modeling::occurrence::GeneratedOccurrencePart,
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
const INVALIDATE_VIEWPORT: u32 = 1 << 19;

/// Dedicated live line group. Bevy collects calls into one GPU submission and
/// the negative depth bias keeps exact black drafting linework above surfaces.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct DrawingSceneGizmos;

#[derive(Debug, Clone)]
struct DrawingSceneWorldLineBatch {
    vertices: Vec<Vec3>,
    spans: Vec<DrawingSceneLineSpan>,
}

#[derive(Debug, Clone, PartialEq)]
struct DrawingSceneViewportText {
    position_px: Vec2,
    content: String,
    font_size_px: f32,
    rotation_rad: f32,
    anchor_mode: TextAnchor,
}

/// Alignment is derived from Bevy's shaped glyph layout, never estimated from
/// character count or a font-size fraction. This component is presentation only.
#[derive(Component, Clone, Copy)]
struct DrawingSceneTextAnchor {
    mode: TextAnchor,
    rotation_rad: f32,
}

/// Transient Bevy-native presentation of DrawingScene text. Authored text
/// remains owned by DraftPrimitiveNode and the canonical DrawingScene.
#[derive(Component)]
struct DrawingSceneLiveText;

/// Inspectable evidence that the live backend is invalidation-bounded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrawingSceneLiveStats {
    pub rebuild_count: u64,
    pub invalidation_count: u64,
    pub last_rebuild_micros: u64,
    pub last_line_count: usize,
    pub last_text_count: usize,
    pub source_model_revision: u64,
}

/// Derived live state. It is intentionally transient and never persisted.
#[derive(Resource, Debug)]
pub struct DrawingSceneLiveCache {
    scene: Option<DrawingScene>,
    world_lines: Option<DrawingSceneWorldLineBatch>,
    text_entities: Vec<Entity>,
    dirty: bool,
    last_invalidation_mask: u32,
    observed_workspace: Option<(bool, Option<ElementId>)>,
    observed_viewport_inset: Option<Vec4>,
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
            text_entities: Vec::new(),
            dirty: true,
            last_invalidation_mask: 0,
            observed_workspace: None,
            observed_viewport_inset: None,
            observed_material_generation: None,
            observed_mesh_handles: HashMap::new(),
            last_rebuild_at: None,
            stats: DrawingSceneLiveStats::default(),
        }
    }
}

impl DrawingSceneLiveCache {
    fn observes_mesh_event(&self, event: &AssetEvent<Mesh>) -> bool {
        match event {
            AssetEvent::Added { id } | AssetEvent::Modified { id } | AssetEvent::Removed { id } => {
                self.observed_mesh_handles
                    .values()
                    .any(|observed| observed == id)
            }
            _ => false,
        }
    }

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
            (INVALIDATE_VIEWPORT, "viewport"),
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
        // Keep the source index across presentation shutdown. Changed<Mesh3d>
        // does not replay for unchanged entities when Drafting is re-entered.
        // Invalidation keeps this index current (including removals) while off.
        self.stats.last_line_count = 0;
        self.stats.last_text_count = 0;
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
        text_count: usize,
        elapsed: Duration,
        now: Instant,
    ) {
        self.stats.rebuild_count = self.stats.rebuild_count.saturating_add(1);
        self.stats.last_rebuild_micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.stats.last_line_count = world_lines.as_ref().map_or(0, |batch| batch.spans.len());
        self.stats.last_text_count = text_count;
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
            )
            .add_systems(
                PostUpdate,
                align_live_text.after(bevy::ui::UiSystems::PostLayout),
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
        Query<
            (),
            (
                Or<(With<ElementId>, With<GeneratedOccurrencePart>)>,
                Added<Mesh3d>,
            ),
        >,
        Query<
            (Entity, &Mesh3d),
            (
                Or<(With<ElementId>, With<GeneratedOccurrencePart>)>,
                Changed<Mesh3d>,
            ),
        >,
        Query<
            (),
            (
                Or<(With<ElementId>, With<GeneratedOccurrencePart>)>,
                Changed<GlobalTransform>,
            ),
        >,
        Query<
            (),
            (
                Or<(With<ElementId>, With<GeneratedOccurrencePart>)>,
                Changed<Visibility>,
            ),
        >,
        Query<(), (With<Mesh3d>, Changed<ElementId>)>,
        Query<
            (),
            (
                Or<(With<ElementId>, With<GeneratedOccurrencePart>)>,
                Changed<MaterialAssignment>,
            ),
        >,
    )>,
    mut semantic_changes: ParamSet<(
        Query<
            (),
            Or<(
                Added<DimensionAnnotationNode>,
                Changed<DimensionAnnotationNode>,
                Added<DimensionLineNode>,
                Changed<DimensionLineNode>,
                Added<DraftPrimitiveNode>,
                Changed<DraftPrimitiveNode>,
            )>,
        >,
        Query<(), Or<(Added<ClipPlaneNode>, Changed<ClipPlaneNode>)>>,
        Query<(), Or<(Added<DraftNode>, Changed<DraftNode>)>>,
        Option<Res<ViewportUiInset>>,
    )>,
    mut removed_meshes: RemovedComponents<Mesh3d>,
    mut removed_annotations: RemovedComponents<DimensionAnnotationNode>,
    mut removed_legacy_dimensions: RemovedComponents<DimensionLineNode>,
    mut removed_draft_primitives: RemovedComponents<DraftPrimitiveNode>,
    mut removed_clip_planes: RemovedComponents<ClipPlaneNode>,
    mut removed_drafts: RemovedComponents<DraftNode>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut cache: ResMut<DrawingSceneLiveCache>,
) {
    let active = workspace.as_ref().is_some_and(|state| state.is_active());
    // UI systems borrow these resources mutably every frame. Compare semantic
    // values rather than change ticks, or idle Drafting rebuilds continuously.
    let workspace_value = (
        active,
        workspace.as_ref().and_then(|state| state.active_draft_id()),
    );
    let workspace_changed = cache.observed_workspace != Some(workspace_value);
    cache.observed_workspace = Some(workspace_value);
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
        .fold(false, |changed, (entity, mesh)| {
            cache.observe_mesh_handle(entity, mesh.id()) || changed
        });
    let mesh_transform_changed = !scene_changes.p4().is_empty();
    let mesh_visibility_changed = !scene_changes.p5().is_empty();
    let mesh_identity_changed = !scene_changes.p6().is_empty();
    let mesh_material_changed = !scene_changes.p7().is_empty();
    let annotation_changed = !semantic_changes.p0().is_empty();
    let clip_changed = !semantic_changes.p1().is_empty();
    let draft_changed = !semantic_changes.p2().is_empty();
    let viewport_value = semantic_changes
        .p3()
        .as_ref()
        .map(|inset| Vec4::new(inset.left, inset.top, inset.right, inset.bottom));
    let viewport_changed = cache.observed_viewport_inset != viewport_value;
    cache.observed_viewport_inset = viewport_value;
    let removed_mesh_entities: Vec<Entity> = removed_meshes.read().collect();
    for entity in &removed_mesh_entities {
        cache.observed_mesh_handles.remove(entity);
    }
    let removed_annotations = removed_annotations.read().count() > 0;
    let removed_legacy_dimensions = removed_legacy_dimensions.read().count() > 0;
    let removed_draft_primitives = removed_draft_primitives.read().count() > 0;
    let removed_clip_planes = removed_clip_planes.read().count() > 0;
    let removed_drafts = removed_drafts.read().count() > 0;
    let removed = !removed_mesh_entities.is_empty()
        || removed_annotations
        || removed_legacy_dimensions
        || removed_draft_primitives
        || removed_clip_planes
        || removed_drafts;
    // Preview/cap/UI meshes are derived presentation, not DrawingScene inputs.
    // Their asset uploads must not recursively rebuild the canonical scene.
    let mesh_asset_changed = mesh_events.read().fold(false, |changed, event| {
        cache.observes_mesh_event(event) || changed
    });

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
    if viewport_changed {
        reasons |= INVALIDATE_VIEWPORT;
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
        clear_live_text_entities(world);
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
    let text_count = sync_live_text_entities(world, scene.as_ref());
    let elapsed = started.elapsed();

    if let Some(mut cache) = world.get_resource_mut::<DrawingSceneLiveCache>() {
        cache.record_rebuild(scene, world_lines, text_count, elapsed, now);
    }
}

fn clear_live_text_entities(world: &mut World) {
    let entities = {
        let Some(mut cache) = world.get_resource_mut::<DrawingSceneLiveCache>() else {
            return;
        };
        std::mem::take(&mut cache.text_entities)
    };
    for entity in entities {
        let _ = world.despawn(entity);
    }
}

/// Adapt DrawingScene text into a pooled Bevy UI presentation only when the
/// canonical scene is rebuilt. Idle frames perform no text traversal or entity
/// churn, and this adapter owns no authoring semantics.
fn sync_live_text_entities(world: &mut World, scene: Option<&DrawingScene>) -> usize {
    let Some(scene) = scene else {
        clear_live_text_entities(world);
        return 0;
    };
    let Some((camera, viewport_size)) = live_viewport_target(world) else {
        clear_live_text_entities(world);
        return 0;
    };
    let specs = scene_viewport_texts(scene, viewport_size);

    let mut existing_entities = {
        let mut cache = world.resource_mut::<DrawingSceneLiveCache>();
        std::mem::take(&mut cache.text_entities)
    };

    let mut next_entities = Vec::with_capacity(specs.len());
    for spec in specs {
        let entity = existing_entities
            .pop()
            .filter(|entity| world.get_entity(*entity).is_ok())
            .unwrap_or_else(|| world.spawn_empty().id());
        world.entity_mut(entity).insert((
            DrawingSceneLiveText,
            UiTargetCamera(camera),
            DrawingSceneTextAnchor {
                mode: spec.anchor_mode,
                rotation_rad: spec.rotation_rad,
            },
            Text::new(spec.content),
            TextLayout::default().with_no_wrap(),
            TextFont {
                font_size: FontSize::Px(spec.font_size_px),
                ..default()
            },
            TextColor(Color::BLACK),
            Node {
                position_type: PositionType::Absolute,
                left: px(spec.position_px.x),
                top: px(spec.position_px.y),
                ..default()
            },
            UiTransform {
                translation: Val2::new(percent(-50.0), percent(-50.0)),
                rotation: Rot2::radians(-spec.rotation_rad),
                ..default()
            },
            Pickable::IGNORE,
            GlobalZIndex(i32::MAX - 32),
        ));
        next_entities.push(entity);
    }
    for entity in existing_entities {
        let _ = world.despawn(entity);
    }
    let count = next_entities.len();
    let mut cache = world.resource_mut::<DrawingSceneLiveCache>();
    cache.text_entities = next_entities;
    count
}

fn text_anchor_transform(size: Vec2, baseline: f32, anchor: DrawingSceneTextAnchor) -> UiTransform {
    let local_anchor = match anchor.mode {
        TextAnchor::LeftBaseline => Vec2::new(0.0, baseline),
        TextAnchor::CenterBaseline => Vec2::new(size.x * 0.5, baseline),
        TextAnchor::Center => size * 0.5,
    };
    let rotation = Rot2::radians(-anchor.rotation_rad);
    // Bevy rotates UI nodes about their centre. Move that centre so the semantic
    // insertion point stays fixed even for vertical and oblique dimensions.
    let translation = rotation * (size * 0.5 - local_anchor) - size * 0.5;
    UiTransform {
        translation: Val2::px(translation.x, translation.y),
        rotation,
        ..default()
    }
}

fn shaped_text_anchor_transform(
    layout: &TextLayoutInfo,
    physical_baseline: f32,
    anchor: DrawingSceneTextAnchor,
) -> UiTransform {
    // Bevy UI already converts TextLayoutInfo.size to logical pixels. Parley
    // retains physical font metrics, so only its baseline needs DPI conversion.
    text_anchor_transform(layout.size, physical_baseline / layout.scale_factor, anchor)
}

fn align_live_text(
    mut texts: Query<
        (
            &DrawingSceneTextAnchor,
            &ComputedTextBlock,
            &TextLayoutInfo,
            &mut UiTransform,
        ),
        Or<(Changed<TextLayoutInfo>, Changed<DrawingSceneTextAnchor>)>,
    >,
) {
    for (anchor, shaped, layout, mut transform) in &mut texts {
        let Some(line) = shaped.buffer().lines().next() else {
            continue;
        };
        if !layout.scale_factor.is_finite() || layout.scale_factor <= 0.0 {
            continue;
        }
        let aligned = shaped_text_anchor_transform(layout, line.metrics().baseline, *anchor);
        if *transform != aligned {
            *transform = aligned;
        }
    }
}

fn live_viewport_target(world: &mut World) -> Option<(Entity, Vec2)> {
    let mut cameras = world.try_query_filtered::<(Entity, &Camera), With<OrbitCamera>>()?;
    let (entity, camera) = cameras.single(world).ok()?;
    let computed_size = camera.logical_viewport_size();
    let has_explicit_viewport = camera.viewport.is_some();
    // DrawingScene projects the full camera frustum. UI panels cover its edges;
    // they do not crop the camera or change the projected annotation anchors.
    // UiTargetCamera also makes UI coordinates relative to the same viewport.
    let size = computed_size.or_else(|| {
        // Before the camera's first render update, the normal full-window
        // camera has no computed target size yet.
        if has_explicit_viewport {
            return None;
        }
        let mut windows = world.try_query_filtered::<&Window, With<PrimaryWindow>>()?;
        windows.single(world).ok().map(|window| window.size())
    })?;
    size.cmpgt(Vec2::ZERO).all().then_some((entity, size))
}

fn scene_viewport_texts(
    scene: &DrawingScene,
    viewport_size: Vec2,
) -> Vec<DrawingSceneViewportText> {
    let paper_height = scene.view.frustum_height_mm();
    if !paper_height.is_finite() || paper_height <= 0.0 {
        return Vec::new();
    }
    scene
        .annotations
        .iter()
        .flat_map(|annotation| annotation.primitives.iter())
        .filter_map(|primitive| {
            let crate::plugins::drafting::DimPrimitive::Text {
                anchor,
                content,
                height_mm,
                rotation_rad,
                anchor_mode,
                ..
            } = primitive
            else {
                return None;
            };
            if !anchor.is_finite()
                || !anchor.cmpge(Vec2::ZERO).all()
                || !anchor.cmple(Vec2::ONE).all()
                || !height_mm.is_finite()
                || *height_mm <= 0.0
                || !rotation_rad.is_finite()
            {
                return None;
            }
            Some(DrawingSceneViewportText {
                position_px: Vec2::new(
                    anchor.x * viewport_size.x,
                    (1.0 - anchor.y) * viewport_size.y,
                ),
                content: content.clone(),
                font_size_px: (*height_mm / paper_height * viewport_size.y).max(1.0),
                rotation_rad: *rotation_rad,
                anchor_mode: *anchor_mode,
            })
        })
        .collect()
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
    use crate::plugins::{
        drafting::{DimPrimitive, TextAnchor},
        drafting_sheet::{
            DrawingPrimitiveId, DrawingPrimitiveRole, DrawingSceneAnnotation, DrawingSceneLine,
            SheetStroke, SheetView,
        },
    };

    #[test]
    fn idle_cache_does_not_rebuild_until_invalidated() {
        let now = Instant::now();
        let mut cache = DrawingSceneLiveCache::default();
        assert!(cache.rebuild_due(now));
        cache.record_rebuild(None, None, 0, Duration::from_micros(50), now);
        assert!(!cache.rebuild_due(now + MIN_REBUILD_INTERVAL));
        cache.invalidate(INVALIDATE_CAMERA_TRANSFORM);
        assert!(!cache.rebuild_due(now + MIN_REBUILD_INTERVAL / 2));
        assert!(cache.rebuild_due(now + MIN_REBUILD_INTERVAL));
    }

    #[test]
    fn leaving_and_reentering_draft_preserves_mesh_removal_invalidation() {
        let mut assets = Assets::<Mesh>::default();
        let mesh = assets.add(Cuboid::default());
        let mut cache = DrawingSceneLiveCache::default();
        let entity = Entity::from_bits(7);
        cache.observe_mesh_handle(entity, mesh.id());
        let now = Instant::now();
        cache.record_rebuild(None, None, 0, Duration::ZERO, now);
        cache.clear(); // Leaving Draft removes presentation, not source tracking.
        cache.record_rebuild(None, None, 0, Duration::ZERO, now); // Re-enter unchanged.
        assert!(!cache.is_dirty());
        let removal = AssetEvent::Removed { id: mesh.id() };
        assert!(cache.observes_mesh_event(&removal));
        if cache.observes_mesh_event(&removal) {
            cache.invalidate(INVALIDATE_MESH_ASSET);
        }
        assert!(cache.is_dirty());
        assert_eq!(cache.last_invalidation_reasons(), vec!["mesh_asset"]);
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

    #[test]
    fn shaped_text_dpi_conversion_keeps_logical_size_and_scales_only_baseline() {
        let anchor = DrawingSceneTextAnchor {
            mode: TextAnchor::CenterBaseline,
            rotation_rad: 0.7,
        };
        let logical_size = Vec2::new(80.0, 20.0);
        let normal = TextLayoutInfo {
            size: logical_size,
            scale_factor: 1.0,
            ..default()
        };
        let retina = TextLayoutInfo {
            size: logical_size,
            scale_factor: 2.0,
            ..default()
        };
        assert_eq!(
            shaped_text_anchor_transform(&normal, 15.0, anchor),
            shaped_text_anchor_transform(&retina, 30.0, anchor)
        );
    }

    #[test]
    fn shaped_text_alignment_preserves_baseline_anchor_through_rotation() {
        let size = Vec2::new(80.0, 20.0);
        let baseline = 15.0;
        for angle in [0.0, std::f32::consts::FRAC_PI_2, 0.37] {
            for (mode, local_anchor) in [
                (TextAnchor::LeftBaseline, Vec2::new(0.0, baseline)),
                (TextAnchor::CenterBaseline, Vec2::new(40.0, baseline)),
                (TextAnchor::Center, size * 0.5),
            ] {
                let transform = text_anchor_transform(
                    size,
                    baseline,
                    DrawingSceneTextAnchor {
                        mode,
                        rotation_rad: angle,
                    },
                );
                let Val::Px(x) = transform.translation.x else {
                    panic!("pixel alignment");
                };
                let Val::Px(y) = transform.translation.y else {
                    panic!("pixel alignment");
                };
                let projected_anchor =
                    size * 0.5 + Vec2::new(x, y) + transform.rotation * (local_anchor - size * 0.5);
                assert!(projected_anchor.length() < 1e-5, "{mode:?} at {angle}");
            }
        }
    }

    #[test]
    fn default_plugins_include_the_ui_render_pass_for_draft_text() {
        // Text and UiPlugin alone produce layout and entities but no pixels.
        // Exercise the actual feature-selected application plugin group.
        assert!(DefaultPlugins
            .build()
            .contains::<bevy::ui_render::UiRenderPlugin>());
    }

    #[test]
    fn text_projection_uses_main_camera_extent_despite_asymmetric_panels() {
        let mut world = World::new();
        world.insert_resource(ViewportUiInset {
            left: 280.0,
            top: 40.0,
            right: 80.0,
            bottom: 24.0,
        });
        world.spawn((
            Window {
                resolution: (1000, 800).into(),
                ..default()
            },
            PrimaryWindow,
        ));
        let main = world
            .spawn((Camera::default(), OrbitCamera::default()))
            .id();
        world.spawn(Camera::default()); // Definition preview must not be selected.
        assert_eq!(
            live_viewport_target(&mut world),
            Some((main, Vec2::new(1000.0, 800.0)))
        );
        world.resource_mut::<ViewportUiInset>().left = 0.0;
        assert_eq!(
            live_viewport_target(&mut world),
            Some((main, Vec2::new(1000.0, 800.0)))
        );
    }

    #[test]
    fn mesh_invalidation_ignores_derived_uploads_but_tracks_authored_asset_edits() {
        let mut assets = Assets::<Mesh>::default();
        let authored = assets.add(Cuboid::default());
        let derived = assets.add(Cuboid::default());
        let mut cache = DrawingSceneLiveCache::default();
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        cache.observe_mesh_handle(entity, authored.id());
        assert!(!cache.observes_mesh_event(&AssetEvent::Modified { id: derived.id() }));
        assert!(cache.observes_mesh_event(&AssetEvent::Modified { id: authored.id() }));
        assert!(cache.observes_mesh_event(&AssetEvent::Removed { id: authored.id() }));
        cache.observe_mesh_handle(entity, derived.id());
        assert!(!cache.observes_mesh_event(&AssetEvent::Modified { id: authored.id() }));
        assert!(cache.observes_mesh_event(&AssetEvent::Added { id: derived.id() }));
    }

    #[test]
    fn live_text_adapter_maps_canonical_scene_without_reauthoring() {
        let view = SheetView {
            eye: Vec3::new(0.0, 0.0, 10.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 2.0,
            aspect: 2.0,
            scale_denominator: 50.0,
            margin_mm: 0.0,
        };
        let owner = ElementId(10);
        let mut scene = DrawingScene::new(view);
        scene.annotations.push(DrawingSceneAnnotation {
            id: DrawingPrimitiveId {
                owner,
                role: DrawingPrimitiveRole::Annotation,
                ordinal: 0,
            },
            owner,
            primitives: vec![DimPrimitive::Text {
                anchor: Vec2::new(0.25, 0.75),
                content: "Shared DrawingScene".to_string(),
                height_mm: 2.5,
                rotation_rad: 0.2,
                anchor_mode: TextAnchor::CenterBaseline,
                font_family: "sans-serif".to_string(),
                color_hex: "000000".to_string(),
            }],
        });

        let texts = scene_viewport_texts(&scene, Vec2::new(1000.0, 800.0));
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].content, "Shared DrawingScene");
        assert_eq!(texts[0].position_px, Vec2::new(250.0, 200.0));
        assert!((texts[0].font_size_px - 50.0).abs() < 1e-5);
        assert!((texts[0].rotation_rad - 0.2).abs() < 1e-5);
    }
}
