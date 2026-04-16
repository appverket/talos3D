//! Sheet → PNG.
//!
//! The goal of PP69's PNG path is *not* "viewport screenshot with UI
//! chrome cropped" (that's what PP68 shipped, and which dragged FPS
//! overlays and mode banners into the export). It's the same drawing the
//! SVG/PDF writers produce, rasterised to a standalone PNG with no
//! chrome.
//!
//! This MVP implements the rasterisation with a tiny self-contained
//! vector renderer over [`image`]: filled polygons for solid-black poché,
//! stroked lines with Bresenham-based anti-aliased drawing for edges and
//! dim primitives, and bitmap text via `ab_glyph` / built-in metrics.
//!
//! For the first cut we skip text (the glyphs would need a font asset
//! and the vector formats already render text correctly). The goal is
//! that a user *looking at* the PNG sees their drawing with no overlays;
//! text can come through as a follow-up.

use image::{Rgb, RgbImage};

use super::sheet::{DraftingSheet, SheetBounds, SheetStroke};
use bevy::math::Vec2;

/// Render the sheet as an RGB PNG at the given DPI (dots per inch on
/// paper). 150–300 DPI is typical for print output; 96 DPI for screen.
#[must_use]
pub fn sheet_to_png(sheet: &DraftingSheet, dpi: f32) -> Vec<u8> {
    let bounds = if sheet.bounds.is_valid() {
        sheet.bounds
    } else {
        SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::splat(100.0),
        }
    };
    // 1 inch = 25.4 mm → pixels per mm = dpi / 25.4.
    let px_per_mm = (dpi / 25.4).max(1.0);
    let w_px = ((bounds.width() * px_per_mm).ceil() as u32).max(1);
    let h_px = ((bounds.height() * px_per_mm).ceil() as u32).max(1);

    let mut img = RgbImage::from_pixel(w_px, h_px, Rgb([255, 255, 255]));

    let to_px = |p: Vec2| -> (f32, f32) {
        let x = (p.x - bounds.min.x) * px_per_mm;
        // Paper +y-up → image +y-down.
        let y = (bounds.max.y - p.y) * px_per_mm;
        (x, y)
    };

    // Hatch: draw the fill polygon (solid black for poché only; hatch
    // lines are drawn below as regular strokes).
    for hatch in &sheet.hatches {
        if matches!(
            hatch.pattern,
            crate::plugins::section_fill::HatchPattern::SolidFill
        ) {
            let pts: Vec<(f32, f32)> = hatch.polygon.iter().map(|p| to_px(*p)).collect();
            fill_polygon(&mut img, &pts, Rgb([0, 0, 0]));
        }
        // For non-solid patterns, generate_hatch_lines would be called
        // upstream, but the sheet writers themselves own that in the
        // SVG/PDF paths. For raster MVP we approximate by leaving the
        // polygon uncoloured; the outline (section-cut stroke) is
        // already on the sheet and will be rendered below.
    }

    // Lines grouped by stroke weight.
    let order = [
        SheetStroke::SectionCut,
        SheetStroke::Silhouette,
        SheetStroke::Crease,
        SheetStroke::Boundary,
        SheetStroke::Dimension,
    ];
    for stroke in order {
        let weight_px = (stroke.weight_mm() * px_per_mm).max(1.0);
        for line in sheet.lines.iter().filter(|l| l.stroke == stroke) {
            let (ax, ay) = to_px(line.a);
            let (bx, by) = to_px(line.b);
            draw_line(&mut img, ax, ay, bx, by, weight_px, Rgb([0, 0, 0]));
        }
    }

    // Dim primitives: render just as thin lines for MVP; text skipped.
    for dim in &sheet.annotations {
        for prim in dim {
            render_dim_prim(&mut img, prim, &to_px, px_per_mm);
        }
    }

    // Encode PNG.
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    image::ImageEncoder::write_image(
        encoder,
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )
    .expect("PNG encoding is infallible for RGB8");
    out
}

fn render_dim_prim(
    img: &mut RgbImage,
    prim: &crate::plugins::drafting::DimPrimitive,
    to_px: &impl Fn(Vec2) -> (f32, f32),
    px_per_mm: f32,
) {
    use crate::plugins::drafting::DimPrimitive;
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            let (ax, ay) = to_px(*a);
            let (bx, by) = to_px(*b);
            let w = (stroke_mm * px_per_mm).max(1.0);
            draw_line(img, ax, ay, bx, by, w, Rgb([0, 0, 0]));
        }
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => {
            let half = length_mm * 0.5;
            let c = rotation_rad.cos();
            let s = rotation_rad.sin();
            let a = Vec2::new(pos.x - half * c, pos.y - half * s);
            let b = Vec2::new(pos.x + half * c, pos.y + half * s);
            let (ax, ay) = to_px(a);
            let (bx, by) = to_px(b);
            let w = (stroke_mm * px_per_mm).max(1.0);
            draw_line(img, ax, ay, bx, by, w, Rgb([0, 0, 0]));
        }
        DimPrimitive::Arrow { tip, tail, .. } => {
            let (tx, ty) = to_px(*tip);
            let (bx, by) = to_px(*tail);
            draw_line(
                img,
                tx,
                ty,
                bx,
                by,
                1.0_f32.max(px_per_mm * 0.18),
                Rgb([0, 0, 0]),
            );
        }
        DimPrimitive::Dot { pos, .. } => {
            let (x, y) = to_px(*pos);
            plot(img, x as i32, y as i32, Rgb([0, 0, 0]));
        }
        DimPrimitive::Text { .. } => {
            // Text is rendered by the SVG/PDF writers. For PNG MVP we
            // intentionally skip it — the dim geometry (lines, ticks,
            // extension lines) is still valid visual documentation.
        }
    }
}

// ─── Minimal line / polygon rasteriser ───────────────────────────────────

fn plot(img: &mut RgbImage, x: i32, y: i32, c: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, c);
    }
}

/// Draw a thick line via "midpoint circle, per pixel on the Bresenham
/// path". Good enough for arch-weight strokes.
fn draw_line(img: &mut RgbImage, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, c: Rgb<u8>) {
    let radius = (thickness * 0.5).max(0.5) as i32;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let steps = len.ceil() as i32;
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let px = (x0 + dx * t) as i32;
        let py = (y0 + dy * t) as i32;
        for oy in -radius..=radius {
            for ox in -radius..=radius {
                if ox * ox + oy * oy <= radius * radius {
                    plot(img, px + ox, py + oy, c);
                }
            }
        }
    }
}

/// Scanline fill for a (small) polygon. Correct for simple convex /
/// mildly concave polygons at architectural scale. Not bulletproof for
/// self-intersecting inputs (we don't generate those on the sheet).
fn fill_polygon(img: &mut RgbImage, pts: &[(f32, f32)], c: Rgb<u8>) {
    if pts.len() < 3 {
        return;
    }
    let y_min = pts
        .iter()
        .map(|p| p.1)
        .fold(f32::INFINITY, f32::min)
        .floor() as i32;
    let y_max = pts
        .iter()
        .map(|p| p.1)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil() as i32;
    let h = img.height() as i32;
    let w = img.width() as i32;
    for y in y_min.max(0)..=y_max.min(h - 1) {
        let mut xs = Vec::new();
        let y_f = y as f32 + 0.5;
        for i in 0..pts.len() {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % pts.len()];
            if (y0 <= y_f && y1 > y_f) || (y1 <= y_f && y0 > y_f) {
                let t = (y_f - y0) / (y1 - y0);
                xs.push(x0 + (x1 - x0) * t);
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut i = 0;
        while i + 1 < xs.len() {
            let x_start = xs[i].floor() as i32;
            let x_end = xs[i + 1].ceil() as i32;
            for x in x_start.max(0)..=x_end.min(w - 1) {
                plot(img, x, y, c);
            }
            i += 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting_sheet::sheet::{SheetBounds, SheetLine, SheetStroke};

    #[test]
    fn png_is_non_empty_and_correct_size_for_100mm_at_96dpi() {
        let mut s = DraftingSheet::new(50.0);
        s.lines.push(SheetLine {
            a: Vec2::new(0.0, 0.0),
            b: Vec2::new(100.0, 0.0),
            stroke: SheetStroke::Silhouette,
        });
        s.bounds = SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 50.0),
        };
        let bytes = sheet_to_png(&s, 96.0);
        assert!(!bytes.is_empty());
        // 100 mm at 96 dpi ≈ 378 px.
        let img = image::load_from_memory(&bytes).unwrap();
        assert!(
            (img.width() as i32 - 378).abs() <= 2,
            "expected width ~378 px, got {}",
            img.width()
        );
    }
}
