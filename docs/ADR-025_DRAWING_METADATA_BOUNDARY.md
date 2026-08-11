# ADR 025: Drawing Metadata Boundary

## Status

Accepted; amended by ADR-026

## Context

Talos3D mixed authored model entities and drawing annotations too freely.

That showed up as:

- dimensions affecting model framing and extents
- section views behaving like authored content instead of view metadata
- paper-mode exports depending on editor state that was not architecturally separated
- orthographic dimensions being placed inside geometry instead of outside the projected silhouette

Mechanical and architectural drafting conventions treat dimensions, section cuts,
and similar notation as view-layer metadata. The authored model remains the
source geometry; drawing annotations explain, measure, or filter that geometry.

## Decision

Talos3D will treat drawing annotations as runtime entities with document-scoped
metadata persistence rather than as authored model entities.

Specifically:

- `AuthoredEntity::scope()` distinguishes `AuthoredModel` from `DrawingMetadata`.
- save/load of the authored project file only persists `AuthoredModel` entities
  through the normal entity list.
- drawing metadata persists through `DocumentProperties.domain_defaults`.
- dimensions and section-view clipping planes are restored from that document
  metadata into runtime entities on load/update.
- model summaries, framing, and authored extents prefer `AuthoredModel`
  snapshots and ignore drawing metadata unless an operation explicitly targets
  metadata-only selection.
- orthographic dimension placement resolves the visible dimension line against
  the host element's projected bounds so witness lines land outside the object
  silhouette by default.
- document display units and precision act as the default formatting contract
  for drawing annotations and exports.

## Consequences

Positive:

- drawing notation no longer pollutes authored model persistence semantics
- orthographic drawing behavior aligns better with drafting practice
- section views and dimensions can be toggled independently of authored geometry
- PNG, PDF, and SVG drawing exports can use the same metadata layer contract

Tradeoffs:

- drawing metadata now has a second persistence path
- migration is required for legacy projects that stored dimensions or clipping
  planes in the authored entity list
- future drawing features should follow this boundary consistently rather than
  bypass it for short-term convenience

## Follow-On Work

- add dedicated drawing settings UI beyond the renderer window section
- extend the metadata model to richer section graphics, callouts, and hatch
  behavior
- implement the first-class `Draft` metadata container, plane, membership, and
  generic annotation primitives specified by ADR-026

## Amendment: Draft container and derived DrawingScene

ADR-026 makes the previously deferred drawing view first-class. `Draft` is the
durable drawing-metadata container and may reference ordinary authored 3D model
entities without cloning or owning their geometry. Lines, rectangles, circles,
text, dimensions, and related 2D annotations persist as drawing metadata.

The normalized `DrawingScene`, paper-layout `DraftingSheet`, raster previews,
projected silhouettes, and export bytes are derived artifacts. They are rebuilt
from the authored model plus `Draft`; none is a second persistence source or
authoring document.
