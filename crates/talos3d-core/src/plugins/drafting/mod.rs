//! Professional architectural and engineering drafting & dimensioning.
//!
//! This module supplies the pure rendering core for dimensioned drawings:
//! number formatting, style presets, per-dimension primitive generation, and
//! 2D export writers (SVG today; PDF and DXF in Phase 3).
//!
//! # Design
//!
//! A dimension's geometry is a pair of world-space endpoints plus an offset;
//! its visual presentation is a named [`DimensionStyle`]. Writers consume
//! [`DimPrimitive`] instructions produced by [`render_dimension`] — no writer
//! re-derives geometry, so SVG/PDF/DXF output is visually consistent.
//!
//! Four production presets come out of the box:
//!
//! - `architectural_imperial` — US feet-inches, 45° ticks
//! - `architectural_metric`   — ISO mm, 45° ticks
//! - `engineering_mm`         — ASME/ISO mechanical, filled arrows, decimal mm
//! - `engineering_inch`       — ASME decimal inch, filled arrows, ASME-style leading zero
//!
//! # Phase 1 scope
//!
//! This phase delivers the pure rendering core. Linear and Aligned dimensions
//! render to pixel-perfect SVG and all formatting paths are covered by tests.
//! Authored-entity integration (persistence, property panel, MCP tools, ECS
//! visibility) lands in Phase 1b; see `composed-finding-steele.md`.
//!
//! The dimensioning core has **no Bevy dependencies** beyond `bevy::math` for
//! Vec2/Vec3, and no I/O. That keeps the renderer unit-testable without a
//! running app.

pub mod annotation;
pub mod export_dxf;
pub mod export_svg;
pub mod format;
pub mod kind;
pub mod migration;
pub mod plugin;
pub mod render;
pub mod style;
pub mod visibility;

pub use annotation::{
    render_annotation, DimensionAnnotationFactory, DimensionAnnotationNode,
    DimensionAnnotationSnapshot, DRAFTING_DIMENSION_TYPE,
};
pub use export_dxf::{export_dxf, DxfUnit};
pub use export_svg::{render_dimensions_svg_document, render_dimensions_svg_fragment};
pub use format::NumberFormat;
pub use kind::{DimensionKind, DimensionKindTag};
pub use plugin::{
    visible_annotations, DraftingPlugin, DRAFTING_ANNOTATIONS_KEY, DRAFTING_CAPABILITY_ID,
};
pub use render::{render_dimension, DimPrimitive, DimensionInput, TextAnchor};
pub use style::{DimensionStyle, DimensionStyleRegistry, TextPlacement, Terminator};
pub use visibility::DraftingVisibility;

#[cfg(test)]
mod reference_test;
