//! DXF export for drafting dimensions.
//!
//! Targets **AC1027 (AutoCAD 2013)** text DXF. Works in a practical
//! "baked-geometry" mode: the DXF is NOT strictly associative. Each dimension
//! is written as primitive geometry (LINE, SOLID, MTEXT/TEXT) so every DXF
//! viewer — LibreCAD, AutoCAD, BricsCAD, DraftSight — renders the dims
//! identically, regardless of its DIMSTYLE implementation.
//!
//! For full associative DXF round-trip, see the plan for Phase 3b (DIMENSION
//! entity + DIMSTYLE table + anonymous `*D###` block). The current
//! implementation is intentionally simpler and more interoperable.
//!
//! # Output structure
//!
//! ```text
//! 0
//! SECTION
//!   2
//!   HEADER            — units, extents
//! 0
//! ENDSEC
//! 0
//! SECTION
//!   2
//!   TABLES
//!     LAYER table with Dimensions + Model layers
//! 0
//! ENDSEC
//! 0
//! SECTION
//!   2
//!   ENTITIES
//!     LINE entities for walls
//!     per-dimension: LINE × N (extension + dim line + ticks),
//!                    SOLID for filled arrows,
//!                    TEXT for the label
//! 0
//! ENDSEC
//! 0
//! EOF
//! ```

use std::fmt::Write;

use crate::plugins::drafting::render::{DimPrimitive, TextAnchor};

/// DXF units codes (`$INSUNITS` header):
#[derive(Debug, Clone, Copy)]
pub enum DxfUnit {
    Inches,      // 1
    Millimetres, // 4
    Metres,      // 6
}

impl DxfUnit {
    fn code(self) -> u8 {
        match self {
            Self::Inches => 1,
            Self::Millimetres => 4,
            Self::Metres => 6,
        }
    }
}

/// Build a complete DXF document containing the supplied drawing primitives
/// (typically walls, edges) and dimensions (lists of primitives per annotation).
/// Both inputs are expected to be in the same target unit — the `unit`
/// parameter only affects the header. Keep inputs in paper-mm when the
/// receiver is a drawing viewer; keep inputs in model-mm when the file will be
/// consumed by a CAD tool at 1:1.
#[must_use]
pub fn export_dxf(
    unit: DxfUnit,
    extent_min: (f32, f32),
    extent_max: (f32, f32),
    drawing: &[DimPrimitive],
    dimensions: &[Vec<DimPrimitive>],
) -> String {
    let mut out = String::with_capacity(8192);
    write_header(&mut out, unit, extent_min, extent_max);
    write_tables(&mut out);
    write_entities(&mut out, drawing, dimensions);
    out.push_str("0\nEOF\n");
    out
}

fn write_group(out: &mut String, code: u16, value: &str) {
    let _ = writeln!(out, "{code}\n{value}");
}

fn write_header(out: &mut String, unit: DxfUnit, min: (f32, f32), max: (f32, f32)) {
    write_group(out, 0, "SECTION");
    write_group(out, 2, "HEADER");

    write_group(out, 9, "$ACADVER");
    write_group(out, 1, "AC1027"); // AutoCAD 2013

    write_group(out, 9, "$INSUNITS");
    write_group(out, 70, &unit.code().to_string());

    write_group(out, 9, "$MEASUREMENT");
    write_group(
        out,
        70,
        match unit {
            DxfUnit::Inches => "0",
            _ => "1",
        },
    );

    write_group(out, 9, "$EXTMIN");
    write_group(out, 10, &dxf_num(min.0));
    write_group(out, 20, &dxf_num(min.1));
    write_group(out, 30, "0.0");

    write_group(out, 9, "$EXTMAX");
    write_group(out, 10, &dxf_num(max.0));
    write_group(out, 20, &dxf_num(max.1));
    write_group(out, 30, "0.0");

    write_group(out, 9, "$DIMSCALE");
    write_group(out, 40, "1.0");

    write_group(out, 0, "ENDSEC");
}

fn write_tables(out: &mut String) {
    write_group(out, 0, "SECTION");
    write_group(out, 2, "TABLES");

    // LAYER table
    write_group(out, 0, "TABLE");
    write_group(out, 2, "LAYER");
    write_group(out, 70, "3"); // number of entries (approximate max)

    // Layer 0 (default, required)
    write_group(out, 0, "LAYER");
    write_group(out, 2, "0");
    write_group(out, 70, "0");
    write_group(out, 62, "7"); // white
    write_group(out, 6, "CONTINUOUS");

    // Model geometry layer
    write_group(out, 0, "LAYER");
    write_group(out, 2, "MODEL");
    write_group(out, 70, "0");
    write_group(out, 62, "7");
    write_group(out, 6, "CONTINUOUS");

    // Dimensions layer (per NCS: A-ANNO-DIMS)
    write_group(out, 0, "LAYER");
    write_group(out, 2, "A-ANNO-DIMS");
    write_group(out, 70, "0");
    write_group(out, 62, "2"); // yellow (ISO arch convention)
    write_group(out, 6, "CONTINUOUS");

    write_group(out, 0, "ENDTAB");

    write_group(out, 0, "ENDSEC");
}

fn write_entities(out: &mut String, drawing: &[DimPrimitive], dimensions: &[Vec<DimPrimitive>]) {
    write_group(out, 0, "SECTION");
    write_group(out, 2, "ENTITIES");

    // Walls / other drawing content on MODEL layer.
    for prim in drawing {
        write_primitive(out, prim, "MODEL");
    }

    // Dimensions on A-ANNO-DIMS layer.
    for dim in dimensions {
        for prim in dim {
            write_primitive(out, prim, "A-ANNO-DIMS");
        }
    }

    write_group(out, 0, "ENDSEC");
}

fn write_primitive(out: &mut String, prim: &DimPrimitive, layer: &str) {
    match prim {
        DimPrimitive::LineSegment { a, b, stroke_mm: _ } => {
            write_line(out, layer, a.x, a.y, b.x, b.y);
        }
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            stroke_mm: _,
        } => {
            let half = length_mm * 0.5;
            let c = rotation_rad.cos();
            let s = rotation_rad.sin();
            write_line(
                out,
                layer,
                pos.x - half * c,
                pos.y - half * s,
                pos.x + half * c,
                pos.y + half * s,
            );
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
            let perp = bevy::math::Vec2::new(-axis_u.y, axis_u.x);
            let half_w = width_mm * 0.5;
            let base_l = *tail + perp * half_w;
            let base_r = *tail - perp * half_w;
            if *filled {
                // SOLID: filled triangle (AutoCAD SOLID uses 4 points; for a
                // triangle, duplicate the last vertex).
                write_group(out, 0, "SOLID");
                write_group(out, 8, layer);
                write_group(out, 10, &dxf_num(tip.x));
                write_group(out, 20, &dxf_num(tip.y));
                write_group(out, 30, "0.0");
                write_group(out, 11, &dxf_num(base_l.x));
                write_group(out, 21, &dxf_num(base_l.y));
                write_group(out, 31, "0.0");
                write_group(out, 12, &dxf_num(base_r.x));
                write_group(out, 22, &dxf_num(base_r.y));
                write_group(out, 32, "0.0");
                write_group(out, 13, &dxf_num(base_r.x));
                write_group(out, 23, &dxf_num(base_r.y));
                write_group(out, 33, "0.0");
            } else {
                // Open triangle: 3 LINE entities.
                write_line(out, layer, tip.x, tip.y, base_l.x, base_l.y);
                write_line(out, layer, base_l.x, base_l.y, base_r.x, base_r.y);
                write_line(out, layer, base_r.x, base_r.y, tip.x, tip.y);
            }
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            // CIRCLE filled via HATCH is complex; simpler: a filled DXF CIRCLE
            // isn't a thing. Use SOLID covering a small square approximation.
            // For Phase 3 we accept an unfilled CIRCLE of the right radius.
            write_group(out, 0, "CIRCLE");
            write_group(out, 8, layer);
            write_group(out, 10, &dxf_num(pos.x));
            write_group(out, 20, &dxf_num(pos.y));
            write_group(out, 30, "0.0");
            write_group(out, 40, &dxf_num(*radius_mm));
        }
        DimPrimitive::Text {
            anchor,
            content,
            height_mm,
            rotation_rad,
            anchor_mode,
            font_family: _,
            color_hex: _,
        } => {
            // TEXT entity with alignment. Group code 72 = horizontal alignment:
            //   0=left, 1=center, 2=right, 4=middle
            // Group code 73 = vertical alignment:
            //   0=baseline, 1=bottom, 2=middle, 3=top
            let (h_align, v_align) = match anchor_mode {
                TextAnchor::CenterBaseline => (1, 0), // center horizontal, baseline vertical
                TextAnchor::Center => (1, 2),         // center horizontal, middle vertical
            };
            write_group(out, 0, "TEXT");
            write_group(out, 8, layer);
            // Group code 10 is insertion point (used when alignment=baseline-left).
            // Group code 11 is the alignment point (used when h_align > 0).
            // We set both to `anchor` for simplicity.
            write_group(out, 10, &dxf_num(anchor.x));
            write_group(out, 20, &dxf_num(anchor.y));
            write_group(out, 30, "0.0");
            write_group(out, 11, &dxf_num(anchor.x));
            write_group(out, 21, &dxf_num(anchor.y));
            write_group(out, 31, "0.0");
            write_group(out, 40, &dxf_num(*height_mm));
            write_group(out, 1, &dxf_escape_text(content));
            write_group(out, 50, &dxf_num(rotation_rad.to_degrees()));
            if h_align != 0 {
                write_group(out, 72, &h_align.to_string());
            }
            if v_align != 0 {
                write_group(out, 73, &v_align.to_string());
            }
        }
    }
}

fn write_line(out: &mut String, layer: &str, x1: f32, y1: f32, x2: f32, y2: f32) {
    write_group(out, 0, "LINE");
    write_group(out, 8, layer);
    write_group(out, 10, &dxf_num(x1));
    write_group(out, 20, &dxf_num(y1));
    write_group(out, 30, "0.0");
    write_group(out, 11, &dxf_num(x2));
    write_group(out, 21, &dxf_num(y2));
    write_group(out, 31, "0.0");
}

fn dxf_num(n: f32) -> String {
    // DXF prefers high-precision floats; 6 decimals is safe for mm drawings.
    let mut s = format!("{n:.6}");
    // Trim trailing zeros for compactness, but keep the decimal point.
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.push('0');
        }
    }
    s
}

fn dxf_escape_text(s: &str) -> String {
    // DXF TEXT (group code 1) disallows line breaks. Keep special chars as-is
    // except newlines, which we strip.
    s.replace('\n', " ").replace('\r', " ")
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
    fn dxf_has_required_sections() {
        let style = DimensionStyle::architectural_imperial();
        let input = DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(4.572, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.5, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let dxf = export_dxf(
            DxfUnit::Millimetres,
            (0.0, 0.0),
            (5000.0, 1000.0),
            &[],
            &[prims],
        );
        assert!(dxf.contains("SECTION"));
        assert!(dxf.contains("HEADER"));
        assert!(dxf.contains("TABLES"));
        assert!(dxf.contains("LAYER"));
        assert!(dxf.contains("A-ANNO-DIMS"));
        assert!(dxf.contains("ENTITIES"));
        assert!(dxf.contains("LINE"));
        assert!(dxf.contains("TEXT"));
        assert!(dxf.contains("EOF"));
    }

    #[test]
    fn dxf_header_includes_units() {
        let dxf = export_dxf(DxfUnit::Millimetres, (0.0, 0.0), (100.0, 100.0), &[], &[]);
        assert!(dxf.contains("$INSUNITS"));
        assert!(dxf.contains("\n4\n")); // mm = 4
    }

    #[test]
    fn dxf_mechanical_emits_solid_arrows() {
        let style = DimensionStyle::engineering_mm();
        let input = DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(0.08, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.012, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let dxf = export_dxf(
            DxfUnit::Millimetres,
            (0.0, 0.0),
            (100.0, 100.0),
            &[],
            &[prims],
        );
        assert!(dxf.contains("SOLID"), "filled arrows → SOLID");
    }

    #[test]
    fn dxf_text_uses_group_codes_for_alignment() {
        let style = DimensionStyle::architectural_imperial();
        let input = DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(4.572, 0.0, 0.0),
            offset: Vec3::new(0.0, 0.5, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let dxf = export_dxf(
            DxfUnit::Millimetres,
            (0.0, 0.0),
            (5000.0, 1000.0),
            &[],
            &[prims],
        );
        // Group code 72 (horizontal alignment) or 73 (vertical) should appear
        // somewhere in the TEXT entity output.
        assert!(dxf.contains("\n72\n") || dxf.contains("\n73\n"));
    }
}
