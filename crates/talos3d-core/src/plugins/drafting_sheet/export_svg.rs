//! Sheet → SVG. Paper-mm native.
//!
//! The SVG viewBox is `bounds.min..bounds.max` in paper millimetres, the
//! outer `<svg>` carries `width="Xmm" height="Ymm"`, and the content is
//! wrapped in one `<g transform="…">` that flips y so paper +y-up lines
//! up with SVG's +y-down. Every attribute below that transform is paper
//! mm; stroke-width ≡ paper mm; `font-size` ≡ paper mm.
//!
//! Nothing in the writer scales — the unit contract is honoured by the
//! sheet upstream.

use std::fmt::Write;

use bevy::math::Vec2;

use crate::plugins::drafting::{DimPrimitive, TextAnchor};
use crate::plugins::section_fill::{generate_hatch_lines, HatchPattern};

use super::sheet::{DraftingSheet, SheetBounds, SheetHatch, SheetLine, SheetStroke};

/// Render a [`DraftingSheet`] as a self-contained SVG document. Output
/// bytes are UTF-8; caller owns the bytes.
#[must_use]
pub fn sheet_to_svg(sheet: &DraftingSheet) -> Vec<u8> {
    let bounds = if sheet.bounds.is_valid() {
        sheet.bounds
    } else {
        SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::splat(100.0),
        }
    };

    let mut out = String::with_capacity(8192);
    let w_mm = bounds.width();
    let h_mm = bounds.height();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w_mm}mm" height="{h_mm}mm" viewBox="0 0 {w_mm} {h_mm}">"#
    );
    let _ = writeln!(
        out,
        r#"  <title>Talos3D drafting sheet (1:{scale})</title>"#,
        scale = fmt_num(sheet.scale_denominator)
    );
    let _ = writeln!(out, r#"  <rect width="100%" height="100%" fill="white"/>"#);

    // Content group: paper +y-up → SVG +y-down by translate+scale(1,-1).
    // Also shift the bounds.min corner to (0,0) so the viewBox starts
    // cleanly at the origin.
    let _ = writeln!(
        out,
        r#"  <g transform="translate({tx} {ty}) scale(1 -1)">"#,
        tx = fmt_num(-bounds.min.x),
        ty = fmt_num(bounds.max.y),
    );

    // Hatches first so lines draw on top.
    write_hatches(&mut out, sheet);
    // Group lines by stroke so writers emit one `<g>` per weight.
    write_lines_grouped(&mut out, &sheet.lines);
    // Annotations (already in paper-mm).
    write_annotations(&mut out, &sheet.annotations);

    let _ = writeln!(out, "  </g>");
    let _ = writeln!(out, "</svg>");
    out.into_bytes()
}

// ─── Stroke helpers ───────────────────────────────────────────────────────

fn stroke_class(stroke: SheetStroke) -> &'static str {
    match stroke {
        SheetStroke::SectionCut => "section-cut",
        SheetStroke::Silhouette => "silhouette",
        SheetStroke::Crease => "crease",
        SheetStroke::Boundary => "boundary",
        SheetStroke::Dimension => "dimension",
        SheetStroke::Hatch => "hatch",
    }
}

fn write_lines_grouped(out: &mut String, lines: &[SheetLine]) {
    let order = [
        SheetStroke::SectionCut,
        SheetStroke::Silhouette,
        SheetStroke::Crease,
        SheetStroke::Boundary,
        SheetStroke::Dimension,
    ];
    for stroke in order {
        let group: Vec<&SheetLine> = lines.iter().filter(|l| l.stroke == stroke).collect();
        if group.is_empty() {
            continue;
        }
        let w = stroke.weight_mm();
        let _ = writeln!(
            out,
            r#"    <g class="{cls}" stroke="black" stroke-width="{w}" stroke-linecap="round" fill="none">"#,
            cls = stroke_class(stroke),
            w = fmt_num(w),
        );
        let _ = write!(out, r#"      <path d=""#);
        for line in group {
            let _ = write!(
                out,
                "M{ax},{ay}L{bx},{by}",
                ax = fmt_num(line.a.x),
                ay = fmt_num(line.a.y),
                bx = fmt_num(line.b.x),
                by = fmt_num(line.b.y),
            );
        }
        let _ = writeln!(out, r#""/>"#);
        let _ = writeln!(out, "    </g>");
    }
}

// ─── Hatches ──────────────────────────────────────────────────────────────

fn write_hatches(out: &mut String, sheet: &DraftingSheet) {
    for (i, hatch) in sheet.hatches.iter().enumerate() {
        if hatch.polygon.len() < 3 {
            continue;
        }
        let clip_id = format!("section-clip-{i}");
        let _ = writeln!(out, r#"    <defs>"#);
        let _ = writeln!(out, r#"      <clipPath id="{clip_id}">"#);
        let _ = write!(out, r#"        <polygon points=""#);
        for (j, p) in hatch.polygon.iter().enumerate() {
            if j > 0 {
                let _ = write!(out, " ");
            }
            let _ = write!(out, "{x},{y}", x = fmt_num(p.x), y = fmt_num(p.y));
        }
        let _ = writeln!(out, r#""/>"#);
        let _ = writeln!(out, r#"      </clipPath>"#);
        let _ = writeln!(out, r#"    </defs>"#);

        if matches!(hatch.pattern, HatchPattern::SolidFill) {
            // Solid black poché.
            let _ = write!(out, r#"    <polygon points=""#);
            for (j, p) in hatch.polygon.iter().enumerate() {
                if j > 0 {
                    let _ = write!(out, " ");
                }
                let _ = write!(out, "{x},{y}", x = fmt_num(p.x), y = fmt_num(p.y));
            }
            let _ = writeln!(out, r#"" fill="black" stroke="none"/>"#);
        } else if !matches!(hatch.pattern, HatchPattern::NoFill) {
            write_hatch_pattern(out, hatch, i, &clip_id);
        }
    }
}

fn write_hatch_pattern(out: &mut String, hatch: &SheetHatch, i: usize, clip_id: &str) {
    // Paper-mm polygon → paper-mm hatch lines. The hatch generator takes
    // a `pixels_per_world` density knob — here, one "unit" is one paper
    // mm, so 1.0 produces ~1 hatch line per mm. Scale to 0.33 → hatch
    // spacing ~3 mm (classic arch).
    let polygon: Vec<[f32; 2]> = hatch.polygon.iter().map(|p| [p.x, p.y]).collect();
    let hatch_lines = generate_hatch_lines(&polygon, hatch.pattern, 0.333);
    if hatch_lines.is_empty() {
        return;
    }
    let weight = SheetStroke::Hatch.weight_mm();
    let _ = writeln!(
        out,
        r#"    <g class="section-hatch-{i}" clip-path="url(#{clip_id})" stroke="black" stroke-width="{w}" fill="none">"#,
        w = fmt_num(weight),
    );
    let _ = write!(out, r#"      <path d=""#);
    for line in &hatch_lines {
        let _ = write!(
            out,
            "M{ax},{ay}L{bx},{by}",
            ax = fmt_num(line[0]),
            ay = fmt_num(line[1]),
            bx = fmt_num(line[2]),
            by = fmt_num(line[3]),
        );
    }
    let _ = writeln!(out, r#""/>"#);
    let _ = writeln!(out, "    </g>");
}

// ─── Annotations ──────────────────────────────────────────────────────────

fn write_annotations(out: &mut String, annotations: &[Vec<DimPrimitive>]) {
    for dim in annotations {
        let _ = writeln!(out, r#"    <g class="dim-drafting">"#);
        for prim in dim {
            write_dim_primitive(out, prim);
        }
        let _ = writeln!(out, "    </g>");
    }
}

fn write_dim_primitive(out: &mut String, prim: &DimPrimitive) {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            let _ = writeln!(
                out,
                r##"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="#000" stroke-width="{}" stroke-linecap="round" fill="none"/>"##,
                fmt_num(a.x),
                fmt_num(a.y),
                fmt_num(b.x),
                fmt_num(b.y),
                fmt_num(*stroke_mm),
            );
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
            let x1 = pos.x - half * c;
            let y1 = pos.y - half * s;
            let x2 = pos.x + half * c;
            let y2 = pos.y + half * s;
            let _ = writeln!(
                out,
                r##"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="#000" stroke-width="{}" stroke-linecap="round" fill="none"/>"##,
                fmt_num(x1),
                fmt_num(y1),
                fmt_num(x2),
                fmt_num(y2),
                fmt_num(*stroke_mm),
            );
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm,
        } => {
            let axis = *tail - *tip;
            let len = axis.length().max(1e-5);
            let axis_u = axis / len;
            let perp = Vec2::new(-axis_u.y, axis_u.x);
            let half_w = width_mm * 0.5;
            let base_l = *tail + perp * half_w;
            let base_r = *tail - perp * half_w;
            let fill = if *filled { "#000" } else { "none" };
            let _ = writeln!(
                out,
                r##"      <polygon points="{},{} {},{} {},{}" fill="{}" stroke="#000" stroke-width="{}" stroke-linejoin="miter"/>"##,
                fmt_num(tip.x),
                fmt_num(tip.y),
                fmt_num(base_l.x),
                fmt_num(base_l.y),
                fmt_num(base_r.x),
                fmt_num(base_r.y),
                fill,
                fmt_num(*stroke_mm),
            );
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            let _ = writeln!(
                out,
                r##"      <circle cx="{}" cy="{}" r="{}" fill="#000"/>"##,
                fmt_num(pos.x),
                fmt_num(pos.y),
                fmt_num(*radius_mm),
            );
        }
        DimPrimitive::Text {
            anchor,
            content,
            height_mm,
            rotation_rad,
            anchor_mode,
            font_family,
            color_hex,
        } => {
            let (text_anchor, baseline) = match anchor_mode {
                TextAnchor::CenterBaseline => ("middle", "alphabetic"),
                TextAnchor::Center => ("middle", "middle"),
            };
            // Compensate for the outer y-flip so glyphs render upright.
            let rot_deg = rotation_rad.to_degrees();
            let _ = writeln!(
                out,
                r##"      <g transform="translate({ax} {ay}) scale(1 -1) rotate({rot})"><text x="0" y="0" text-anchor="{ta}" dominant-baseline="{db}" font-family="{font}" font-size="{fs}" fill="#{color}">{content}</text></g>"##,
                ax = fmt_num(anchor.x),
                ay = fmt_num(anchor.y),
                rot = fmt_num(-rot_deg),
                ta = text_anchor,
                db = baseline,
                font = escape_attr(font_family),
                fs = fmt_num(*height_mm),
                color = color_hex,
                content = escape_text(content),
            );
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────

fn fmt_num(n: f32) -> String {
    let mut s = format!("{n:.3}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting_sheet::sheet::{SheetBounds, SheetLine, SheetStroke};

    fn tiny_sheet() -> DraftingSheet {
        let mut s = DraftingSheet::new(50.0);
        s.lines.push(SheetLine {
            a: Vec2::new(10.0, 10.0),
            b: Vec2::new(90.0, 10.0),
            stroke: SheetStroke::Silhouette,
        });
        s.bounds = SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 20.0),
        };
        s
    }

    #[test]
    fn svg_viewbox_matches_paper_bounds_in_mm() {
        let bytes = sheet_to_svg(&tiny_sheet());
        let svg = String::from_utf8(bytes).unwrap();
        assert!(svg.contains(r#"width="100mm""#));
        assert!(svg.contains(r#"height="20mm""#));
        assert!(svg.contains(r#"viewBox="0 0 100 20""#));
    }

    #[test]
    fn silhouette_stroke_is_exactly_035mm() {
        let svg = String::from_utf8(sheet_to_svg(&tiny_sheet())).unwrap();
        // 0.35 mm is the canonical arch silhouette weight. Writer must
        // not scale it.
        assert!(
            svg.contains(r#"stroke-width="0.35""#),
            "expected silhouette weight 0.35mm in SVG output"
        );
    }

    #[test]
    fn scale_appears_in_title() {
        let svg = String::from_utf8(sheet_to_svg(&tiny_sheet())).unwrap();
        assert!(svg.contains("1:50"));
    }
}
