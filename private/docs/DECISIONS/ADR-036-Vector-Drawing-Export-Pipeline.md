# ADR-036: Vector Drawing Export Pipeline

**Status**: Accepted  
**Date**: 2026-04-12

## Context

ADR-035 established drawing views as orthographic projections of the 3D model
with paper-mode rendering, and specified PNG, PDF, and SVG as export targets.
The initial implementation captures the viewport as a raster screenshot and
embeds that image into PDF (as JPEG) and SVG (as base64 PNG). This delivers
functional exports quickly but has fundamental limitations for architectural
drawing output:

1. **No scale fidelity.** Building permit drawings must be printed at standard
   scales (1:50, 1:100) with measurable dimensions. Raster captures at screen
   resolution do not survive print scaling — lines alias and measurements become
   unreliable.

2. **No line weight control.** Architectural drawing conventions require
   distinct line weights: cut lines are thick, projection/silhouette lines are
   medium, dimension lines are thin. A raster capture flattens all lines to the
   same pixel width.

3. **No downstream editability.** Contractors and structural engineers expect
   vector output (DWG/DXF, or at minimum vector PDF) that they can import,
   measure, and annotate in their own tools.

4. **File size.** A full-page A3 at 300 DPI is ~25 MB as PNG. The equivalent
   vector drawing is typically 50–500 KB.

The existing edge detection system in `render_pipeline.rs` already computes
visible feature edges (silhouettes, boundary edges, sharp creases) with
hidden-line removal against scene triangles. This computation is the hard part
of producing an architectural line drawing. What is missing is projecting those
3D edge segments into 2D coordinates and writing them as vector primitives
instead of Gizmo lines.

## Decision

### 1. Extract edge computation into a reusable vector geometry pipeline

The visible-edge detection logic (`collect_visible_feature_segments`,
`collect_scene_triangles`, `edge_is_visible`, etc.) is refactored so that it
can produce `Vec<ProjectedEdge>` — 2D line segments with classified edge
types — in addition to drawing Gizmos for the live viewport.

The pipeline is:

```
3D Mesh + Camera State
    → collect scene triangles (existing)
    → detect feature edges per mesh (existing)
    → hidden-line removal via ray sampling (existing)
    → project surviving 3D segments through view·projection matrix → 2D
    → classify each edge: Silhouette, Crease, Boundary, SectionCut
    → collect dimension annotations as positioned 2D text + leader geometry
    → output: DrawingGeometry { edges, dimensions, viewport }
```

### 2. Edge types carry line-weight semantics

Each projected edge carries a classification that maps to architectural line
weight conventions:

| Edge type    | Architectural meaning        | Default weight |
|-------------|------------------------------|----------------|
| SectionCut  | Wall/slab cut by clip plane  | Heavy (0.7 mm) |
| Silhouette  | Object outline               | Medium (0.35 mm) |
| Crease      | Sharp fold in surface        | Medium (0.35 mm) |
| Boundary    | Open mesh edge               | Medium (0.35 mm) |
| Dimension   | Measurement annotation       | Light (0.18 mm) |

These weights are configurable through `DrawingExportSettings` but the defaults
follow ISO 128 / SIS conventions.

### 3. SVG export writes true vector paths

The SVG exporter groups edges by type into `<g>` elements with appropriate
`stroke-width` attributes. Dimension annotations render as `<text>` elements
positioned at the label midpoint. The SVG viewBox is derived from the
orthographic viewport bounds so that the drawing scales correctly when printed.

### 4. PDF export writes vector drawing operators

The PDF exporter writes path-construction operators (`m`, `l`, `S`) grouped by
line weight, with `w` (line width) set per edge type. Dimension text is placed
with `BT`/`ET` text operators using a standard base-14 font (Helvetica). The
MediaBox is sized to match the viewport aspect ratio at the target paper size.

### 5. Raster export remains screenshot-based

PNG and JPEG export continue to use the existing viewport-capture path. The
vector pipeline applies only to SVG and PDF, which are the formats where vector
fidelity matters.

### 6. Drawing scale is explicit

`DrawingExportSettings` carries a `drawing_scale` field (e.g. 100.0 for 1:100)
and a `paper_size` field. The orthographic viewport extent in world units,
divided by the drawing scale, determines the printed size. If the viewport
extent exceeds the paper at the chosen scale, the export warns but proceeds.

### 7. The pipeline is AI-driveable

The MCP `export_drawing` / `take_screenshot` tools gain an optional
`vector: bool` parameter (default true for SVG/PDF). The vector pipeline uses
the same camera state, clipping planes, and dimension annotations as the live
viewport, so AI-composed views export identically.

## Consequences

**Positive**

- SVG and PDF exports contain true vector geometry that survives print scaling
  and can be imported into CAD tools.
- Line weight conventions produce architecturally correct drawing output without
  manual post-processing.
- The same edge detection code serves both live viewport and export, ensuring
  visual fidelity between screen and paper.
- Drawing scale support enables standard-scale printing for building permits.

**Negative**

- The vector export is orthographic-only. Perspective vector export would
  require a different projection pipeline and is not in scope.
- Section fill (hatching on cut faces) is still deferred — the vector pipeline
  exports the cut edges but not the fill.
- Complex models with many edges will produce large SVG/PDF files. Line
  simplification or level-of-detail may be needed for very detailed models.

## Relationship To Existing Decisions

- **ADR-035 (Drawing Views)**: This ADR fulfils the vector-fidelity gap
  acknowledged in ADR-035. Raster export remains as-is.
- **ADR-027 (Clipping Planes)**: Section cut edges are classified as
  `SectionCut` with heavy line weight, matching architectural convention.
- **ADR-025 (Drawing Metadata)**: Dimension annotations are included in vector
  export from the same metadata scope.
- **ADR-024 (Render/View State)**: The vector pipeline reads the same
  `OrbitCamera` and `Projection` state as the live viewport.
