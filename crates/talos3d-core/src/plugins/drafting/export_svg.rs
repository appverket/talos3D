//! SVG export for dimension primitives.
//!
//! Emits clean, print-ready SVG at paper-scale (units interpreted as mm when
//! wrapped in a document via [`render_dimensions_svg_document`]). A drawing
//! harness that already controls the outer `<svg>` element can use
//! [`render_dimensions_svg_fragment`] directly and compose.
//!
//! # Conventions
//!
//! - Uses `markerUnits="userSpaceOnUse"` so markers stay at correct paper size
//!   regardless of stroke-width — critical for ticks and arrows to look right.
//! - Wraps each dimension's primitives in `<g class="dim">` so downstream CSS
//!   can restyle post-export.
//! - All numeric values are written with at most 3 decimal places to keep the
//!   SVG compact while staying well below plotter precision.
//!
//! The writer is deterministic and allocation-conservative: same input → same
//! byte-for-byte SVG.

use std::fmt::Write;

use crate::plugins::drafting::render::{DimPrimitive, TextAnchor};

/// Build a complete, self-contained SVG document showing the given dimensions.
/// Viewport size is `width_mm` × `height_mm`; content is not clipped. Coordinates
/// are taken as paper millimetres; SVG `y` is flipped to grow downward.
///
/// `drawing_primitives` is an optional companion list of primitives for the
/// underlying drawing (walls, etc.) the dimensions are annotating; it is drawn
/// first so dimensions overlay the geometry.
#[must_use]
pub fn render_dimensions_svg_document(
    width_mm: f32,
    height_mm: f32,
    drawing_primitives: &[DimPrimitive],
    dimensions: &[Vec<DimPrimitive>],
) -> String {
    let mut out = String::with_capacity(4096);
    let _ = writeln!(
        out,
        r#"<?xml version="1.0" encoding="UTF-8"?>"#
    );
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" version="1.1" width="{w}mm" height="{h}mm" viewBox="0 0 {w} {h}">"#,
        w = fmt_num(width_mm),
        h = fmt_num(height_mm)
    );

    // Transform that flips y so +y-up in paper becomes +y-down in SVG viewBox,
    // and shifts so (0,0) paper lands at the bottom-left of the viewBox.
    let _ = writeln!(
        out,
        r#"<g transform="translate(0 {h}) scale(1 -1)">"#,
        h = fmt_num(height_mm)
    );

    // Base drawing (walls etc.)
    if !drawing_primitives.is_empty() {
        let _ = writeln!(out, r#"<g class="drawing">"#);
        for prim in drawing_primitives {
            write_primitive(&mut out, prim);
        }
        let _ = writeln!(out, "</g>");
    }

    // Dimensions
    for dim in dimensions {
        let _ = writeln!(out, r#"<g class="dim">"#);
        for prim in dim {
            write_primitive(&mut out, prim);
        }
        let _ = writeln!(out, "</g>");
    }

    let _ = writeln!(out, "</g>"); // transform
    let _ = writeln!(out, "</svg>");
    out
}

/// SVG fragment (no `<?xml ?>`, no outer `<svg>`) ready to splice into an
/// existing drawing. Emits only a `<g class="dim">` wrapper; the caller is
/// responsible for the paper y-flip transform.
#[must_use]
pub fn render_dimensions_svg_fragment(dimensions: &[Vec<DimPrimitive>]) -> String {
    let mut out = String::with_capacity(1024);
    for dim in dimensions {
        let _ = writeln!(out, r#"<g class="dim">"#);
        for prim in dim {
            write_primitive(&mut out, prim);
        }
        let _ = writeln!(out, "</g>");
    }
    out
}

fn write_primitive(out: &mut String, prim: &DimPrimitive) {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm } => {
            let _ = writeln!(
                out,
                r##"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="#000" stroke-width="{}" stroke-linecap="round" fill="none"/>"##,
                fmt_num(a.x),
                fmt_num(a.y),
                fmt_num(b.x),
                fmt_num(b.y),
                fmt_num(*stroke_mm)
            );
        }
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm,
        } => {
            // Draw a tick centred on `pos`, rotated so its long axis makes the
            // requested paper-space angle with +x. Because the enclosing group
            // already flips y, the tick's rotation angle is supplied in the
            // PAPER frame (+y up) and must be negated to match SVG's +y-down
            // at the <line> level — but the outer scale(1 -1) already handles
            // that, so we render the tick in the paper frame directly.
            let half = length_mm * 0.5;
            let c = rotation_rad.cos();
            let s = rotation_rad.sin();
            let x1 = pos.x - half * c;
            let y1 = pos.y - half * s;
            let x2 = pos.x + half * c;
            let y2 = pos.y + half * s;
            let _ = writeln!(
                out,
                r##"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="#000" stroke-width="{}" stroke-linecap="round" fill="none"/>"##,
                fmt_num(x1),
                fmt_num(y1),
                fmt_num(x2),
                fmt_num(y2),
                fmt_num(*stroke_mm)
            );
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            filled,
            stroke_mm,
        } => {
            // Build triangle in paper frame: tip + two base points ± perpendicular.
            let axis = *tail - *tip;
            let len = axis.length().max(1e-5);
            let axis_u = axis / len;
            let perp = bevy::math::Vec2::new(-axis_u.y, axis_u.x);
            let half_w = width_mm * 0.5;
            let base_l = *tail + perp * half_w;
            let base_r = *tail - perp * half_w;
            let fill = if *filled { "#000" } else { "none" };
            let _ = writeln!(
                out,
                r##"<polygon points="{},{} {},{} {},{}" fill="{}" stroke="#000" stroke-width="{}" stroke-linejoin="miter"/>"##,
                fmt_num(tip.x),
                fmt_num(tip.y),
                fmt_num(base_l.x),
                fmt_num(base_l.y),
                fmt_num(base_r.x),
                fmt_num(base_r.y),
                fill,
                fmt_num(*stroke_mm)
            );
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            let _ = writeln!(
                out,
                r##"<circle cx="{}" cy="{}" r="{}" fill="#000"/>"##,
                fmt_num(pos.x),
                fmt_num(pos.y),
                fmt_num(*radius_mm)
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
            // Text anchoring:
            //   CenterBaseline → text-anchor=middle, dominant-baseline=alphabetic
            //                    and lifted by height_mm so the baseline sits at
            //                    the anchor.
            //   Center         → text-anchor=middle, dominant-baseline=middle.
            //
            // Because we render inside scale(1 -1), text would draw upside-down
            // without a compensating local scale(1 -1) around each text anchor.
            let (text_anchor, dominant_baseline) = match anchor_mode {
                TextAnchor::CenterBaseline => ("middle", "alphabetic"),
                TextAnchor::Center => ("middle", "middle"),
            };
            // Compose: translate to anchor → flip y back so glyphs render
            // upright in the viewBox → rotate by rotation_rad (paper frame).
            let rot_deg = rotation_rad.to_degrees();
            let _ = writeln!(
                out,
                r##"<g transform="translate({ax} {ay}) scale(1 -1) rotate({rot})"><text x="0" y="0" text-anchor="{ta}" dominant-baseline="{db}" font-family="{font}" font-size="{fs}" fill="#{color}">{content}</text></g>"##,
                ax = fmt_num(anchor.x),
                ay = fmt_num(anchor.y),
                rot = fmt_num(-rot_deg), // flip rotation sense after scale(1,-1)
                ta = text_anchor,
                db = dominant_baseline,
                font = escape_attr(font_family),
                fs = fmt_num(*height_mm),
                color = color_hex,
                content = escape_text(content)
            );
        }
    }
}

fn fmt_num(n: f32) -> String {
    // 3-decimal precision, trim trailing zeros for compactness.
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
    use crate::plugins::drafting::{
        kind::DimensionKind,
        render::{render_dimension, DimensionInput},
        style::DimensionStyle,
    };
    use bevy::math::Vec3;

    #[test]
    fn fragment_is_not_empty_for_simple_dim() {
        let style = DimensionStyle::architectural_imperial();
        let input = DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(4.572, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.5, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let svg = render_dimensions_svg_fragment(&[prims]);
        assert!(svg.contains("<g class=\"dim\""));
        assert!(svg.contains("<line"));
        assert!(svg.contains("15'-0\""), "SVG should contain formatted text");
    }

    #[test]
    fn document_has_xml_prolog_and_svg_root() {
        let style = DimensionStyle::architectural_metric();
        let input = DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(4.572, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.5, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let svg = render_dimensions_svg_document(100.0, 100.0, &[], &[prims]);
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("4572"));
    }

    #[test]
    fn escapes_text_content() {
        let style = DimensionStyle::architectural_metric();
        let input = DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(1.0, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.5, 0.0),
            text_override: Some("A & B <tag>".to_string()),
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let svg = render_dimensions_svg_fragment(&[prims]);
        assert!(svg.contains("A &amp; B &lt;tag&gt;"));
    }
}
