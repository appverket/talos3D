//! Live 2D preview of the current [`DraftingSheet`].
//!
//! Reuses the exact same export path the user gets on disk: capture the
//! sheet from the active 3D orthographic view, rasterise through
//! `sheet_to_png`, upload the resulting bytes as an egui texture, and
//! show it in a floating resizable egui window.
//!
//! Why this over a `Camera2d` + gizmo pane:
//!
//! * No new render layers, no custom `GizmoConfigGroup`, no coordinate
//!   mirroring. The preview is *definitionally* what the exporter
//!   emits — if the preview is right, the export is right, and vice
//!   versa.
//! * egui is already wired into the chrome, and its `TextureHandle`
//!   flow is the standard way to show arbitrary raster content.
//! * Capture + raster takes a couple of milliseconds on the kinds of
//!   scenes that benefit from the preview; we rate-limit to one update
//!   every `REFRESH_INTERVAL` so editing the 3D scene stays snappy.

use std::time::Duration;

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use serde_json::Value;

use crate::plugins::{
    command_registry::{
        CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
    },
    drafting::DraftingVisibility,
    drawing_export::DRAFTING_CAPABILITY_ID,
};

use super::{
    capture::{capture_sheet, sheet_view_from_active_camera},
    export_png::sheet_to_png,
    sheet::DraftingSheet,
    DEFAULT_MARGIN_MM, DEFAULT_SCALE_DENOMINATOR,
};

/// DPI used to rasterise the preview. Lower than the export default so
/// the resulting texture fits comfortably inside typical GPU texture
/// size limits (≤ 2048 px on a ~260 mm wide A4-landscape sheet).
const PREVIEW_DPI: f32 = 120.0;
/// Upper bound on any dimension of the preview bitmap, as a defence
/// against extreme sheet sizes. 1536 × 1536 is safely below the 2048
/// min-guaranteed texture size reported by wgpu/Metal.
const PREVIEW_MAX_PX: u32 = 1536;

/// How often the preview re-captures + re-rasterises. Capture + PNG
/// encode + texture upload is cheap (~a few ms) on small scenes, but
/// we still want generous headroom for the editor to stay at 60 FPS
/// while the preview is live. A follow-up can swap this for an
/// on-dirty refresh that observes the source components directly.
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Default window size when the preview first opens.
const DEFAULT_WINDOW_SIZE: [f32; 2] = [420.0, 300.0];

pub struct DraftingSheetPreviewPlugin;

impl Plugin for DraftingSheetPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SheetPreviewState>()
            .register_command(
                CommandDescriptor {
                    id: "drafting.toggle_sheet_preview".to_string(),
                    label: "Sheet preview".to_string(),
                    description: "Toggle a live 2D preview of the current \
                                  drafting sheet derived from the active \
                                  orthographic view."
                        .to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.dimensions".to_string()),
                    hint: Some(
                        "Show a floating window with the captured drafting sheet"
                            .to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                execute_toggle_sheet_preview,
            )
            .add_systems(
                Update,
                (refresh_preview_sheet, show_preview_egui_window).chain(),
            );
    }
}

fn execute_toggle_sheet_preview(world: &mut World, _args: &Value) -> Result<CommandResult, String> {
    if let Some(mut state) = world.get_resource_mut::<SheetPreviewState>() {
        state.enabled = !state.enabled;
    }
    Ok(CommandResult::empty())
}

/// Latest rasterised sheet, plus the egui texture it's uploaded to.
#[derive(Resource, Default)]
pub struct SheetPreviewState {
    /// Whether the floating preview window is visible. The user flips
    /// this via the egui window's close button; tools that don't care
    /// can leave it at its default (off).
    pub enabled: bool,
    /// Time of the last refresh. Used to rate-limit.
    last_refresh: Option<Duration>,
    /// Most recent capture, kept around for debugging and for
    /// downstream tools that want the live sheet.
    pub sheet: Option<DraftingSheet>,
    /// Cached PNG bytes for the most recent sheet — re-uploaded to the
    /// egui texture when they change.
    png_bytes: Vec<u8>,
    /// Dimensions of the cached PNG, so the window can layout without
    /// first having to decode.
    png_dims: Option<[usize; 2]>,
    /// egui texture handle for the preview image.
    texture: Option<egui::TextureHandle>,
}

fn refresh_preview_sheet(world: &mut World) {
    let enabled = world
        .get_resource::<SheetPreviewState>()
        .is_some_and(|s| s.enabled);
    if !enabled {
        return;
    }

    let now = world
        .get_resource::<Time<Real>>()
        .map(|t| t.elapsed())
        .unwrap_or(Duration::ZERO);
    let should_refresh = world
        .get_resource::<SheetPreviewState>()
        .and_then(|s| s.last_refresh)
        .is_none_or(|last| now.saturating_sub(last) >= REFRESH_INTERVAL);
    if !should_refresh {
        return;
    }

    let Some(view) = sheet_view_from_active_camera(
        world,
        DEFAULT_SCALE_DENOMINATOR,
        DEFAULT_MARGIN_MM,
    ) else {
        // Non-orthographic camera or no scene → drop the cached sheet.
        if let Some(mut state) = world.get_resource_mut::<SheetPreviewState>() {
            state.sheet = None;
            state.png_bytes.clear();
            state.png_dims = None;
            state.texture = None;
            state.last_refresh = Some(now);
        }
        return;
    };

    let visibility_on = world
        .get_resource::<DraftingVisibility>()
        .map(|v| v.show_all)
        .unwrap_or(true);
    if !visibility_on {
        return;
    }

    let Some(sheet) = capture_sheet(world, &view) else {
        return;
    };

    // Shrink DPI further if the sheet would still exceed the GPU
    // texture cap. Keeps the preview image inside the 2048-px-per-side
    // envelope Metal/WGPU guarantee.
    let mut dpi = PREVIEW_DPI;
    let mm_to_px = |mm: f32, d: f32| (mm * d / 25.4).ceil() as u32;
    let w = sheet.bounds.width();
    let h = sheet.bounds.height();
    let max_dim = mm_to_px(w.max(h), dpi);
    if max_dim > PREVIEW_MAX_PX {
        dpi *= PREVIEW_MAX_PX as f32 / max_dim as f32;
    }

    let png = sheet_to_png(&sheet, dpi);
    let dims = image::load_from_memory(&png)
        .ok()
        .map(|img| [img.width() as usize, img.height() as usize]);

    if let Some(mut state) = world.get_resource_mut::<SheetPreviewState>() {
        state.sheet = Some(sheet);
        state.png_bytes = png;
        state.png_dims = dims;
        state.texture = None; // force re-upload on next egui pass
        state.last_refresh = Some(now);
    }
}

fn show_preview_egui_window(
    mut contexts: EguiContexts,
    mut state: ResMut<SheetPreviewState>,
) {
    if !state.enabled {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();

    // Lazily upload the PNG to an egui texture.
    if state.texture.is_none() {
        if let (Some([w, h]), Ok(img)) = (
            state.png_dims,
            image::load_from_memory(&state.png_bytes),
        ) {
            let rgba = img.to_rgba8();
            let pixels: Vec<u8> = rgba.into_raw();
            let colour_image = egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels);
            let handle = ctx.load_texture(
                "talos3d-drafting-sheet-preview",
                colour_image,
                egui::TextureOptions::LINEAR,
            );
            state.texture = Some(handle);
        }
    }

    let mut open = state.enabled;
    egui::Window::new("Drafting sheet preview")
        .open(&mut open)
        .default_size(DEFAULT_WINDOW_SIZE)
        .resizable(true)
        .vscroll(false)
        .hscroll(false)
        .show(&ctx, |ui| {
            let Some(texture) = state.texture.as_ref() else {
                ui.label("Capturing…");
                return;
            };
            let [w_px, h_px] = state.png_dims.unwrap_or([1, 1]);
            let aspect = if h_px > 0 {
                w_px as f32 / h_px as f32
            } else {
                1.0
            };
            let avail = ui.available_size();
            let w = avail.x.max(1.0);
            let h = (w / aspect.max(0.01)).min(avail.y).max(1.0);
            let display_w = if h * aspect <= avail.x { h * aspect } else { w };
            ui.image((texture.id(), egui::Vec2::new(display_w, display_w / aspect.max(0.01))));
        });
    state.enabled = open;
}
