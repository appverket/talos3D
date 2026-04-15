use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use base64::Engine;
use bevy::{
    prelude::*,
    render::{render_resource::TextureFormat, view::screenshot::Screenshot},
};
use image::{codecs::jpeg::JpegEncoder, DynamicImage, RgbImage};
use serde_json::Value;

use crate::capability_registry::{
    CapabilityDescriptor, CapabilityDistribution, CapabilityMaturity, CapabilityRegistryAppExt,
};
use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    document_state::DocumentState,
    ui::StatusBarData,
    vector_drawing,
};

const STATUS_MESSAGE_DURATION_SECONDS: f32 = 2.0;
const DEFAULT_EXPORT_FILE_STEM: &str = "drawing";

/// Capability id for the "2D drafting" extension that surfaces vector-paper
/// output (PDF/SVG) in the UI.
///
/// PNG export stays global — rasterising any viewport is always meaningful.
/// Vector formats are gated because they only make architectural sense when
/// the user is producing a 2D drafting artefact (plans, sections, elevations
/// with dimensions and line-weight conventions); from a shaded 3D view they
/// degrade to "raster wrapped in a PDF envelope", misleading the user about
/// what they will get.
pub const DRAFTING_CAPABILITY_ID: &str = "drafting";

pub struct DrawingExportPlugin;

impl Plugin for DrawingExportPlugin {
    fn build(&self, app: &mut App) {
        app.register_capability(
            CapabilityDescriptor::new(DRAFTING_CAPABILITY_ID, "2D Drafting")
                .with_description(
                    "Vector-paper output and drafting annotations \
                     (PDF/SVG export, dimensions, guide lines). \
                     Turn on for plan/section/elevation work; leave off for \
                     pure 3D modelling.",
                )
                .with_distribution(CapabilityDistribution::Bundled)
                .with_maturity(CapabilityMaturity::Stable),
        )
        .register_command(
            CommandDescriptor {
                id: "core.export_drawing".to_string(),
                label: "Export Drawing...".to_string(),
                description: "Export the current drawing viewport as PNG, PDF, or SVG.".to_string(),
                category: CommandCategory::File,
                parameters: None,
                version: 1,
                default_shortcut: Some("Ctrl/Cmd+Shift+E".to_string()),
                icon: Some("icon.export".to_string()),
                hint: Some("Export the cropped drawing viewport as PNG, PDF, or SVG".to_string()),
                requires_selection: false,
                show_in_menu: true,
                activates_tool: None,
                capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
            },
            execute_export_drawing,
        )
        .register_command(
            CommandDescriptor {
                id: "core.export_drawing_png".to_string(),
                label: "Export Drawing as PNG...".to_string(),
                description: "Export the current drawing viewport as a PNG image.".to_string(),
                category: CommandCategory::File,
                parameters: None,
                version: 1,
                default_shortcut: None,
                icon: Some("icon.export".to_string()),
                hint: Some("Export the cropped drawing viewport as PNG".to_string()),
                requires_selection: false,
                show_in_menu: true,
                activates_tool: None,
                // PNG is always legitimate — rasterising a shaded 3D view
                // is not misleading, so leave it ungated.
                capability_id: None,
            },
            execute_export_drawing_png,
        )
        .register_command(
            CommandDescriptor {
                id: "core.export_drawing_pdf".to_string(),
                label: "Export Drawing as PDF...".to_string(),
                description: "Export the current drawing viewport as a PDF document.".to_string(),
                category: CommandCategory::File,
                parameters: None,
                version: 1,
                default_shortcut: None,
                icon: Some("icon.export".to_string()),
                hint: Some("Export the cropped drawing viewport as PDF".to_string()),
                requires_selection: false,
                show_in_menu: true,
                activates_tool: None,
                capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
            },
            execute_export_drawing_pdf,
        )
        .register_command(
            CommandDescriptor {
                id: "core.export_drawing_svg".to_string(),
                label: "Export Drawing as SVG...".to_string(),
                description: "Export the current drawing viewport as an SVG document.".to_string(),
                category: CommandCategory::File,
                parameters: None,
                version: 1,
                default_shortcut: None,
                icon: Some("icon.export".to_string()),
                hint: Some("Export the cropped drawing viewport as SVG".to_string()),
                requires_selection: false,
                show_in_menu: true,
                activates_tool: None,
                capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
            },
            execute_export_drawing_svg,
        )
        .register_command(
            CommandDescriptor {
                id: "core.export_drawing_dxf".to_string(),
                label: "Export Drawing as DXF...".to_string(),
                description: "Export the current drawing viewport as a DXF file (AC1027).".to_string(),
                category: CommandCategory::File,
                parameters: None,
                version: 1,
                default_shortcut: None,
                icon: Some("icon.export".to_string()),
                hint: Some("Export the cropped drawing viewport as DXF".to_string()),
                requires_selection: false,
                show_in_menu: true,
                activates_tool: None,
                capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
            },
            execute_export_drawing_dxf,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ViewportExportFormat {
    Raster(image::ImageFormat),
    Pdf,
    Svg,
    Dxf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewportCapture {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    window_width: f32,
    window_height: f32,
}

impl ViewportCapture {
    pub(crate) fn from_window_and_inset(
        window: &bevy::window::Window,
        inset: &crate::plugins::cursor::ViewportUiInset,
    ) -> Option<Self> {
        let window_width = window.width();
        let window_height = window.height();
        if window_width <= 0.0 || window_height <= 0.0 {
            return None;
        }

        let left = inset.left.clamp(0.0, window_width);
        let top = inset.top.clamp(0.0, window_height);
        let right = inset.right.clamp(0.0, window_width - left);
        let bottom = inset.bottom.clamp(0.0, window_height - top);
        let width = (window_width - left - right).max(1.0);
        let height = (window_height - top - bottom).max(1.0);

        Some(Self {
            left,
            top,
            width,
            height,
            window_width,
            window_height,
        })
    }

    pub(crate) fn image_bounds(
        self,
        image_width: u32,
        image_height: u32,
    ) -> Option<(u32, u32, u32, u32)> {
        if image_width == 0 || image_height == 0 {
            return None;
        }

        let scale_x = image_width as f32 / self.window_width.max(1.0);
        let scale_y = image_height as f32 / self.window_height.max(1.0);
        let left = (self.left * scale_x)
            .floor()
            .clamp(0.0, image_width as f32 - 1.0) as u32;
        let top = (self.top * scale_y)
            .floor()
            .clamp(0.0, image_height as f32 - 1.0) as u32;
        let right = ((self.left + self.width) * scale_x)
            .ceil()
            .clamp(left as f32 + 1.0, image_width as f32) as u32;
        let bottom = ((self.top + self.height) * scale_y)
            .ceil()
            .clamp(top as f32 + 1.0, image_height as f32) as u32;
        Some((left, top, right - left, bottom - top))
    }
}

#[cfg(feature = "model-api")]
pub(crate) fn default_drawing_export_path() -> String {
    "/tmp/talos_drawing.png".to_string()
}

pub(crate) fn capture_viewport(world: &World) -> Option<ViewportCapture> {
    let inset = world.get_resource::<crate::plugins::cursor::ViewportUiInset>()?;
    let mut window_query =
        world.try_query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>()?;
    let window = window_query.single(world).ok()?;
    ViewportCapture::from_window_and_inset(window, inset)
}

pub(crate) fn normalize_export_path(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension("png")
    }
}

pub(crate) fn viewport_export_format_from_path(
    path: &Path,
) -> Result<ViewportExportFormat, String> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    match extension.as_deref() {
        None => Ok(ViewportExportFormat::Raster(image::ImageFormat::Png)),
        Some("pdf") => Ok(ViewportExportFormat::Pdf),
        Some("svg") | Some("svd") => Ok(ViewportExportFormat::Svg),
        Some("dxf") => Ok(ViewportExportFormat::Dxf),
        _ => image::ImageFormat::from_path(path)
            .map(ViewportExportFormat::Raster)
            .map_err(|_| {
                format!(
                    "Unsupported export format for '{}'. Use PNG, PDF, SVG, or DXF.",
                    path.display()
                )
            }),
    }
}

pub fn export_drawing_now(world: &mut World) -> Result<Option<PathBuf>, String> {
    export_drawing_now_with_format(world, None)
}

pub(crate) fn export_drawing_now_with_format(
    world: &mut World,
    preferred_format: Option<ViewportExportFormat>,
) -> Result<Option<PathBuf>, String> {
    let current_path = world.resource::<DocumentState>().current_path.clone();
    let mut dialog = rfd::FileDialog::new();
    dialog = match preferred_format {
        Some(ViewportExportFormat::Raster(image::ImageFormat::Png)) => dialog
            .add_filter("PNG Image", &["png"])
            .set_file_name(default_export_file_name(
                current_path.as_deref(),
                Some(ViewportExportFormat::Raster(image::ImageFormat::Png)),
            )),
        Some(ViewportExportFormat::Pdf) => dialog
            .add_filter("PDF Document", &["pdf"])
            .set_file_name(default_export_file_name(
                current_path.as_deref(),
                Some(ViewportExportFormat::Pdf),
            )),
        Some(ViewportExportFormat::Svg) => dialog
            .add_filter("SVG Drawing", &["svg"])
            .set_file_name(default_export_file_name(
                current_path.as_deref(),
                Some(ViewportExportFormat::Svg),
            )),
        Some(ViewportExportFormat::Dxf) => dialog
            .add_filter("DXF Drawing", &["dxf"])
            .set_file_name(default_export_file_name(
                current_path.as_deref(),
                Some(ViewportExportFormat::Dxf),
            )),
        Some(ViewportExportFormat::Raster(other)) => dialog
            .add_filter("Drawing Export", &[other.extensions_str()[0]])
            .set_file_name(default_export_file_name(
                current_path.as_deref(),
                Some(ViewportExportFormat::Raster(other)),
            )),
        None => dialog
            .add_filter("Drawing Export", &["png", "pdf", "svg", "dxf"])
            .add_filter("PNG Image", &["png"])
            .add_filter("PDF Document", &["pdf"])
            .add_filter("SVG Drawing", &["svg"])
            .add_filter("DXF Drawing", &["dxf"])
            .set_file_name(default_export_file_name(current_path.as_deref(), None)),
    };
    if let Some(ref path) = current_path {
        if let Some(parent) = path.parent() {
            dialog = dialog.set_directory(parent);
        }
    }

    match dialog.save_file() {
        Some(path) => {
            let path = export_drawing_to_path(world, path)?;
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

pub fn export_drawing_to_path(world: &mut World, path: PathBuf) -> Result<PathBuf, String> {
    let path = normalize_export_path(path);
    let format = viewport_export_format_from_path(&path)?;
    match format {
        ViewportExportFormat::Svg | ViewportExportFormat::Pdf | ViewportExportFormat::Dxf => {
            export_vector_drawing(world, &path, format)?;
        }
        ViewportExportFormat::Raster(_) => {
            queue_viewport_export(world, &path)?;
        }
    }
    Ok(path)
}

fn export_vector_drawing(
    world: &World,
    path: &Path,
    format: ViewportExportFormat,
) -> Result<(), String> {
    let drawing = vector_drawing::extract_drawing_geometry(world)
        .ok_or_else(|| "Cannot extract drawing geometry — is a camera active?".to_string())?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let bytes = match format {
        ViewportExportFormat::Svg => vector_drawing::drawing_to_svg(&drawing),
        ViewportExportFormat::Pdf => vector_drawing::drawing_to_pdf(&drawing),
        ViewportExportFormat::Dxf => vector_drawing::drawing_to_dxf(&drawing).into_bytes(),
        ViewportExportFormat::Raster(_) => unreachable!(),
    };

    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

pub(crate) fn queue_viewport_export(world: &mut World, path: &Path) -> Result<(), String> {
    let _ = viewport_export_format_from_path(path)?;
    let viewport_capture = capture_viewport(world);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    world
        .commands()
        .spawn(Screenshot::primary_window())
        .observe(save_viewport_export_to_disk(
            path.to_path_buf(),
            viewport_capture,
        ));
    world.flush();
    Ok(())
}

pub(crate) fn save_viewport_export_to_disk(
    path: PathBuf,
    viewport_capture: Option<ViewportCapture>,
) -> impl FnMut(bevy::prelude::On<bevy::render::view::screenshot::ScreenshotCaptured>) {
    move |screenshot_captured| {
        let img = screenshot_captured.image.clone();
        let source_format = img.texture_descriptor.format;
        match img.try_into_dynamic() {
            Ok(dynamic) => {
                let mut rgb = crop_dynamic_to_viewport(dynamic, viewport_capture);
                if is_linear_texture_format(source_format) {
                    encode_linear_to_srgb(&mut rgb);
                }
                if let Err(error) = write_rgb_to_path(&path, &rgb) {
                    error!("Cannot save drawing export '{}': {error}", path.display());
                }
            }
            Err(error) => {
                error!("Cannot save drawing export, screen format cannot be understood: {error}");
            }
        }
    }
}

/// Whether the captured swapchain pixels are already in the sRGB transfer
/// function. Linear formats store gamma-decoded values and must be re-encoded
/// before being handed to a viewer that expects sRGB.
fn is_linear_texture_format(format: TextureFormat) -> bool {
    matches!(
        format,
        TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm
    )
}

/// Apply the sRGB OETF (linear → encoded) to every channel of `rgb`.
///
/// Bevy's screenshot system returns the raw swapchain bytes. On surfaces whose
/// format lacks the `Srgb` suffix (e.g. `Bgra8Unorm`), those bytes are still in
/// the linear working space the tonemapper wrote, so saving them to a PNG
/// without encoding produces an image that looks roughly two stops darker than
/// what the display shows. Image viewers (and the `image` crate's PNG encoder)
/// assume 8-bit RGB is sRGB, so we apply the standard OETF here.
fn encode_linear_to_srgb(rgb: &mut RgbImage) {
    let table = build_linear_to_srgb_lut();
    for pixel in rgb.pixels_mut() {
        pixel[0] = table[pixel[0] as usize];
        pixel[1] = table[pixel[1] as usize];
        pixel[2] = table[pixel[2] as usize];
    }
}

fn build_linear_to_srgb_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let linear = (i as f32) / 255.0;
        let srgb = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        *slot = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

fn crop_dynamic_to_viewport(
    dynamic: DynamicImage,
    viewport_capture: Option<ViewportCapture>,
) -> RgbImage {
    let mut rgb = dynamic.to_rgb8();
    if let Some(bounds) =
        viewport_capture.and_then(|capture| capture.image_bounds(rgb.width(), rgb.height()))
    {
        rgb = image::imageops::crop_imm(&rgb, bounds.0, bounds.1, bounds.2, bounds.3).to_image();
    }
    rgb
}

fn write_rgb_to_path(path: &Path, rgb: &RgbImage) -> Result<(), String> {
    match viewport_export_format_from_path(path)? {
        ViewportExportFormat::Raster(format) => rgb
            .save_with_format(path, format)
            .map_err(|error| error.to_string()),
        ViewportExportFormat::Svg => {
            let document = svg_document(rgb)?;
            std::fs::write(path, document).map_err(|error| error.to_string())
        }
        ViewportExportFormat::Pdf => {
            let document = pdf_document(rgb)?;
            std::fs::write(path, document).map_err(|error| error.to_string())
        }
        ViewportExportFormat::Dxf => {
            // Fallback when we only have a raster: DXF has no sensible raster
            // encoding, so emit an empty DXF with the drawing extents so
            // downstream consumers see something valid.
            let stub = crate::plugins::drafting::export_dxf(
                crate::plugins::drafting::DxfUnit::Millimetres,
                (0.0, 0.0),
                (rgb.width() as f32, rgb.height() as f32),
                &[],
                &[],
            );
            std::fs::write(path, stub).map_err(|error| error.to_string())
        }
    }
}

fn svg_document(rgb: &RgbImage) -> Result<Vec<u8>, String> {
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(rgb.clone())
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    Ok(format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n",
            "  <rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n",
            "  <image href=\"data:image/png;base64,{encoded}\" width=\"{width}\" height=\"{height}\" preserveAspectRatio=\"none\"/>\n",
            "</svg>\n"
        ),
        width = rgb.width(),
        height = rgb.height(),
        encoded = encoded
    )
    .into_bytes())
}

fn pdf_document(rgb: &RgbImage) -> Result<Vec<u8>, String> {
    let width = rgb.width();
    let height = rgb.height();

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 95)
        .encode_image(rgb)
        .map_err(|error| error.to_string())?;

    let content_stream = format!("q\n{width} 0 0 {height} 0 0 cm\n/Im0 Do\nQ\n");
    let image_length = jpeg.len();

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xC7\xEC\x8F\xA2\n");
    let mut offsets = Vec::new();

    write_pdf_object(
        &mut out,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    )?;
    write_pdf_object(
        &mut out,
        &mut offsets,
        2,
        format!("<< /Type /Pages /Kids [3 0 R] /Count 1 >>").as_bytes(),
    )?;
    write_pdf_object(
        &mut out,
        &mut offsets,
        3,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"
        )
        .as_bytes(),
    )?;

    offsets.push(out.len());
    write!(
        out,
        "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {image_length} >>\nstream\n"
    )
    .map_err(|error| error.to_string())?;
    out.extend_from_slice(&jpeg);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    write_pdf_object(
        &mut out,
        &mut offsets,
        5,
        format!(
            "<< /Length {} >>\nstream\n{}endstream",
            content_stream.len(),
            content_stream
        )
        .as_bytes(),
    )?;

    let xref_offset = out.len();
    write!(out, "xref\n0 {}\n", offsets.len() + 1).map_err(|error| error.to_string())?;
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        writeln!(out, "{offset:010} 00000 n ").map_err(|error| error.to_string())?;
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        6, xref_offset
    )
    .map_err(|error| error.to_string())?;

    Ok(out)
}

fn write_pdf_object(
    out: &mut Vec<u8>,
    offsets: &mut Vec<usize>,
    object_id: usize,
    content: &[u8],
) -> Result<(), String> {
    offsets.push(out.len());
    write!(out, "{object_id} 0 obj\n").map_err(|error| error.to_string())?;
    out.extend_from_slice(content);
    out.extend_from_slice(b"\nendobj\n");
    Ok(())
}

fn default_export_file_stem(current_path: Option<&Path>) -> String {
    current_path
        .and_then(|path| path.file_stem())
        .map(|stem| format!("{}-drawing", stem.to_string_lossy()))
        .unwrap_or_else(|| DEFAULT_EXPORT_FILE_STEM.to_string())
}

fn default_export_file_name(
    current_path: Option<&Path>,
    preferred_format: Option<ViewportExportFormat>,
) -> String {
    let stem = default_export_file_stem(current_path);
    match preferred_format {
        Some(ViewportExportFormat::Raster(image::ImageFormat::Png)) => format!("{stem}.png"),
        Some(ViewportExportFormat::Pdf) => format!("{stem}.pdf"),
        Some(ViewportExportFormat::Svg) => format!("{stem}.svg"),
        Some(ViewportExportFormat::Dxf) => format!("{stem}.dxf"),
        Some(ViewportExportFormat::Raster(other)) => {
            let extension = other.extensions_str().first().copied().unwrap_or("png");
            format!("{stem}.{extension}")
        }
        None => stem,
    }
}

fn execute_export_drawing(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let Some(path) = export_drawing_now(world)? else {
        return Ok(CommandResult::empty());
    };
    set_export_feedback(world, &path);
    Ok(CommandResult::empty())
}

fn execute_export_drawing_png(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let Some(path) = export_drawing_now_with_format(
        world,
        Some(ViewportExportFormat::Raster(image::ImageFormat::Png)),
    )?
    else {
        return Ok(CommandResult::empty());
    };
    set_export_feedback(world, &path);
    Ok(CommandResult::empty())
}

fn execute_export_drawing_pdf(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let Some(path) = export_drawing_now_with_format(world, Some(ViewportExportFormat::Pdf))? else {
        return Ok(CommandResult::empty());
    };
    set_export_feedback(world, &path);
    Ok(CommandResult::empty())
}

fn execute_export_drawing_svg(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let Some(path) = export_drawing_now_with_format(world, Some(ViewportExportFormat::Svg))? else {
        return Ok(CommandResult::empty());
    };
    set_export_feedback(world, &path);
    Ok(CommandResult::empty())
}

fn execute_export_drawing_dxf(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let Some(path) = export_drawing_now_with_format(world, Some(ViewportExportFormat::Dxf))? else {
        return Ok(CommandResult::empty());
    };
    set_export_feedback(world, &path);
    Ok(CommandResult::empty())
}

fn set_export_feedback(world: &mut World, path: &Path) {
    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(
            format!("Exporting drawing to {}", path.display()),
            STATUS_MESSAGE_DURATION_SECONDS,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn viewport_capture_maps_ui_insets_to_image_bounds() {
        let window = bevy::window::Window {
            resolution: bevy::window::WindowResolution::new(1280, 720),
            ..default()
        };
        let inset = crate::plugins::cursor::ViewportUiInset {
            top: 52.0,
            right: 300.0,
            bottom: 24.0,
            left: 48.0,
        };
        let capture = ViewportCapture::from_window_and_inset(&window, &inset)
            .expect("capture should be derived from window metrics");

        assert_eq!(
            capture.image_bounds(2560, 1440),
            Some((96, 104, 1864, 1288))
        );
    }

    #[test]
    fn export_format_defaults_to_png_and_accepts_svg_aliases() {
        assert_eq!(
            viewport_export_format_from_path(Path::new("/tmp/drawing")),
            Ok(ViewportExportFormat::Raster(image::ImageFormat::Png))
        );
        assert_eq!(
            viewport_export_format_from_path(Path::new("/tmp/drawing.png")),
            Ok(ViewportExportFormat::Raster(image::ImageFormat::Png))
        );
        assert_eq!(
            viewport_export_format_from_path(Path::new("/tmp/drawing.pdf")),
            Ok(ViewportExportFormat::Pdf)
        );
        assert_eq!(
            viewport_export_format_from_path(Path::new("/tmp/drawing.svg")),
            Ok(ViewportExportFormat::Svg)
        );
        assert_eq!(
            viewport_export_format_from_path(Path::new("/tmp/drawing.svd")),
            Ok(ViewportExportFormat::Svg)
        );
    }

    #[test]
    fn generic_export_file_name_uses_extensionless_stem() {
        assert_eq!(default_export_file_name(None, None), "drawing");
        assert_eq!(
            default_export_file_name(Some(Path::new("/tmp/model.talos3d")), None),
            "model-drawing"
        );
    }

    #[test]
    fn format_specific_export_file_names_use_requested_extension() {
        assert_eq!(
            default_export_file_name(
                Some(Path::new("/tmp/model.talos3d")),
                Some(ViewportExportFormat::Pdf)
            ),
            "model-drawing.pdf"
        );
        assert_eq!(
            default_export_file_name(
                Some(Path::new("/tmp/model.talos3d")),
                Some(ViewportExportFormat::Svg)
            ),
            "model-drawing.svg"
        );
        assert_eq!(
            default_export_file_name(
                Some(Path::new("/tmp/model.talos3d")),
                Some(ViewportExportFormat::Raster(image::ImageFormat::Png))
            ),
            "model-drawing.png"
        );
    }

    #[test]
    fn svg_document_embeds_png_payload() {
        let rgb = RgbImage::from_pixel(4, 3, Rgb([255, 255, 255]));
        let svg = String::from_utf8(svg_document(&rgb).expect("svg export should render"))
            .expect("svg export should be utf-8");

        assert!(svg.contains("<svg"));
        assert!(svg.contains("width=\"4\""));
        assert!(svg.contains("height=\"3\""));
        assert!(svg.contains("data:image/png;base64,"));
    }

    #[test]
    fn pdf_document_embeds_single_raster_page() {
        let rgb = RgbImage::from_pixel(4, 3, Rgb([240, 240, 240]));
        let pdf = pdf_document(&rgb).expect("pdf export should render");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("%PDF-1.4"));
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/MediaBox [0 0 4 3]"));
        assert!(text.contains("/Filter /DCTDecode"));
        assert!(text.contains("startxref"));
    }

    #[test]
    fn linear_to_srgb_lut_matches_reference_points() {
        let lut = build_linear_to_srgb_lut();
        // 0 and 255 must be preserved (endpoints).
        assert_eq!(lut[0], 0);
        assert_eq!(lut[255], 255);
        // Mid-grey in linear (0.5) should map to roughly 0.735 in sRGB
        // (standard OETF reference value) — i.e. 187 ± 1.
        let mid = lut[128];
        assert!(
            (186..=188).contains(&mid),
            "expected mid-grey ~187, got {mid}"
        );
    }

    #[test]
    fn encode_linear_to_srgb_brightens_midtones() {
        let mut rgb = RgbImage::from_pixel(1, 1, Rgb([64, 64, 64]));
        encode_linear_to_srgb(&mut rgb);
        let pixel = rgb.get_pixel(0, 0);
        // Linear 64/255 ≈ 0.251 encodes to ~0.539 in sRGB ≈ 137.
        assert!(
            (135..=139).contains(&pixel[0]),
            "expected ~137 after sRGB encoding, got {}",
            pixel[0]
        );
    }

    #[test]
    fn linear_format_detection_matches_wgpu_conventions() {
        assert!(is_linear_texture_format(TextureFormat::Rgba8Unorm));
        assert!(is_linear_texture_format(TextureFormat::Bgra8Unorm));
        assert!(!is_linear_texture_format(TextureFormat::Rgba8UnormSrgb));
        assert!(!is_linear_texture_format(TextureFormat::Bgra8UnormSrgb));
    }
}
