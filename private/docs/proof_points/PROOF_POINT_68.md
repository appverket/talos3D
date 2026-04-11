# Proof Point 68: Multi-Format Drawing Export

**Status**: Implemented  
**Date**: 2026-04-11

## Goal

Talos3D should export the current drawing-ready viewport directly to practical
delivery formats: PNG for raster workflows, PDF for document workflows, and SVG
for vector-oriented interchange.

## Delivered

- added a dedicated drawing export command in the File surface:
  - `core.export_drawing`
- exposed that command in the core toolbar and File menu
- added a shared viewport export implementation that:
  - crops to the modeling viewport
  - preserves authored viewport annotations such as dimensions
  - supports `png`, `pdf`, and `svg`
  - accepts `svd` as a forgiving alias for `svg`
- kept PNG as the native raster export
- generated PDF and SVG from the same cropped viewport capture so exported
  drawing output matches the live viewport result exactly
- exposed the same capability through MCP as `export_drawing`
- aligned `take_screenshot` with the same exporter so capture behavior no
  longer diverges from drawing export behavior by format

## Why It Matters

This closes the loop on the first drawing workflow. A user or AI can now set up
an orthographic or isometric drawing view, enable paper-style presentation and
hidden-line overlays, and export the result directly in the format best suited
to downstream use.

## Verification

- `cargo test -p talos3d-core viewport_capture_maps_ui_insets_to_image_bounds --all-features`
- `cargo test -p talos3d-core export_format_defaults_to_png_and_accepts_svg_aliases --all-features`
- `cargo test -p talos3d-core svg_document_embeds_png_payload --all-features`
- `cargo test -p talos3d-core pdf_document_embeds_single_raster_page --all-features`
- `cargo test -p talos3d-core render_settings_round_trip_and_validate --all-features`
- `cargo test -p talos3d-core --all-features --no-run`
