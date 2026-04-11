# ADR-035: Drawing Views And Paper Export

**Status**: Accepted  
**Date**: 2026-04-11

## Context

Talos3D needs a repeatable way to present 3D authored geometry as 2D drawing
views. The requirement is not "wireframe mode" in the game-engine sense. The
requirement is architectural drawing output:

- orthographic front, top, bottom, left, and right projections
- orthographic isometric views for schematic drawing sets
- visible linework without back-edge clutter
- white-paper presentation without modeling-grid noise
- annotations such as dimensions remaining visible in export
- direct control through MCP so an AI can compose, inspect, and export the
  same views as the desktop UI

The existing renderer already exposes shaded lookdev controls, and the camera
system already has a perspective/isometric split. What was missing was an
drawing-specific contract that ties camera, renderer, capture, and annotation
behavior together.

## Decision

### 1. Drawing views remain model-space views in the first slice

Talos3D will not introduce a separate paper-space document model in this slice.
Drawing views are produced from the active modeling viewport. This keeps the
first implementation composable with the current editor, avoids duplicate scene
state, and keeps MCP semantics simple.

### 2. Orthographic view presets are first-class camera states

The camera system must support canonical orthographic presets for:

- front
- top
- bottom
- left
- right
- isometric

These are camera states, not special render passes. They can be driven from the
UI or restored through named views and MCP.

### 3. "Paper drawing" is a renderer preset, not a separate renderer

A drawing view is defined by render settings rather than a second pipeline:

- white background
- construction grid hidden
- white unlit material fill for modeled geometry
- visible-edge overlay enabled
- contour-only and wireframe-only overlays optional, but not required for the
  default drawing contract

This keeps the drawing view compatible with the same authored entities and
annotation systems as the shaded view.

### 4. Hidden-line removal is based on visible feature edges

The exported line drawing should show:

- silhouette edges
- boundary edges
- sharp crease edges that are visible from the active camera

Back edges are not part of the default drawing contract. Visibility is
evaluated against scene triangles so the output behaves like a drawing view,
not a raw mesh wireframe.

### 5. Export captures the active viewport, not the whole app window

The export path crops to the modeling viewport:

- menu bars, toolbars, assistant UI, and property panels are excluded
- authored viewport annotations such as dimensions remain included
- the first multi-format export contract writes the same cropped drawing view
  to PNG directly and to PDF/SVG by embedding that exact capture, so there is
  no second drawing renderer to drift from the live viewport result

This makes exported images usable as drawing assets without post-cropping.

### 6. Drawing views are AI-visible and AI-driveable

The following must be public through MCP:

- named view save/list/restore/update/delete
- renderer settings including background, grid visibility, paper fill, and
  edge overlays
- viewport screenshot export
- drawing export to PDF, PNG, and SVG

This is required for Agent Experience parity. An AI should be able to create a
drawing-ready view without simulating mouse interaction.

### 7. Follow-on annotation work builds on this contract

Dimension offsets, guide-line inference, protractor-driven angular guides, and
future sheet composition all build on the drawing-view contract above. They
must remain authored, visible, and exportable from the same viewport.

## Consequences

**Positive**

- Orthographic drawing export is available without introducing a separate sheet
  authoring subsystem.
- UI and MCP use the same camera/render/capture contract.
- White-paper views can be composed incrementally from existing renderer
  components.
- Future dimension and guide-line work has a stable visual/export target.

**Negative**

- This slice does not yet create multi-view sheets or title-block layouts.
- Hidden-line output quality depends on mesh-derived feature edges and will
  need continued refinement on more complex topology.
- Paper presentation and lookdev now share renderer state, so defaults and UX
  must clearly distinguish modeling from drawing tasks.
