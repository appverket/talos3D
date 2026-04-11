# Proof Point 61: Drawing Views And Paper Export

**Status**: Implemented  
**Date**: 2026-04-11

## Goal

Talos3D should be able to render and export drawing-style viewport images from
the authored 3D model using orthographic front/top/right/isometric views,
white-paper presentation, and hidden-line-friendly visible edge overlays.

## Delivered

- renderer settings now include:
  - `visible_edge_overlay_enabled`
  - `grid_enabled`
  - `background_rgb`
  - `paper_fill_enabled`
- the modeling grid can be hidden as part of a drawing workflow
- white background and white-paper fill can be driven from the same public
  render settings surface used by MCP
- the camera toolbar now exposes a `Front` preset in addition to the existing
  orthographic views
- front/top/left/right/bottom/isometric states are valid orthographic camera
  targets through the named-view and render-control path
- visible-edge linework now evaluates orthographic visibility using
  parallel-view rays rather than perspective-style rays from a camera point
- viewport screenshots continue to crop to the 3D viewport, which keeps app
  chrome out of exported drawing images

## Why It Matters

This proof point establishes the first reliable bridge between the interactive
3D model and practical 2D drawing output. It is also the foundation for the
next two slices: dimension offsets and refined construction guides.

## Verification

- `cargo test -p talos3d-core render_settings_round_trip_and_validate --all-features`
- `cargo test -p talos3d-core visible_feature_edges_hide_back_edges_in_front_projection --all-features`
- `cargo test -p talos3d-core top_and_bottom_views_align_to_world_vertical_axis --all-features`
- live verification in a `model-api` app instance created a box, enabled
  white-paper drawing settings, restored front/top orthographic views, and
  confirmed via OS-level screen capture that the box rendered as a single
  visible rectangle without a second hidden back rectangle bleeding through
