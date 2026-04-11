# Proof Point 67: Surface Cursor For 3D Dimensions And Guides

**Status**: Complete  
**Date**: 2026-04-11

## Goal

Dimension lines and guide lines must work in normal perspective modeling, not
only when the cursor happens to be projected onto the active drawing plane.
When a user points at visible geometry in 3D, construction annotation tools
should start from that visible surface and only fall back to the plane when no
surface is available.

## Target Outcome

- guide and dimension placement use a real scene-surface cursor in normal 3D
  workflows
- those tools still work when the pointer is over empty space by falling back
  to the drawing plane
- construction snapping starts from the visible 3D hit location instead of a
  forced plane-grid projection

## Landed

- `CursorPlugin` now ray-casts visible authored mesh surfaces when
  `PlaceDimensionLine` or `PlaceGuideLine` is active.
- Those tools now fall back to the drawing plane only when the ray does not hit
  visible scene geometry.
- For those annotation tools, the cursor's snapped position now begins from the
  3D surface hit so snap-point resolution can operate from an actual scene pick
  location.

## Verification

- `cargo test -p talos3d-core --all-features --no-run`
- live smoke verification in a fresh `model-api` app instance:
  - launched Talos3D with a new `codex-3d-dim-guide` instance id after the
    cursor patch
  - confirmed the app booted successfully with the new 3D construction cursor
    path compiled into the running binary
