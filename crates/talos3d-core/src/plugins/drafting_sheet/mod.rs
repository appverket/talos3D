//! PP69 — DraftingSheet: a 2D paper-native drawing document derived from
//! a 3D view.
//!
//! See `private/docs/proof_points/PROOF_POINT_69.md`.
//!
//! Public surface:
//!
//! - [`DraftingSheet`], [`SheetView`], [`SheetBounds`], [`SheetLine`],
//!   [`SheetHatch`], [`SheetStroke`] — the sheet data model, all in
//!   paper millimetres.
//! - [`capture_sheet`] — flatten a 3D world into a paper-mm sheet.
//! - [`sheet_to_svg`], [`sheet_to_pdf`], [`sheet_to_dxf`],
//!   [`sheet_to_png`] — writers that consume a sheet.
//! - [`export_sheet_to_path`] — convenience: capture current camera,
//!   decide format from the path extension, write. Used by the MCP tool
//!   `export_drafting_sheet`.

use std::path::PathBuf;

use bevy::prelude::World;

pub mod capture;
pub mod export_dxf;
pub mod export_pdf;
pub mod export_png;
pub mod export_svg;
pub mod preview;
pub mod sheet;

pub use capture::{capture_sheet, sheet_view_from_active_camera};
pub use export_dxf::sheet_to_dxf;
pub use export_pdf::sheet_to_pdf;
pub use export_png::sheet_to_png;
pub use export_svg::sheet_to_svg;
pub use preview::{DraftingSheetPreviewPlugin, SheetPreviewState};
pub use sheet::{
    DraftingSheet, SheetBounds, SheetHatch, SheetLine, SheetStroke, SheetView,
};

/// Default architectural drawing scale used by [`export_sheet_to_path`]
/// when the caller does not specify one. `1:50` is the common choice for
/// a single-room / small-house elevation or section in metric arch
/// practice.
pub const DEFAULT_SCALE_DENOMINATOR: f32 = 50.0;
/// Default paper margin around the captured bounds (mm).
pub const DEFAULT_MARGIN_MM: f32 = 10.0;
/// Default raster DPI for [`sheet_to_png`] output.
pub const DEFAULT_PNG_DPI: f32 = 200.0;

/// One-call export: capture the current orthographic camera into a
/// paper-mm sheet at `scale_denominator` (or [`DEFAULT_SCALE_DENOMINATOR`]
/// if `None`), choose the writer by file extension, and write bytes to
/// `path`. Returns the path on success, an error string otherwise.
pub fn export_sheet_to_path(
    world: &World,
    path: PathBuf,
    scale_denominator: Option<f32>,
) -> Result<PathBuf, String> {
    let path = normalize_path(path);
    let scale = scale_denominator.unwrap_or(DEFAULT_SCALE_DENOMINATOR);
    let view = sheet_view_from_active_camera(world, scale, DEFAULT_MARGIN_MM)
        .ok_or_else(|| "no active orthographic camera — drafting requires an ortho view".to_string())?;
    let sheet = capture_sheet(world, &view)
        .ok_or_else(|| "sheet capture returned nothing (no visible geometry?)".to_string())?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let bytes = match ext.as_deref() {
        Some("svg") | Some("svd") => sheet_to_svg(&sheet),
        Some("pdf") => sheet_to_pdf(&sheet),
        Some("dxf") => sheet_to_dxf(&sheet).into_bytes(),
        Some("png") => sheet_to_png(&sheet, DEFAULT_PNG_DPI),
        Some(other) => {
            return Err(format!(
                "unsupported extension '.{other}' for drafting sheet (use svg/pdf/dxf/png)"
            ))
        }
        None => {
            return Err("export path must have an extension (svg/pdf/dxf/png)".to_string())
        }
    };
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

fn normalize_path(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        // Default to SVG for a "looks right on screen" deliverable.
        path.with_extension("svg")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn normalize_path_adds_svg_extension_if_missing() {
        let p = normalize_path(Path::new("/tmp/foo").to_path_buf());
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("svg"));
    }

    #[test]
    fn normalize_path_preserves_explicit_extension() {
        let p = normalize_path(Path::new("/tmp/foo.pdf").to_path_buf());
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("pdf"));
    }
}
