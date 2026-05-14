//! Real 3D occurrence preview for the Definition Editor (PP-DBUX3).
//!
//! Renders the active definition draft as a synthetic occurrence into a
//! [`RenderTarget::Image`] that is bridged into egui via the standard
//! `add_image` / `image_id` pattern.  The rendered image is then shown in the
//! Definition Editor's left-column preview panel.
//!
//! ## Safety contract
//!
//! All preview entities carry [`PreviewOnly`].  Downstream systems use
//! `Without<PreviewOnly>` as a filter to exclude preview entities from:
//!
//! * **Save / persistence** — `build_project_file` in `persistence.rs`
//! * **Scene selection** — `MeshSelectableQueryFilter` and the group-edit
//!   muting queries in `selection.rs`
//! * **Authored-element undo history** — history commands operate on
//!   `ElementId`-identified authored entities, not raw preview entities
//!
//! Preview entities are _not_ exported by the export / DXF / SVG / DraftingSheet
//! paths because those paths iterate authored entities via `ElementId` queries;
//! the occurrence root is tagged `PreviewOnly` so it is filtered by the
//! `Without<PreviewOnly>` guard added to `build_project_file`, and the
//! generated parts never carry `ElementId` at all.
//!
//! ## ElementId sentinel
//!
//! The occurrence root entity carries `ElementId(u64::MAX - 1)` — a reserved
//! sentinel that the [`ElementIdAllocator`] will never emit (the allocator
//! starts at 0 and increments; reaching `u64::MAX - 1` in normal operation is
//! not realistic).  This sentinel is needed so that `evaluate_occurrences`
//! (which requires `ElementId` + `OccurrenceIdentity` + `NeedsEval`) can
//! locate and evaluate the preview occurrence through the standard eval
//! pipeline, producing genuine `GeneratedOccurrencePart` entities with real
//! material assignments — exactly matching what a live occurrence would render.
//!
//! The `Without<PreviewOnly>` guard in `build_project_file` ensures this
//! sentinel entity is never captured by the persistence layer even though it
//! carries `ElementId`.

use bevy::{
    camera::{visibility::RenderLayers, RenderTarget},
    gizmos::config::{GizmoConfigGroup, GizmoConfigStore},
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    },
};
use bevy_egui::{egui, EguiContexts, EguiTextureHandle};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use crate::plugins::{
    definition_browser::{DefinitionEditorNode, DefinitionsWindowState},
    identity::ElementId,
    modeling::{
        definition::{DefinitionId, DefinitionRegistry, OverrideMap},
        mesh_generation::EvaluationSet,
        occurrence::{render_occurrence, GeneratedOccurrencePart, OccurrenceIdentity},
        primitive_trait::Primitive as _,
        primitives::ShapeRotation,
        profile::ProfileExtrusion,
    },
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Size (in pixels) of the render-target image used for the 3D preview.
const PREVIEW_TEXTURE_SIZE: u32 = 512;

/// The render layer used exclusively for preview-scene entities.
///
/// Layer 0 is the default layer used by all authored scene content.
/// Layer 1 is reserved here for the definition preview.  No other system
/// in the codebase uses `RenderLayers::layer(1)` (verified at PP-DBUX3
/// by grepping `RenderLayers::layer` across all crates).
pub const PREVIEW_RENDER_LAYER: RenderLayers = RenderLayers::layer(1);

/// Sentinel [`ElementId`] given to the synthetic preview occurrence root.
///
/// The [`ElementIdAllocator`] starts at 0 and increments monotonically; this
/// value is unreachable in normal operation.  It is paired with
/// `Without<PreviewOnly>` in the persistence layer so the sentinel is never
/// serialised.
pub const PREVIEW_ELEMENT_ID_SENTINEL: ElementId = ElementId(u64::MAX - 1);

/// Selection highlight colour used for the selected slot's generated parts
/// in the preview.  Matches the saturated orange used by the old 2D painter.
const PREVIEW_HIGHLIGHT_COLOR: Color = Color::srgb(1.0, 0.62, 0.31);

/// Hover pulse colour — same hue as the selection highlight but dimmed.
/// Applied when the pointer hovers a slot row without clicking.
const PREVIEW_HOVER_COLOR: Color = Color::srgba(1.0, 0.62, 0.31, 0.45);

// ---------------------------------------------------------------------------
// Gizmo group
// ---------------------------------------------------------------------------

/// Custom [`GizmoConfigGroup`] for the definition-preview slot-highlight
/// wireframes.
///
/// Its `render_layers` is set to [`PREVIEW_RENDER_LAYER`] at startup so
/// the gizmos are only visible through the preview camera and never leak
/// into the main viewport.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct PreviewSelectionGizmos;

// ---------------------------------------------------------------------------
// Marker component
// ---------------------------------------------------------------------------

/// Non-persistent marker carried by every entity that belongs to the
/// definition preview scene.
///
/// Systems that must exclude preview entities (persistence, selection, export)
/// add `Without<PreviewOnly>` to their queries.  This is the single
/// gating component — no other per-entity bookkeeping is required.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PreviewOnly;

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// Tracks the state of the running definition preview scene.
///
/// Stored as a Bevy `Resource`; the sync system reads it each frame.
#[derive(Resource, Debug)]
pub struct DefinitionPreviewScene {
    /// The definition whose occurrence is currently materialised in the scene,
    /// or `None` if the preview is blank.
    pub current_definition_id: Option<DefinitionId>,
    /// Handle to the render-target image shown in egui.
    pub render_target: Handle<Image>,
    /// The preview camera entity (spawned once at startup, never despawned).
    pub camera_entity: Entity,
    /// The directional light entity (spawned once, never despawned).
    pub light_entity: Entity,
    /// The current occurrence root entity, or `Entity::PLACEHOLDER` when no
    /// occurrence is materialised.
    pub occurrence_root: Entity,
    /// Signature of the registry snapshot used for the current materialised
    /// occurrence.
    pub current_registry_signature: u64,
}

/// Requested definition + registry snapshot for the preview render target.
///
/// The Definitions browser can preview document definitions, bundled-library
/// definitions, and unsaved drafts. Those are not always present in the live
/// document [`DefinitionRegistry`], so the egui layer submits the exact
/// preview registry to render against.
#[derive(Resource, Debug, Clone, Default)]
pub struct DefinitionPreviewTarget {
    pub definition_id: Option<DefinitionId>,
    pub registry: DefinitionRegistry,
    pub registry_signature: u64,
}

impl DefinitionPreviewTarget {
    pub fn request(&mut self, definition_id: DefinitionId, registry: DefinitionRegistry) {
        let registry_signature = registry_signature(&registry);
        self.definition_id = Some(definition_id);
        self.registry = registry;
        self.registry_signature = registry_signature;
    }

    pub fn clear(&mut self) {
        self.definition_id = None;
        self.registry = DefinitionRegistry::default();
        self.registry_signature = 0;
    }
}

/// PP-DBUX4: NDC-space coordinates of a click on the preview image, pending
/// resolution by [`resolve_preview_click`].
///
/// The egui draw system writes `Some(ndc)` when the user clicks the preview
/// image.  The follow-up Bevy system reads and clears the value in the same
/// frame, updating `DefinitionsWindowState::selected_node`.
///
/// NDC convention: X ∈ [-1, 1] left-to-right, Y ∈ [-1, 1] bottom-to-top
/// (standard clip space, matching `Camera::viewport_to_ndc` output).
#[derive(Resource, Debug, Default)]
pub struct PendingPreviewClick {
    /// Normalised device coordinates of the last un-processed click, or `None`
    /// if no click is pending.  Reset to `None` after `resolve_preview_click`
    /// consumes it.
    pub ndc: Option<Vec2>,
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin that owns the definition preview render-target pipeline.
///
/// Register this in `main.rs` alongside the other core plugins.
pub struct DefinitionPreviewPlugin;

impl Plugin for DefinitionPreviewPlugin {
    fn build(&self, app: &mut App) {
        app
            // Register the custom gizmo group so Bevy allocates its pipeline.
            .init_gizmo_group::<PreviewSelectionGizmos>()
            .init_resource::<DefinitionPreviewTarget>()
            .insert_resource(PendingPreviewClick::default())
            .add_systems(Startup, (setup_preview_scene, configure_preview_gizmos))
            .add_systems(Update, sync_preview_to_target)
            .add_systems(
                Update,
                tag_preview_generated_parts.after(EvaluationSet::Evaluate),
            )
            // PP-DBUX4: reset the frame-local hover target before egui draws
            // the property tree so that rows from a previous frame never
            // produce sticky highlights.
            .add_systems(Update, reset_hovered_node)
            // PP-DBUX4: draw per-slot wireframe highlights in the preview.
            .add_systems(Update, draw_preview_slot_highlight)
            // PP-DBUX4: consume a pending preview-image click and map it to a
            // property-tree selection.
            .add_systems(Update, resolve_preview_click);
    }
}

// ---------------------------------------------------------------------------
// Startup systems
// ---------------------------------------------------------------------------

/// Create the render-target image, preview camera, and directional light.
///
/// Runs once at app startup.  The occurrence root is not created here — it is
/// spawned / replaced by `sync_preview_to_target` whenever the requested
/// preview definition changes.
fn setup_preview_scene(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let size = Extent3d {
        width: PREVIEW_TEXTURE_SIZE,
        height: PREVIEW_TEXTURE_SIZE,
        depth_or_array_layers: 1,
    };

    let mut image = Image {
        texture_descriptor: TextureDescriptor {
            label: Some("definition_preview_render_target"),
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Bgra8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        ..default()
    };
    image.resize(size);
    let render_target = images.add(image);

    // ── Preview camera ────────────────────────────────────────────────────────
    // Perspective, looking at the origin from a fixed position on the
    // isometric-ish diagonal.  The camera framing system adjusts distance
    // based on the spawned generated parts.
    //
    // `order: -1` renders before the main camera so the texture is ready
    // when the egui frame samples it.
    // In Bevy 0.18, `RenderTarget` is a required component separate from
    // `Camera`.  Pass it as its own component in the spawn tuple.
    let camera_entity = commands
        .spawn((
            Camera3d::default(),
            Camera {
                order: -1,
                clear_color: ClearColorConfig::Custom(Color::srgb(0.12, 0.13, 0.15)),
                ..default()
            },
            RenderTarget::Image(render_target.clone().into()),
            // Look at origin from slightly above and to the right — a classic
            // three-quarter view.
            Transform::from_xyz(2.5, 2.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
            PREVIEW_RENDER_LAYER,
            PreviewOnly,
        ))
        .id();

    // ── Directional light for the preview layer ───────────────────────────────
    // A warm key light from above-right and a cold fill from the front-left
    // give good shading on extruded solids without requiring IBL setup.
    let light_entity = commands
        .spawn((
            DirectionalLight {
                color: Color::srgb(1.0, 0.97, 0.92),
                illuminance: 8_000.0,
                shadows_enabled: false,
                ..default()
            },
            Transform::from_rotation(Quat::from_euler(
                EulerRot::YXZ,
                std::f32::consts::FRAC_PI_4,        // 45° yaw
                -std::f32::consts::FRAC_PI_4 * 0.8, // 36° pitch down
                0.0,
            )),
            PREVIEW_RENDER_LAYER,
            PreviewOnly,
        ))
        .id();

    commands.insert_resource(DefinitionPreviewScene {
        current_definition_id: None,
        render_target,
        camera_entity,
        light_entity,
        occurrence_root: Entity::PLACEHOLDER,
        current_registry_signature: 0,
    });
}

/// Point the [`PreviewSelectionGizmos`] config at [`PREVIEW_RENDER_LAYER`].
///
/// Runs once after the gizmo plugin initialises.  Without this the highlight
/// wireframes would render on every camera, leaking into the main viewport.
fn configure_preview_gizmos(mut config_store: ResMut<GizmoConfigStore>) {
    let (config, _ext) = config_store.config_mut::<PreviewSelectionGizmos>();
    config.render_layers = PREVIEW_RENDER_LAYER;
    config.depth_bias = -0.1;
}

// ---------------------------------------------------------------------------
// Sync system
// ---------------------------------------------------------------------------

/// Observe the requested preview target and re-materialise the occurrence when
/// it changes.
///
/// Despawns all preview entities except the camera and directional light, then
/// renders a fresh synthetic occurrence against the submitted preview registry.
pub fn sync_preview_to_target(world: &mut World) {
    let (target_definition_id, preview_registry, registry_signature) = {
        let target = world.resource::<DefinitionPreviewTarget>();
        (
            target.definition_id.clone(),
            target.registry.clone(),
            target.registry_signature,
        )
    };

    // ── Check if we need to re-spawn ──────────────────────────────────────────
    let needs_respawn = {
        let scene = world.resource::<DefinitionPreviewScene>();
        scene.current_definition_id != target_definition_id
            || scene.current_registry_signature != registry_signature
    };

    if !needs_respawn {
        return;
    }

    // ── Despawn the old occurrence root and all its generated parts ───────────
    //
    // We keep the camera and light entities alive (they carry PreviewOnly but
    // must persist).  We despawn everything else with PreviewOnly.
    let (camera_entity, light_entity) = {
        let scene = world.resource::<DefinitionPreviewScene>();
        (scene.camera_entity, scene.light_entity)
    };

    let preview_entities: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, With<PreviewOnly>>();
        query
            .iter(world)
            .filter(|&e| e != camera_entity && e != light_entity)
            .collect()
    };
    for entity in preview_entities {
        if world.get_entity(entity).is_ok() {
            let _ = world.despawn(entity);
        }
    }

    // ── Spawn the new occurrence root (or leave blank if no definition) ───────
    let new_root = if let Some(def_id) = &target_definition_id {
        let definition_version = preview_registry
            .get(def_id)
            .map(|d| d.definition_version)
            .unwrap_or_default();

        let mut identity = OccurrenceIdentity::new(def_id.clone(), definition_version);
        identity.overrides = OverrideMap::default();

        match render_occurrence(
            world,
            &preview_registry,
            PREVIEW_ELEMENT_ID_SENTINEL,
            &identity,
            Transform::default(),
            Some("__preview_occurrence__"),
        ) {
            Ok(()) => tag_preview_entities(world),
            Err(error) => {
                warn!(
                    "Failed to render definition preview for '{}': {}",
                    def_id, error
                );
                Entity::PLACEHOLDER
            }
        }
    } else {
        Entity::PLACEHOLDER
    };

    // ── Update the resource ───────────────────────────────────────────────────
    let mut scene = world.resource_mut::<DefinitionPreviewScene>();
    scene.current_definition_id = target_definition_id;
    scene.occurrence_root = new_root;
    scene.current_registry_signature = registry_signature;
}

/// After the eval system runs, tag every freshly-spawned
/// `GeneratedOccurrencePart` owned by the preview occurrence with `PreviewOnly`
/// and the preview render layer.
///
/// This system runs every frame; it is cheap because it only iterates entities
/// that lack `PreviewOnly` and have `GeneratedOccurrencePart` owned by the
/// sentinel `ElementId`.
pub fn tag_preview_generated_parts(
    mut commands: Commands,
    untagged: Query<(Entity, &GeneratedOccurrencePart), Without<PreviewOnly>>,
) {
    for (entity, part) in &untagged {
        if part.owner == PREVIEW_ELEMENT_ID_SENTINEL {
            commands
                .entity(entity)
                .insert((PreviewOnly, PREVIEW_RENDER_LAYER));
        }
    }
}

fn tag_preview_entities(world: &mut World) -> Entity {
    let root =
        crate::plugins::commands::find_entity_by_element_id(world, PREVIEW_ELEMENT_ID_SENTINEL)
            .unwrap_or(Entity::PLACEHOLDER);
    if root != Entity::PLACEHOLDER {
        if let Ok(mut entity) = world.get_entity_mut(root) {
            entity.insert((PreviewOnly, PREVIEW_RENDER_LAYER, Visibility::Visible));
        }
    }

    let generated_parts: Vec<Entity> = {
        let mut query =
            world.query_filtered::<(Entity, &GeneratedOccurrencePart), Without<PreviewOnly>>();
        query
            .iter(world)
            .filter_map(|(entity, part)| {
                (part.owner == PREVIEW_ELEMENT_ID_SENTINEL).then_some(entity)
            })
            .collect()
    };
    for entity in generated_parts {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert((PreviewOnly, PREVIEW_RENDER_LAYER));
        }
    }

    root
}

// ---------------------------------------------------------------------------
// PP-DBUX4 — Selection echo systems
// ---------------------------------------------------------------------------

/// Reset the frame-local `hovered_node` to `None` at the start of each frame.
///
/// This must run before the egui property-tree draw so that stale hover state
/// from the previous frame is cleared before new hover state is written.
pub fn reset_hovered_node(mut state: ResMut<DefinitionsWindowState>) {
    state.hovered_node = None;
}

/// Draw selection-echo wireframe highlights in the preview for the currently
/// selected (and optionally hovered) slot.
///
/// * If `selected_node` is `Slot(id)` or `SlotParameterBinding { slot_id, .. }`,
///   all `GeneratedOccurrencePart`s with matching `slot_path` (or whose
///   `slot_path` starts with `id + "."`) are highlighted with
///   [`PREVIEW_HIGHLIGHT_COLOR`].
/// * If `hovered_node` is set to a different slot, a softer
///   [`PREVIEW_HOVER_COLOR`] pulse is drawn on top.
///
/// Gizmos are drawn through [`PreviewSelectionGizmos`], whose `render_layers`
/// is set to [`PREVIEW_RENDER_LAYER`] at startup, so they are only visible
/// through the preview camera.
pub fn draw_preview_slot_highlight(
    state: Res<DefinitionsWindowState>,
    preview_parts: Query<
        (
            &GeneratedOccurrencePart,
            &ProfileExtrusion,
            Option<&ShapeRotation>,
        ),
        With<PreviewOnly>,
    >,
    mut gizmos: Gizmos<PreviewSelectionGizmos>,
) {
    let selected_slot_id = state
        .selected_preview_slot_path
        .clone()
        .or_else(|| selected_slot_id(&state.selected_node));
    let hovered_slot_id = state
        .hovered_node
        .as_ref()
        .and_then(|n| selected_slot_id_from_node(n));

    for (part, extrusion, rotation) in &preview_parts {
        let rot = rotation.copied().unwrap_or_default().0;

        // Selection takes priority over hover.
        if let Some(sel_id) = &selected_slot_id {
            if slot_path_matches(&part.slot_path, sel_id) {
                draw_extrusion_wireframe_preview(
                    &mut gizmos,
                    extrusion,
                    rot,
                    PREVIEW_HIGHLIGHT_COLOR,
                );
                continue;
            }
        }

        // Hover pulse — only when the hovered slot differs from the selection.
        if let Some(hov_id) = &hovered_slot_id {
            if Some(hov_id.as_str()) != selected_slot_id.as_deref() {
                if slot_path_matches(&part.slot_path, hov_id) {
                    draw_extrusion_wireframe_preview(
                        &mut gizmos,
                        extrusion,
                        rot,
                        PREVIEW_HOVER_COLOR,
                    );
                }
            }
        }
    }
}

/// Consume a [`PendingPreviewClick`] stored by the egui draw pass and map it
/// to a `selected_node` update in [`DefinitionsWindowState`].
///
/// Ray-casts a ray from the preview camera against the AABB of every
/// `GeneratedOccurrencePart` with a [`ProfileExtrusion`] that is tagged
/// [`PreviewOnly`].  The closest hit wins; an empty-space click resets the
/// selection back to `DefinitionEditorNode::Definition`.
pub fn resolve_preview_click(
    mut pending: ResMut<PendingPreviewClick>,
    mut state: ResMut<DefinitionsWindowState>,
    scene: Res<DefinitionPreviewScene>,
    camera_query: Query<(&Camera, &GlobalTransform)>,
    preview_parts: Query<
        (
            &GeneratedOccurrencePart,
            &ProfileExtrusion,
            Option<&ShapeRotation>,
        ),
        With<PreviewOnly>,
    >,
) {
    let Some(ndc) = pending.ndc.take() else {
        return;
    };

    // Retrieve the preview camera.
    let Ok((camera, camera_transform)) = camera_query.get(scene.camera_entity) else {
        return;
    };

    // Convert NDC to a viewport position so we can call `viewport_to_world`.
    //
    // `viewport_to_world` expects a position in **logical viewport pixels**
    // (origin top-left).  The viewport for the preview camera is the full
    // 512×512 render target.
    //
    // NDC → pixel: pixel = (ndc * 0.5 + 0.5) * size, then flip Y.
    let half = PREVIEW_TEXTURE_SIZE as f32 * 0.5;
    let viewport_pos = Vec2::new(
        (ndc.x * 0.5 + 0.5) * PREVIEW_TEXTURE_SIZE as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * PREVIEW_TEXTURE_SIZE as f32,
    );

    let Ok(ray) = camera.viewport_to_world(camera_transform, viewport_pos) else {
        return;
    };

    // Intersect the ray with each part's AABB.
    let mut closest: Option<(f32, &GeneratedOccurrencePart)> = None;
    for (part, extrusion, rotation) in &preview_parts {
        let rot = rotation.copied().unwrap_or_default().0;
        let Some(bounds) = extrusion.bounds(rot) else {
            continue;
        };
        let aabb = Aabb3d {
            min: bounds.min.into(),
            max: bounds.max.into(),
        };
        if let Some(dist) = ray_aabb_intersection(ray.origin, *ray.direction, aabb) {
            if closest.is_none_or(|(best, _)| dist < best) {
                closest = Some((dist, part));
            }
        }
    }

    // Map the hit to a slot selection.
    if let Some((_, part)) = closest {
        // Use the top-level slot segment (everything up to the first '.').
        let slot_id =
            slot_segment_base(part.slot_path.split('.').next().unwrap_or(&part.slot_path))
                .to_string();
        state.selected_node = DefinitionEditorNode::Slot(slot_id);
        state.selected_preview_slot_path = Some(normalize_slot_path(&part.slot_path));
        state.technical_view_buffer.clear();
        state.technical_view_error = None;
    } else {
        // Empty-space click — fall back to root selection.
        state.selected_node = DefinitionEditorNode::Definition;
        state.selected_preview_slot_path = None;
        state.technical_view_buffer.clear();
        state.technical_view_error = None;
    }
    let _ = half; // suppress unused-variable warning (used in comment above)
}

// ---------------------------------------------------------------------------
// egui UI helper
// ---------------------------------------------------------------------------

/// Render the definition's 3D occurrence preview inside an egui panel.
///
/// Call this from `draw_definition_editor` in place of `draw_definition_preview`.
/// When the scene has no current definition (e.g. draft not yet evaluated),
/// a small placeholder message is shown instead.
///
/// PP-DBUX4: if the user clicks the image, the normalised device coordinates of
/// the click are written to [`PendingPreviewClick`] so that
/// `resolve_preview_click` can map the click to a slot selection on the next
/// system run.
pub fn draw_definition_3d_preview(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    scene: &DefinitionPreviewScene,
    pending_click: &mut PendingPreviewClick,
    available_height: f32,
) {
    let width = ui.available_width().clamp(220.0, 360.0);

    egui::Frame::new()
        .fill(egui::Color32::from_rgb(26, 30, 34))
        .corner_radius(6.0)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(64, 72, 78)))
        .show(ui, |ui| {
            ui.set_min_size(egui::vec2(width, available_height));
            ui.set_max_size(egui::vec2(width, available_height));

            if scene.current_definition_id.is_none() {
                // Blank placeholder — same wording as the old `draw_empty_definition_preview`.
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.add_space(available_height * 0.35);
                    ui.label(
                        egui::RichText::new("Add an evaluator to preview geometry")
                            .small()
                            .color(egui::Color32::from_rgb(165, 176, 184)),
                    );
                });
                return;
            }

            // Resolve the egui texture ID for the render-target image.
            let texture_id = contexts.image_id(&scene.render_target).unwrap_or_else(|| {
                contexts.add_image(EguiTextureHandle::Weak(scene.render_target.id()))
            });

            // PP-DBUX4: add Sense::click() so egui reports primary clicks on
            // the preview image.  Sense::hover() is included by default in
            // egui::Image interactions; we do NOT add Sense::drag() per the
            // PP-DBUX4 constraints.
            let response = ui
                .add(egui::Image::new((
                    texture_id,
                    egui::vec2(width, available_height),
                )))
                .interact(egui::Sense::click());

            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some(pointer_pos) = response.interact_pointer_pos() {
                    // Convert the screen-space click position to NDC relative
                    // to the preview image's egui rect.
                    //
                    // egui rect origin is top-left; NDC is centre-origin,
                    // X right, Y up.
                    let rect = response.rect;
                    let relative = pointer_pos - rect.left_top();
                    let norm = relative / rect.size(); // [0,1] top-left origin
                    let ndc = egui::Vec2::new(
                        norm.x * 2.0 - 1.0,
                        1.0 - norm.y * 2.0, // flip Y: egui Y down, NDC Y up
                    );
                    pending_click.ndc = Some(Vec2::new(ndc.x, ndc.y));
                }
            }
        });

    // Overlay label drawn after the frame so it is not clipped.
    let painter = ui.painter();
    let rect = ui.min_rect();
    painter.text(
        rect.left_top() + egui::vec2(10.0, 8.0),
        egui::Align2::LEFT_TOP,
        "Occurrence preview",
        egui::TextStyle::Small.resolve(ui.style()),
        egui::Color32::from_rgb(205, 214, 220),
    );
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Arc tessellation resolution for wireframe drawing — matches the value used
/// in `profile.rs` (`ARC_SEGMENTS_PER_CIRCLE = 32`).
const WIREFRAME_ARC_SEGMENTS: u32 = 32;

/// Draw an extrusion wireframe onto a [`Gizmos<PreviewSelectionGizmos>`].
///
/// Mirrors the logic in [`Primitive::draw_wireframe`] for
/// [`ProfileExtrusion`] but uses the custom gizmo group so the lines only
/// appear on the preview render layer.
fn draw_extrusion_wireframe_preview(
    gizmos: &mut Gizmos<PreviewSelectionGizmos>,
    extrusion: &ProfileExtrusion,
    rotation: Quat,
    color: Color,
) {
    let pts = extrusion.profile.tessellate(WIREFRAME_ARC_SEGMENTS);
    let half_h = extrusion.height * 0.5;
    let to_world = |p: bevy::math::Vec2, y: f32| -> Vec3 {
        extrusion.centre + rotation * Vec3::new(p.x, y, p.y)
    };
    let n = pts.len();
    for i in 0..n {
        let j = (i + 1) % n;
        gizmos.line(to_world(pts[i], -half_h), to_world(pts[j], -half_h), color);
        gizmos.line(to_world(pts[i], half_h), to_world(pts[j], half_h), color);
    }
    for p in &pts {
        gizmos.line(to_world(*p, -half_h), to_world(*p, half_h), color);
    }
}

/// Return the slot id from a `DefinitionEditorNode` if it is a slot variant.
fn selected_slot_id(node: &DefinitionEditorNode) -> Option<String> {
    selected_slot_id_from_node(node)
}

fn selected_slot_id_from_node(node: &DefinitionEditorNode) -> Option<String> {
    match node {
        DefinitionEditorNode::Slot(id) => Some(id.clone()),
        DefinitionEditorNode::SlotParameterBinding { slot_id, .. } => Some(slot_id.clone()),
        _ => None,
    }
}

fn registry_signature(registry: &DefinitionRegistry) -> u64 {
    let mut entries = registry.list();
    entries.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut hasher = DefaultHasher::new();
    for definition in entries {
        definition.id.hash(&mut hasher);
        definition.definition_version.hash(&mut hasher);
        if let Ok(serialized) = serde_json::to_string(definition) {
            serialized.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Return `true` if `slot_path` equals `slot_id` or is a child path of it.
///
/// A child path is one where the path starts with `slot_id + "."`, e.g.
/// `"glazing.left_pane"` is a child of `"glazing"`.
fn slot_path_matches(slot_path: &str, slot_id: &str) -> bool {
    slot_path_pattern_matches(slot_path, slot_id, true)
}

fn slot_path_pattern_matches(slot_path: &str, pattern: &str, allow_descendants: bool) -> bool {
    let actual_segments = slot_path.split('.').collect::<Vec<_>>();
    let pattern_segments = pattern.split('.').collect::<Vec<_>>();
    if pattern_segments.is_empty() || pattern_segments.len() > actual_segments.len() {
        return false;
    }
    for (actual, expected) in actual_segments.iter().zip(pattern_segments.iter()) {
        if slot_segment_base(actual) != *expected {
            return false;
        }
    }
    allow_descendants || actual_segments.len() == pattern_segments.len()
}

fn normalize_slot_path(slot_path: &str) -> String {
    slot_path
        .split('.')
        .map(slot_segment_base)
        .collect::<Vec<_>>()
        .join(".")
}

fn slot_segment_base(segment: &str) -> &str {
    segment.split('[').next().unwrap_or(segment)
}

/// Minimal slab AABB type used for the ray-AABB intersection.
struct Aabb3d {
    min: Vec3,
    max: Vec3,
}

/// Ray–AABB slab intersection.  Returns the entry `t` along `direction` (in
/// world units) if the ray hits the box, or `None` otherwise.
///
/// `direction` does not need to be normalised; `t` is parameterised in terms
/// of `direction`'s length.  The caller should compare `t` values to find the
/// nearest hit.
fn ray_aabb_intersection(origin: Vec3, direction: Vec3, aabb: Aabb3d) -> Option<f32> {
    let inv = Vec3::new(
        if direction.x.abs() > f32::EPSILON {
            1.0 / direction.x
        } else {
            f32::INFINITY
        },
        if direction.y.abs() > f32::EPSILON {
            1.0 / direction.y
        } else {
            f32::INFINITY
        },
        if direction.z.abs() > f32::EPSILON {
            1.0 / direction.z
        } else {
            f32::INFINITY
        },
    );

    let t1 = (aabb.min - origin) * inv;
    let t2 = (aabb.max - origin) * inv;

    let t_enter = t1.min(t2).max_element();
    let t_exit = t1.max(t2).min_element();

    if t_exit >= t_enter && t_exit >= 0.0 {
        Some(t_enter.max(0.0))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that `PREVIEW_RENDER_LAYER` is layer 1 — distinct from the
    /// default layer 0 used by all authored scene content.
    #[test]
    fn preview_render_layer_is_unique() {
        let default_layer = RenderLayers::layer(0);
        assert_ne!(
            PREVIEW_RENDER_LAYER, default_layer,
            "PREVIEW_RENDER_LAYER must differ from the default layer 0"
        );
        // Verify it is exactly layer 1.
        assert_eq!(
            PREVIEW_RENDER_LAYER,
            RenderLayers::layer(1),
            "PREVIEW_RENDER_LAYER must be layer 1"
        );
    }

    /// Assert that the sentinel ElementId is out of the normal allocator range.
    ///
    /// The allocator starts at 0; `u64::MAX - 1` is practically unreachable.
    #[test]
    fn preview_element_id_sentinel_is_out_of_normal_range() {
        // Normal IDs are allocated from 0 upward.  The sentinel must be well
        // above any value a real document would reach.
        assert!(
            PREVIEW_ELEMENT_ID_SENTINEL.0 > 1_000_000,
            "sentinel must be far above normal element id range"
        );
    }

    /// PP-DBUX4: `slot_path_matches` must return `true` only for the selected
    /// slot and its children, not for siblings or unrelated paths.
    ///
    /// Given two `GeneratedOccurrencePart`s with `slot_path` "left_glazing" and
    /// "right_glazing", and `selected_node = Slot("left_glazing")`, the match
    /// helper must return `true` only for the left one.
    #[test]
    fn slot_highlight_filters_by_slot_path() {
        assert!(
            slot_path_matches("left_glazing", "left_glazing"),
            "exact match must return true"
        );
        assert!(
            !slot_path_matches("right_glazing", "left_glazing"),
            "sibling slot must not match"
        );
        // Nested child of selected slot must also highlight.
        assert!(
            slot_path_matches("left_glazing.inner_frame", "left_glazing"),
            "child slot path must match"
        );
        assert!(
            slot_path_matches("muntins[0]", "muntins"),
            "collection instance must match its slot id"
        );
        assert!(
            slot_path_matches("muntins[0].bar", "muntins.bar"),
            "nested collection child path must match by slot segment"
        );
        // Prefix that is not a dot-separated parent must not match.
        assert!(
            !slot_path_matches("left_glazing_extra", "left_glazing"),
            "same-prefix but non-child path must not match"
        );
    }

    /// `ray_aabb_intersection` returns `Some` for a ray pointing directly at a
    /// unit box and `None` for one that misses.
    #[test]
    fn ray_aabb_hit_and_miss() {
        let aabb = Aabb3d {
            min: Vec3::new(-0.5, -0.5, -0.5),
            max: Vec3::new(0.5, 0.5, 0.5),
        };
        // Ray along +Z from z = -5, aimed at origin.
        let hit = ray_aabb_intersection(Vec3::new(0.0, 0.0, -5.0), Vec3::Z, aabb);
        assert!(hit.is_some(), "centre ray must hit the box");
        assert!((hit.unwrap() - 4.5).abs() < 1e-4, "entry t should be ~4.5");

        // Ray along +Z but offset in X so it misses.
        let aabb2 = Aabb3d {
            min: Vec3::new(-0.5, -0.5, -0.5),
            max: Vec3::new(0.5, 0.5, 0.5),
        };
        let miss = ray_aabb_intersection(Vec3::new(2.0, 0.0, -5.0), Vec3::Z, aabb2);
        assert!(miss.is_none(), "offset ray must miss the box");
    }

    // PP-DBUX3 followup: integration tests for persistence and selection
    // exclusion cannot be written in isolation because they require the
    // full CapabilityRegistry + eval pipeline to be registered.  The
    // acceptance criteria for those paths are:
    //
    //   1. `build_project_file` in persistence.rs uses
    //      `world.query_filtered::<Entity, (With<ElementId>, Without<PreviewOnly>)>()`,
    //      so no entity with `PreviewOnly` can appear in the saved document.
    //
    //   2. `MeshSelectableQueryFilter` in selection.rs is
    //      `(Or<(With<ElementId>, With<GeneratedOccurrencePart>)>, Without<PreviewOnly>)`,
    //      so no preview entity can be selected by raycast or box-select.
    //
    //   3. `tag_preview_generated_parts` ensures every newly-spawned
    //      `GeneratedOccurrencePart` with `owner == PREVIEW_ELEMENT_ID_SENTINEL`
    //      receives `PreviewOnly`, closing the window between eval and tagging.
    //
    // A headless-render integration test fixture would require a full Bevy App
    // with the eval pipeline, StandardMaterial renderer, and WGPU backend.
    // That is tracked as a PP-DBUX4 followup when the full selection-echo
    // test harness is in place.
}
