//! Sheet → PDF. Paper-mm native; one conversion `mm → pt`.
//!
//! The PDF MediaBox is `bounds` converted from mm to PostScript points
//! (`mm * 72 / 25.4`). Every primitive goes through the same single
//! conversion so the line-weight hierarchy from the sheet is preserved
//! verbatim on the page.

use std::io::Write as IoWrite;

use bevy::math::Vec2;

use crate::plugins::drafting::DimPrimitive;
use crate::plugins::section_fill::{generate_hatch_lines, HatchPattern};

use super::sheet::{DraftingSheet, SheetBounds, SheetHatch, SheetLine, SheetStroke};

const MM_TO_PT: f32 = 72.0 / 25.4;
const DIMENSION_STROKE_GRAY: f32 = 0.29;

#[must_use]
pub fn sheet_to_pdf(sheet: &DraftingSheet) -> Vec<u8> {
    let bounds = if sheet.bounds.is_valid() {
        sheet.bounds
    } else {
        SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::splat(100.0),
        }
    };
    let w_pt = bounds.width() * MM_TO_PT;
    let h_pt = bounds.height() * MM_TO_PT;

    // Mapping: paper-mm (sheet-space, +y-up) →
    //          PDF points (PDF +y-up natively, so no flip), shifted so
    //          sheet's bounds.min lands at (0, 0).
    let to_pt = |p: Vec2| -> (f32, f32) {
        (
            (p.x - bounds.min.x) * MM_TO_PT,
            (p.y - bounds.min.y) * MM_TO_PT,
        )
    };

    let mut content = Vec::new();
    let _ = writeln!(content, "1 1 1 rg");
    let _ = writeln!(content, "0 0 {w_pt} {h_pt} re f");
    let _ = writeln!(content, "0 0 0 RG");
    let _ = writeln!(content, "1 J"); // round line caps

    // Hatches (black against white).
    for hatch in &sheet.hatches {
        write_hatch(&mut content, hatch, &to_pt);
    }

    // Lines grouped by stroke so we issue one `w` operator per weight.
    let order = [
        SheetStroke::SectionCut,
        SheetStroke::Silhouette,
        SheetStroke::Crease,
        SheetStroke::Boundary,
        SheetStroke::Dimension,
    ];
    for stroke in order {
        let group: Vec<&SheetLine> = sheet.lines.iter().filter(|l| l.stroke == stroke).collect();
        if group.is_empty() {
            continue;
        }
        let weight_pt = stroke.weight_mm() * MM_TO_PT;
        let _ = writeln!(content, "{weight_pt:.3} w");
        for line in group {
            let (ax, ay) = to_pt(line.a);
            let (bx, by) = to_pt(line.b);
            let _ = writeln!(content, "{ax:.2} {ay:.2} m {bx:.2} {by:.2} l S");
        }
    }

    // Annotations: rich drafting primitives, coordinates in paper-mm.
    let _ = writeln!(
        content,
        "{g:.3} {g:.3} {g:.3} RG",
        g = DIMENSION_STROKE_GRAY
    );
    let _ = writeln!(
        content,
        "{g:.3} {g:.3} {g:.3} rg",
        g = DIMENSION_STROKE_GRAY
    );
    for dim in &sheet.annotations {
        for prim in dim {
            write_dim_primitive(&mut content, prim, &to_pt);
        }
    }

    // Assemble the PDF structure.
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xC7\xEC\x8F\xA2\n");
    let mut offsets = Vec::new();
    write_obj(
        &mut out,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    write_obj(
        &mut out,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    let page_dict = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt:.2} {h_pt:.2}] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
    );
    write_obj(&mut out, &mut offsets, 3, page_dict.as_bytes());
    write_obj(
        &mut out,
        &mut offsets,
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    let stream_hdr = format!("<< /Length {} >>\nstream\n", content.len());
    offsets.push(out.len());
    let _ = write!(out, "5 0 obj\n{stream_hdr}");
    out.extend_from_slice(&content);
    out.extend_from_slice(b"endstream\nendobj\n");

    let xref_offset = out.len();
    let _ = write!(out, "xref\n0 {}\n", offsets.len() + 1);
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        let _ = writeln!(out, "{off:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        offsets.len() + 1,
    );
    out
}

fn write_obj(out: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, payload: &[u8]) {
    offsets.push(out.len());
    let _ = write!(out, "{id} 0 obj\n");
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendobj\n");
}

// ─── Hatches ──────────────────────────────────────────────────────────────

fn write_hatch(content: &mut Vec<u8>, hatch: &SheetHatch, to_pt: &impl Fn(Vec2) -> (f32, f32)) {
    if hatch.polygon.len() < 3 {
        return;
    }
    let _ = writeln!(content, "q"); // save state
                                    // Clip path.
    let (x, y) = to_pt(hatch.polygon[0]);
    let _ = writeln!(content, "{x:.2} {y:.2} m");
    for p in &hatch.polygon[1..] {
        let (px, py) = to_pt(*p);
        let _ = writeln!(content, "{px:.2} {py:.2} l");
    }
    let _ = writeln!(content, "h W n");

    match hatch.pattern {
        HatchPattern::SolidFill => {
            let _ = writeln!(content, "0 0 0 rg");
            let (x, y) = to_pt(hatch.polygon[0]);
            let _ = writeln!(content, "{x:.2} {y:.2} m");
            for p in &hatch.polygon[1..] {
                let (px, py) = to_pt(*p);
                let _ = writeln!(content, "{px:.2} {py:.2} l");
            }
            let _ = writeln!(content, "h f");
        }
        HatchPattern::NoFill => {}
        _ => {
            let polygon: Vec<[f32; 2]> = hatch.polygon.iter().map(|p| [p.x, p.y]).collect();
            let hatch_lines = generate_hatch_lines(&polygon, hatch.pattern, 0.333);
            if !hatch_lines.is_empty() {
                let weight_pt = SheetStroke::Hatch.weight_mm() * MM_TO_PT;
                let _ = writeln!(content, "{weight_pt:.3} w");
                for line in &hatch_lines {
                    let (ax, ay) = to_pt(Vec2::new(line[0], line[1]));
                    let (bx, by) = to_pt(Vec2::new(line[2], line[3]));
                    let _ = writeln!(content, "{ax:.2} {ay:.2} m {bx:.2} {by:.2} l S");
                }
            }
        }
    }

    let _ = writeln!(content, "Q"); // restore
}

// ─── Dim primitives ──────────────────────────────────────────────────────

fn write_dim_primitive(
    content: &mut Vec<u8>,
    prim: &DimPrimitive,
    to_pt: &impl Fn(Vec2) -> (f32, f32),
) {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            let w = stroke_mm * MM_TO_PT;
            let _ = writeln!(content, "{w:.3} w");
            let (ax, ay) = to_pt(*a);
            let (bx, by) = to_pt(*b);
            let _ = writeln!(content, "{ax:.2} {ay:.2} m {bx:.2} {by:.2} l S");
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
            let x1 = Vec2::new(pos.x - half * c, pos.y - half * s);
            let x2 = Vec2::new(pos.x + half * c, pos.y + half * s);
            let w = stroke_mm * MM_TO_PT;
            let _ = writeln!(content, "{w:.3} w");
            let (ax, ay) = to_pt(x1);
            let (bx, by) = to_pt(x2);
            let _ = writeln!(content, "{ax:.2} {ay:.2} m {bx:.2} {by:.2} l S");
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm: _,
        } => {
            let axis = *tail - *tip;
            let len = axis.length().max(1e-5);
            let axis_u = axis / len;
            let perp = Vec2::new(-axis_u.y, axis_u.x);
            let half_w = width_mm * 0.5;
            let base_l = *tail + perp * half_w;
            let base_r = *tail - perp * half_w;
            let (tx, ty) = to_pt(*tip);
            let (lx, ly) = to_pt(base_l);
            let (rx, ry) = to_pt(base_r);
            let op = if *filled { "f" } else { "S" };
            let _ = writeln!(
                content,
                "{tx:.2} {ty:.2} m {lx:.2} {ly:.2} l {rx:.2} {ry:.2} l h {op}"
            );
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            let r_pt = radius_mm * MM_TO_PT;
            let (cx, cy) = to_pt(*pos);
            // Filled square approximation (PDF has no primitive circle
            // op; dots are small enough this is invisible in practice).
            let _ = writeln!(
                content,
                "{x:.2} {y:.2} {w:.2} {h:.2} re f",
                x = cx - r_pt,
                y = cy - r_pt,
                w = r_pt * 2.0,
                h = r_pt * 2.0,
            );
        }
        DimPrimitive::Text {
            anchor,
            content: text,
            height_mm,
            rotation_rad,
            anchor_mode: _,
            font_family: _,
            color_hex: _,
        } => {
            let size_pt = height_mm * MM_TO_PT;
            let cr = rotation_rad.cos();
            let sr = rotation_rad.sin();
            let (x, y) = to_pt(*anchor);
            let _ = writeln!(content, "BT");
            let _ = writeln!(content, "/F1 {size_pt:.2} Tf");
            let _ = writeln!(
                content,
                "{cr:.4} {sr:.4} {:.4} {cr:.4} {x:.2} {y:.2} Tm",
                -sr,
            );
            let escaped = pdf_escape_text(text);
            let _ = writeln!(content, "({escaped}) Tj");
            let _ = writeln!(content, "ET");
        }
    }
}

fn pdf_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii() => out.push(c),
            c => out.push_str(&format!("\\{:03o}", c as u32 & 0xff)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting_sheet::sheet::{SheetBounds, SheetLine, SheetStroke};

    #[test]
    fn pdf_mediabox_matches_paper_bounds_in_points() {
        let mut s = DraftingSheet::new(50.0);
        s.lines.push(SheetLine {
            a: Vec2::ZERO,
            b: Vec2::new(100.0, 0.0),
            stroke: SheetStroke::Silhouette,
        });
        s.bounds = SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 50.0),
        };
        let bytes = sheet_to_pdf(&s);
        let text = String::from_utf8_lossy(&bytes);
        // 100 mm × 72/25.4 = 283.46 pt ; 50 mm = 141.73 pt
        assert!(
            text.contains("/MediaBox [0 0 283.46 141.73]"),
            "mediabox missing, got:\n{text}"
        );
    }

    #[test]
    fn silhouette_stroke_matches_035mm_converted() {
        let mut s = DraftingSheet::new(50.0);
        s.lines.push(SheetLine {
            a: Vec2::ZERO,
            b: Vec2::new(100.0, 0.0),
            stroke: SheetStroke::Silhouette,
        });
        s.bounds = SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 10.0),
        };
        let bytes = sheet_to_pdf(&s);
        let text = String::from_utf8_lossy(&bytes);
        // 0.35 mm * 72/25.4 = 0.992 pt
        assert!(
            text.contains("0.992 w"),
            "expected silhouette 0.992 pt, got:\n{text}"
        );
    }
}
