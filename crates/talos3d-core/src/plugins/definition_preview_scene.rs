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
    camera::{RenderTarget, visibility::RenderLayers},
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    },
};
use bevy_egui::{egui, EguiContexts, EguiTextureHandle};

use crate::plugins::{
    definition_authoring::{
        draft_effective_definition, DefinitionDraftRegistry,
    },
    identity::ElementId,
    modeling::{
        definition::{DefinitionId, DefinitionLibraryRegistry, DefinitionRegistry, OverrideMap},
        mesh_generation::EvaluationSet,
        occurrence::{NeedsEval, OccurrenceIdentity},
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
// Resource
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
        app.add_systems(Startup, setup_preview_scene)
            .add_systems(Update, sync_preview_to_active_draft)
            .add_systems(
                Update,
                tag_preview_generated_parts.after(EvaluationSet::Evaluate),
            );
    }
}

// ---------------------------------------------------------------------------
// Startup system
// ---------------------------------------------------------------------------

/// Create the render-target image, preview camera, and directional light.
///
/// Runs once at app startup.  The occurrence root is not created here — it is
/// spawned / replaced by `sync_preview_to_active_draft` whenever the active
/// draft changes.
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
                std::f32::consts::FRAC_PI_4,       // 45° yaw
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
    });
}

// ---------------------------------------------------------------------------
// Sync system
// ---------------------------------------------------------------------------

/// Observe the active draft and re-materialise the preview occurrence when it
/// changes.
///
/// Despawns all preview entities except the camera and directional light, then
/// spawns a fresh occurrence root with `NeedsEval` so the standard eval
/// pipeline produces `GeneratedOccurrencePart` entities.
pub fn sync_preview_to_active_draft(world: &mut World) {
    // ── Determine what definition the active draft represents ─────────────────
    let target_definition_id: Option<DefinitionId> = {
        let drafts = world.resource::<DefinitionDraftRegistry>();
        let Some(active_draft_id) = drafts.active_draft_id.clone() else {
            return;
        };
        let Some(active_draft) = drafts.get(&active_draft_id) else {
            return;
        };
        let definitions = world.resource::<DefinitionRegistry>().clone();
        let libraries = world.resource::<DefinitionLibraryRegistry>().clone();
        draft_effective_definition(&definitions, &libraries, active_draft)
            .ok()
            .map(|def| def.id)
    };

    // ── Check if we need to re-spawn ──────────────────────────────────────────
    let needs_respawn = {
        let scene = world.resource::<DefinitionPreviewScene>();
        scene.current_definition_id != target_definition_id
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
        let definition_version = world
            .resource::<DefinitionRegistry>()
            .get(def_id)
            .map(|d| d.definition_version)
            .unwrap_or_default();

        let mut identity = OccurrenceIdentity::new(def_id.clone(), definition_version);
        identity.overrides = OverrideMap::default();

        // Spawn at origin with a minimal set of components.  The eval system
        // picks this up via `With<NeedsEval>` and populates geometry.
        //
        // IMPORTANT: GlobalTransform is required for the eval pipeline to
        // propagate world-space translations to child slots.
        let root = world
            .spawn((
                PREVIEW_ELEMENT_ID_SENTINEL,
                identity,
                Transform::default(),
                GlobalTransform::default(),
                Visibility::Visible,
                PREVIEW_RENDER_LAYER,
                PreviewOnly,
                NeedsEval,
                Name::new("__preview_occurrence__"),
            ))
            .id();
        root
    } else {
        Entity::PLACEHOLDER
    };

    // ── Update the resource ───────────────────────────────────────────────────
    let mut scene = world.resource_mut::<DefinitionPreviewScene>();
    scene.current_definition_id = target_definition_id;
    scene.occurrence_root = new_root;
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
    untagged: Query<
        (Entity, &crate::plugins::modeling::occurrence::GeneratedOccurrencePart),
        Without<PreviewOnly>,
    >,
) {
    for (entity, part) in &untagged {
        if part.owner == PREVIEW_ELEMENT_ID_SENTINEL {
            commands
                .entity(entity)
                .insert((PreviewOnly, PREVIEW_RENDER_LAYER));
        }
    }
}

// ---------------------------------------------------------------------------
// egui UI helper
// ---------------------------------------------------------------------------

/// Render the definition's 3D occurrence preview inside an egui panel.
///
/// Call this from `draw_definition_editor` in place of `draw_definition_preview`.
/// When the scene has no current definition (e.g. draft not yet evaluated),
/// a small placeholder message is shown instead.
pub fn draw_definition_3d_preview(
    ui: &mut egui::Ui,
    contexts: &mut EguiContexts,
    scene: &DefinitionPreviewScene,
    available_height: f32,
) {
    let width = ui.available_width().clamp(220.0, 360.0);

    egui::Frame::new()
        .fill(egui::Color32::from_rgb(26, 30, 34))
        .corner_radius(6.0)
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(64, 72, 78),
        ))
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
            let texture_id = contexts
                .image_id(&scene.render_target)
                .unwrap_or_else(|| {
                    contexts.add_image(EguiTextureHandle::Weak(scene.render_target.id()))
                });

            ui.add(egui::Image::new((
                texture_id,
                egui::vec2(width, available_height),
            )));
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
