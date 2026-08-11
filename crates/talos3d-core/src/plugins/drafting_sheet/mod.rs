//! PP69 — DraftingSheet: a 2D paper-native drawing document derived from
//! a 3D view.
//!
//! This module originated from the paper-native drafting sheet proof-point work.
//!
//! Public surface:
//!
//! - [`DraftingSheet`], [`SheetView`], [`SheetBounds`], [`SheetLine`],
//!   [`SheetHatch`], [`SheetStroke`] — the sheet data model, all in
//!   paper millimetres.
//! - [`build_drawing_scene`] — the sole normalized semantic projector.
//! - [`capture_sheet`] — compatibility wrapper that lays out that scene in
//!   paper millimetres.
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
pub mod live;
pub mod preview;
pub mod scene;
pub mod sheet;

pub use capture::{
    build_drawing_scene, capture_sheet, drawing_normalized_to_world, sheet_paper_to_world,
    sheet_view_from_active_camera,
};
pub use export_dxf::sheet_to_dxf;
pub use export_pdf::sheet_to_pdf;
pub use export_png::sheet_to_png;
pub use export_svg::sheet_to_svg;
pub use live::{DrawingSceneLiveCache, DrawingSceneLivePlugin, DrawingSceneLiveStats};
pub use preview::{DraftingSheetPreviewPlugin, SheetPreviewState};
pub use scene::{
    DrawingPrimitiveId, DrawingPrimitiveRole, DrawingScene, DrawingSceneAnnotation,
    DrawingSceneFinding, DrawingSceneHatch, DrawingSceneLine, DrawingSceneLineBatch,
    DrawingSceneLineSpan,
};
pub use sheet::{DraftingSheet, SheetBounds, SheetHatch, SheetLine, SheetStroke, SheetView};

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
    let view = sheet_view_from_active_camera(world, scale, DEFAULT_MARGIN_MM).ok_or_else(|| {
        "no active orthographic camera — drafting requires an ortho view".to_string()
    })?;
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
        None => return Err("export path must have an extension (svg/pdf/dxf/png)".to_string()),
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

    /// Characterizes the live drawing contract — `build_drawing_scene` → pure
    /// `DraftingSheet` layout → the four `sheet_to_*` writers — not the dead
    /// `extract_drawing_geometry`/`DrawingGeometry` path in
    /// `vector_drawing.rs`, which has zero callers outside its own tests.
    ///
    /// Feeds ONE captured sheet into all four exporters and asserts each
    /// format's reported paper size (converted back to mm) agrees with the
    /// others and with `sheet.bounds`, proving they all derive from the
    /// same shared capture rather than four independently hand-built
    /// sheets (as the existing per-format tests do).
    #[test]
    fn capture_sheet_feeds_all_four_exporters_with_consistent_paper_bounds() {
        use bevy::asset::RenderAssetUsages;
        use bevy::mesh::PrimitiveTopology;
        use bevy::prelude::{Assets, GlobalTransform, Mesh, Mesh3d, Vec3, Visibility};

        use crate::capability_registry::CapabilityRegistry;
        use crate::plugins::definition_preview_scene::PreviewOnly;
        use crate::plugins::identity::ElementId;
        use crate::plugins::modeling::generic_factory::PrimitiveFactory;
        use crate::plugins::modeling::primitives::{BoxPrimitive, ShapeRotation};

        let mut world = World::new();
        world.insert_resource(Assets::<Mesh>::default());
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        world.insert_resource(registry);
        // `capture_sheet`'s subject query filters `Without<PreviewOnly>` and
        // reads `Option<&Visibility>` — both component types must be known
        // to a bare `World::new()` before `try_query_filtered` will resolve,
        // even though no entity here actually carries `PreviewOnly`.
        world.register_component::<PreviewOnly>();

        // A single free-standing triangle is enough to produce classified
        // boundary edges (`collect_classified_visible_edges` needs no
        // indices/normals — a position count divisible by 3 is read as one
        // flat triangle list). The `BoxPrimitive`/`ShapeRotation` pair is
        // only there so the capability registry can capture a snapshot and
        // include the entity as a drawing subject; its shape does not need
        // to match the mesh geometry, since `capture_sheet` reads geometry
        // from `Mesh3d`/`Assets<Mesh>`, not from the primitive component.
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[-1.0f32, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
        );
        let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
        world.spawn((
            ElementId(1),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::splat(1.0),
            },
            ShapeRotation::default(),
            Mesh3d(mesh_handle),
            GlobalTransform::IDENTITY,
            Visibility::Visible,
        ));

        let view = SheetView {
            eye: Vec3::new(0.0, 0.0, 10.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 4.0,
            aspect: 1.0,
            scale_denominator: 10.0,
            margin_mm: 5.0,
        };

        let scene = build_drawing_scene(&world, &view)
            .expect("scene builder should project a visible triangle");
        assert!(!scene.lines.is_empty());
        assert!(scene
            .lines
            .iter()
            .all(|line| line.owner == ElementId(1) && line.id.owner == ElementId(1)));
        assert!(scene.lines.iter().all(|line| {
            [line.a, line.b].into_iter().all(|point| {
                point.cmpge(bevy::prelude::Vec2::ZERO).all()
                    && point.cmple(bevy::prelude::Vec2::ONE).all()
            })
        }));
        assert_ne!(scene.source_model_revision, 0);
        let repeated = build_drawing_scene(&world, &view).expect("repeat projection should work");
        assert_eq!(scene.source_model_revision, repeated.source_model_revision);
        assert_eq!(scene.lines[0].id, repeated.lines[0].id);

        let sheet = DraftingSheet::from_scene(scene.clone(), view.margin_mm);
        let compatibility_sheet = capture_sheet(&world, &view)
            .expect("capture_sheet should delegate to the normalized scene");
        assert_eq!(sheet.lines.len(), compatibility_sheet.lines.len());
        assert!((sheet.bounds.min - compatibility_sheet.bounds.min).length() < 1e-4);
        assert!((sheet.bounds.max - compatibility_sheet.bounds.max).length() < 1e-4);
        assert!(
            !sheet.lines.is_empty(),
            "captured sheet should contain the triangle's boundary edges"
        );
        assert!(sheet.bounds.is_valid());
        let width_mm = sheet.bounds.width();
        let height_mm = sheet.bounds.height();

        let svg = String::from_utf8(sheet_to_svg(&sheet)).expect("svg output should be utf8");
        let dxf = sheet_to_dxf(&sheet);
        let pdf = sheet_to_pdf(&sheet);
        let dpi = 96.0;
        let png = sheet_to_png(&sheet, dpi);

        assert!(!svg.is_empty());
        assert!(!dxf.is_empty());
        assert!(!pdf.is_empty());
        assert!(!png.is_empty());

        // SVG viewBox is `0 0 width height` in paper-mm directly.
        let viewbox = svg
            .split("viewBox=\"0 0 ")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("svg should carry a viewBox derived from the captured bounds");
        let mut viewbox_parts = viewbox.split_whitespace();
        let svg_w: f32 = viewbox_parts.next().unwrap().parse().unwrap();
        let svg_h: f32 = viewbox_parts.next().unwrap().parse().unwrap();
        assert!((svg_w - width_mm).abs() < 0.05, "svg width mm mismatch");
        assert!((svg_h - height_mm).abs() < 0.05, "svg height mm mismatch");

        // PDF MediaBox is `0 0 width height` in points (mm * 72 / 25.4).
        let pdf_text = pdf_text(&pdf);
        let mediabox = pdf_text
            .split("/MediaBox [0 0 ")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .expect("pdf should carry a MediaBox derived from the captured bounds");
        let mut mediabox_parts = mediabox.split_whitespace();
        let pdf_w_pt: f32 = mediabox_parts.next().unwrap().parse().unwrap();
        let pdf_h_pt: f32 = mediabox_parts.next().unwrap().parse().unwrap();
        let mm_per_pt = 25.4 / 72.0;
        assert!(
            (pdf_w_pt * mm_per_pt - width_mm).abs() < 0.05,
            "pdf width mm mismatch"
        );
        assert!(
            (pdf_h_pt * mm_per_pt - height_mm).abs() < 0.05,
            "pdf height mm mismatch"
        );

        // DXF $EXTMIN/$EXTMAX are the raw (unnormalized) sheet bounds.
        let (ext_min, ext_max) = dxf_extents(&dxf);
        assert!(
            ((ext_max.0 - ext_min.0) - width_mm).abs() < 0.05,
            "dxf width mm mismatch"
        );
        assert!(
            ((ext_max.1 - ext_min.1) - height_mm).abs() < 0.05,
            "dxf height mm mismatch"
        );

        // PNG pixel size is bounds * dpi/25.4, ceiled.
        let png_image =
            image::load_from_memory(&png).expect("sheet_to_png should emit a decodable PNG");
        use image::GenericImageView;
        let (png_w, png_h) = png_image.dimensions();
        let px_per_mm = dpi / 25.4;
        let expected_w = (width_mm * px_per_mm).ceil() as u32;
        let expected_h = (height_mm * px_per_mm).ceil() as u32;
        assert_eq!(png_w, expected_w.max(1), "png width px mismatch");
        assert_eq!(png_h, expected_h.max(1), "png height px mismatch");
    }

    /// `sheet_to_pdf` returns raw PDF bytes (binary streams included), so
    /// callers that only need the ASCII header/object text (as this test
    /// does, to read `/MediaBox`) must not require the whole byte stream to
    /// be valid UTF-8. Lossily decode instead.
    fn pdf_text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Pull the `$EXTMIN`/`$EXTMAX` (x, y) pairs out of `sheet_to_dxf`'s
    /// text-DXF header. Mirrors the group-code/value line pairing
    /// `write_group` in `drafting/export_dxf.rs` emits.
    fn dxf_extents(dxf: &str) -> ((f32, f32), (f32, f32)) {
        let lines: Vec<&str> = dxf.lines().collect();
        let pair_after = |tag: &str| -> (f32, f32) {
            let idx = lines
                .iter()
                .position(|line| *line == tag)
                .unwrap_or_else(|| panic!("dxf should carry a {tag} header entry"));
            let x: f32 = lines[idx + 2].parse().expect("extent x");
            let y: f32 = lines[idx + 4].parse().expect("extent y");
            (x, y)
        };
        (pair_after("$EXTMIN"), pair_after("$EXTMAX"))
    }
}
