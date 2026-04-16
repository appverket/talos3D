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

use crate::plugins::drafting::{export_dxf, DimPrimitive, DxfUnit};

use super::sheet::{DraftingSheet, SheetLine};

/// Render a [`DraftingSheet`] to an AC1027 text-DXF string with paper-mm
/// coordinates.
#[must_use]
pub fn sheet_to_dxf(sheet: &DraftingSheet) -> String {
    let drawing: Vec<DimPrimitive> = sheet.lines.iter().map(line_to_primitive).collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting_sheet::sheet::{SheetBounds, SheetLine, SheetStroke};

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
}
