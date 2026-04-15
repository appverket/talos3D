# ADR 025: Drawing Metadata Boundary

## Status

Accepted

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
- introduce a first-class drawing sheet/view model when paper-mode output moves
  beyond cropped viewport export
