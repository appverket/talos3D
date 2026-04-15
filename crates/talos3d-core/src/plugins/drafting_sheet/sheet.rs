//! Paper-millimetre 2D drawing document.
//!
//! Every numeric value in a [`DraftingSheet`] is paper millimetres. No
//! "viewport pixels", no "NDC", no "canvas units at some implicit scale":
//! writers can consume the sheet with 1:1 unit mapping to their native
//! output (SVG user units, PDF points via `mm * 72/25.4`, DXF with
//! `$INSUNITS = 4`).
//!
//! See PROOF_POINT_69.md for context.
//!
//! Sheets are *derived* — captured from a 3D [`SheetView`] of the current
//! world, not authored directly. Annotations stay 3D-world-anchored on the
//! source side; the capture pass projects them into paper-mm before
//! calling the drafting renderer.

use bevy::math::{Vec2, Vec3};

use crate::plugins::drafting::DimPrimitive;
use crate::plugins::section_fill::HatchPattern;

// ─── Stroke classification ────────────────────────────────────────────────

/// Line weight class following classical architectural-drafting hierarchy.
///
/// Each class has one canonical paper-millimetre weight (see
/// [`SheetStroke::weight_mm`]). Consumers (SVG/PDF/DXF writers) are free
/// to pick their own mapping but should preserve the *ordering*:
///
/// `SectionCut ≥ Silhouette ≥ Dimension`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetStroke {
    /// Outline where the section plane slices through solid material.
    /// Heaviest weight on the page.
    SectionCut,
    /// Object outline — the visible boundary of a piece of geometry.
    Silhouette,
    /// Feature edge between two faces whose normals differ sharply.
    Crease,
    /// Manifold boundary (free edge) — visible rim of an open mesh.
    Boundary,
    /// Dimension line, extension line, and tick. Thinnest structural
    /// stroke on the drawing.
    Dimension,
    /// Hatching inside a section-fill region. Same weight as Dimension.
    Hatch,
}

impl SheetStroke {
    /// Canonical architectural line weight in paper millimetres.
    #[must_use]
    pub fn weight_mm(self) -> f32 {
        match self {
            Self::SectionCut => 0.70,
            Self::Silhouette | Self::Crease | Self::Boundary => 0.35,
            Self::Dimension | Self::Hatch => 0.18,
        }
    }
}

// ─── Primitives ───────────────────────────────────────────────────────────

/// A classified straight line segment, paper-mm coordinates.
#[derive(Debug, Clone, Copy)]
pub struct SheetLine {
    pub a: Vec2,
    pub b: Vec2,
    pub stroke: SheetStroke,
}

/// A section-fill region — the polygonal footprint of a cut surface, with
/// the hatch pattern chosen by the source material.
///
/// `polygon` is in paper-mm, winding not specified (writers tolerate
/// both). An empty polygon means the region is suppressed.
#[derive(Debug, Clone)]
pub struct SheetHatch {
    pub polygon: Vec<Vec2>,
    pub pattern: HatchPattern,
}

/// Paper-space bounding box in paper millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SheetBounds {
    pub min: Vec2,
    pub max: Vec2,
}

impl SheetBounds {
    /// Bounds that swallow no points — [`SheetBounds::include`] the first
    /// point and it becomes the (degenerate) bounding rect of that point.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            min: Vec2::splat(f32::INFINITY),
            max: Vec2::splat(f32::NEG_INFINITY),
        }
    }

    pub fn include(&mut self, p: Vec2) {
        if p.x.is_finite() && p.y.is_finite() {
            self.min.x = self.min.x.min(p.x);
            self.min.y = self.min.y.min(p.y);
            self.max.x = self.max.x.max(p.x);
            self.max.y = self.max.y.max(p.y);
        }
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.min.x.is_finite()
            && self.min.y.is_finite()
            && self.max.x >= self.min.x
            && self.max.y >= self.min.y
    }

    /// Width of the bounding box in paper millimetres.
    #[must_use]
    pub fn width(&self) -> f32 {
        (self.max.x - self.min.x).max(0.0)
    }

    /// Height of the bounding box in paper millimetres.
    #[must_use]
    pub fn height(&self) -> f32 {
        (self.max.y - self.min.y).max(0.0)
    }

    /// Outward-grown copy of this bounding box (`margin_mm` added on every
    /// side). Paper margins for the export canvas live here.
    #[must_use]
    pub fn inflated(&self, margin_mm: f32) -> Self {
        Self {
            min: self.min - Vec2::splat(margin_mm),
            max: self.max + Vec2::splat(margin_mm),
        }
    }
}

impl Default for SheetBounds {
    fn default() -> Self {
        Self::empty()
    }
}

// ─── View spec ────────────────────────────────────────────────────────────

/// A 3D view that gets flattened into a [`DraftingSheet`].
///
/// Only orthographic views are valid for drafting — perspective
/// foreshortening would make dimensions nonsense. `capture_sheet` refuses
/// anything else.
#[derive(Debug, Clone)]
pub struct SheetView {
    /// World-space camera position.
    pub eye: Vec3,
    /// World-space point the camera is aimed at.
    pub target: Vec3,
    /// World-space up vector (usually `Vec3::Y`).
    pub up: Vec3,
    /// Orthographic view-frustum height in world metres. Width derives
    /// from this and the aspect ratio.
    pub ortho_height_m: f32,
    /// Orthographic aspect ratio (width / height). Canvas shape.
    pub aspect: f32,
    /// Drawing scale denominator, so `1:N`. Paper size derives as
    /// `ortho_*_m * 1000 / N`.
    pub scale_denominator: f32,
    /// Paper margin around the bounding box, in paper millimetres.
    pub margin_mm: f32,
}

impl SheetView {
    /// Paper-millimetre width of the view-frustum projection at this scale.
    #[must_use]
    pub fn frustum_width_mm(&self) -> f32 {
        self.ortho_height_m * self.aspect * 1000.0 / self.scale_denominator
    }

    /// Paper-millimetre height of the view-frustum projection at this scale.
    #[must_use]
    pub fn frustum_height_mm(&self) -> f32 {
        self.ortho_height_m * 1000.0 / self.scale_denominator
    }

    /// Paper-millimetres per world-metre at this scale (e.g. 20 mm/m for
    /// 1:50).
    #[must_use]
    pub fn paper_mm_per_world_m(&self) -> f32 {
        1000.0 / self.scale_denominator
    }
}

// ─── The sheet ────────────────────────────────────────────────────────────

/// A 2D drawing in paper millimetres, derived from a 3D [`SheetView`].
///
/// Construct via `capture::capture_sheet`. Consume via the
/// format-specific writers (`sheet_to_svg`, `sheet_to_pdf`,
/// `sheet_to_dxf`, `sheet_to_png`).
#[derive(Debug, Clone)]
pub struct DraftingSheet {
    /// Paper bounding box of all emitted content, plus the view's
    /// `margin_mm`. Writers map this to viewBox / MediaBox / `$EXTMIN..MAX`.
    pub bounds: SheetBounds,
    /// Classified line segments.
    pub lines: Vec<SheetLine>,
    /// Section-fill regions with their hatch pattern.
    pub hatches: Vec<SheetHatch>,
    /// Rich drafting annotations — already rendered into paper-mm
    /// `DimPrimitive`s by the drafting renderer. The writers can emit
    /// these directly.
    pub annotations: Vec<Vec<DimPrimitive>>,
    /// 1 ∶ N scale this sheet was captured at. Persisted so writers can
    /// embed it (e.g. in the SVG `<title>` and the DXF title block).
    pub scale_denominator: f32,
}

impl DraftingSheet {
    pub fn new(scale_denominator: f32) -> Self {
        Self {
            bounds: SheetBounds::empty(),
            lines: Vec::new(),
            hatches: Vec::new(),
            annotations: Vec::new(),
            scale_denominator,
        }
    }

    /// Fold all primitive extents into `self.bounds`. Writers call this
    /// once at the end of a capture pass (`capture_sheet` already does).
    pub fn recompute_bounds(&mut self, margin_mm: f32) {
        let mut bounds = SheetBounds::empty();
        for line in &self.lines {
            bounds.include(line.a);
            bounds.include(line.b);
        }
        for hatch in &self.hatches {
            for p in &hatch.polygon {
                bounds.include(*p);
            }
        }
        for dim in &self.annotations {
            for prim in dim {
                for p in primitive_extents(prim) {
                    bounds.include(p);
                }
            }
        }
        if bounds.is_valid() {
            bounds = bounds.inflated(margin_mm);
        }
        self.bounds = bounds;
    }
}

pub(crate) fn primitive_extents(prim: &DimPrimitive) -> Vec<Vec2> {
    match prim {
        DimPrimitive::LineSegment { a, b, .. } => vec![*a, *b],
        DimPrimitive::Tick { pos, .. } | DimPrimitive::Dot { pos, .. } => vec![*pos],
        DimPrimitive::Arrow { tip, tail, .. } => vec![*tip, *tail],
        DimPrimitive::Text { anchor, .. } => vec![*anchor],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_hierarchy_follows_arch_convention() {
        assert!(SheetStroke::SectionCut.weight_mm() >= SheetStroke::Silhouette.weight_mm());
        assert!(SheetStroke::Silhouette.weight_mm() >= SheetStroke::Dimension.weight_mm());
        assert!(SheetStroke::Crease.weight_mm() >= SheetStroke::Dimension.weight_mm());
        assert!(SheetStroke::Boundary.weight_mm() >= SheetStroke::Dimension.weight_mm());
    }

    #[test]
    fn sheet_view_paper_scale_is_1mm_per_world_m_at_1_1000() {
        let view = SheetView {
            eye: Vec3::Z,
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 4.0,
            aspect: 1.5,
            scale_denominator: 1000.0,
            margin_mm: 10.0,
        };
        assert!((view.paper_mm_per_world_m() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sheet_view_paper_scale_is_20mm_per_world_m_at_1_50() {
        let view = SheetView {
            eye: Vec3::Z,
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 4.0,
            aspect: 1.5,
            scale_denominator: 50.0,
            margin_mm: 10.0,
        };
        assert!((view.paper_mm_per_world_m() - 20.0).abs() < 1e-6);
    }

    #[test]
    fn bounds_grow_monotonically() {
        let mut b = SheetBounds::empty();
        b.include(Vec2::new(10.0, 5.0));
        b.include(Vec2::new(-3.0, 12.0));
        assert!(b.is_valid());
        assert!((b.min - Vec2::new(-3.0, 5.0)).length() < 1e-6);
        assert!((b.max - Vec2::new(10.0, 12.0)).length() < 1e-6);
        assert!((b.width() - 13.0).abs() < 1e-6);
        assert!((b.height() - 7.0).abs() < 1e-6);
    }

    #[test]
    fn bounds_empty_is_invalid_and_fold_detects_no_content() {
        let b = SheetBounds::empty();
        assert!(!b.is_valid());
    }

    #[test]
    fn bounds_inflation_grows_by_margin_on_all_sides() {
        let b = SheetBounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 50.0),
        }
        .inflated(5.0);
        assert_eq!(b.min, Vec2::new(-5.0, -5.0));
        assert_eq!(b.max, Vec2::new(105.0, 55.0));
    }
}
