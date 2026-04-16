//! Sheet → DXF (AC1027). Paper-mm native, `$INSUNITS = 4`.
//!
//! Wraps the drafting plugin's existing `export_dxf` — which already
//! speaks AC1027 text DXF and `DimPrimitive` lists — by converting our
//! classified sheet lines into `DimPrimitive::LineSegment`s tagged with
//! the appropriate paper-mm weight.
//!
//! Because everything on the sheet is already paper millimetres, the DXF
//! produced here is a valid millimetre-unit drawing consumable by any
//! LibreCAD / BricsCAD / AutoCAD at 1:1 paper scale.

use bevy::math::Vec2;

use crate::plugins::{
    drafting::{export_dxf, DimPrimitive, DxfUnit},
    section_fill::{generate_hatch_lines, HatchPattern},
};

use super::sheet::{DraftingSheet, SheetHatch, SheetLine, SheetStroke};

/// Render a [`DraftingSheet`] to an AC1027 text-DXF string with paper-mm
/// coordinates.
#[must_use]
pub fn sheet_to_dxf(sheet: &DraftingSheet) -> String {
    let mut drawing: Vec<DimPrimitive> = sheet.lines.iter().map(line_to_primitive).collect();
    drawing.extend(sheet.hatches.iter().flat_map(hatch_to_primitives));
    let annotations: Vec<Vec<DimPrimitive>> = sheet.annotations.clone();

    let (min, max) = if sheet.bounds.is_valid() {
        (
            (sheet.bounds.min.x, sheet.bounds.min.y),
            (sheet.bounds.max.x, sheet.bounds.max.y),
        )
    } else {
        ((0.0, 0.0), (100.0, 100.0))
    };

    export_dxf(DxfUnit::Millimetres, min, max, &drawing, &annotations)
}

fn line_to_primitive(line: &SheetLine) -> DimPrimitive {
    DimPrimitive::LineSegment {
        a: Vec2::new(line.a.x, line.a.y),
        b: Vec2::new(line.b.x, line.b.y),
        stroke_mm: line.stroke.weight_mm(),
    }
}

fn hatch_to_primitives(hatch: &SheetHatch) -> Vec<DimPrimitive> {
    let polygon: Vec<[f32; 2]> = hatch.polygon.iter().map(|p| [p.x, p.y]).collect();
    let hatch_lines = match hatch.pattern {
        HatchPattern::NoFill | HatchPattern::SolidFill => Vec::new(),
        _ => generate_hatch_lines(&polygon, hatch.pattern, 1.0),
    };
    hatch_lines
        .into_iter()
        .map(|line| DimPrimitive::LineSegment {
            a: Vec2::new(line[0], line[1]),
            b: Vec2::new(line[2], line[3]),
            stroke_mm: SheetStroke::Hatch.weight_mm(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting_sheet::sheet::{SheetBounds, SheetHatch, SheetLine, SheetStroke};

    #[test]
    fn dxf_declares_mm_units() {
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
        let text = sheet_to_dxf(&s);
        assert!(text.contains("$INSUNITS"));
        // `4` is the millimetres code.
        assert!(text.contains("\n4\n"), "expected $INSUNITS 4 (mm)");
        // Coordinates must appear in millimetres, not some scaled value.
        assert!(text.contains("100.0"));
    }

    #[test]
    fn dxf_contains_hatch_lines_for_section_patterns() {
        let mut s = DraftingSheet::new(50.0);
        s.hatches.push(SheetHatch {
            polygon: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(20.0, 0.0),
                Vec2::new(20.0, 20.0),
                Vec2::new(0.0, 20.0),
            ],
            pattern: HatchPattern::DiagonalLines {
                angle_deg: 45.0,
                spacing_mm: 5.0,
            },
        });
        s.bounds = SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::new(20.0, 20.0),
        };
        let text = sheet_to_dxf(&s);
        let line_count = text.matches("\nLINE\n").count();
        assert!(
            line_count >= 2,
            "expected hatch geometry in DXF, got:\n{text}"
        );
    }
}
