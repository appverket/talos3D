# Proof Point 58: Appearance Controls And Browser-Safe Bundled Catalogs

**Status**: Implemented  
**Date**: 2026-04-09

## Goal

Talos3D should expose a richer appearance-control surface for both humans and
agents while keeping reusable bootstrap content compatible with native and
browser deployments.

## Delivered

- slider contrast in the chrome theme is increased so numeric controls remain
  legible against the dark UI
- egui window title text now matches menu text sizing instead of using a larger
  heading treatment
- built-in materials are seeded with stable ids, including the MAIBEC cedar
  siding material and a blue-tint glazing material
- bundled definition libraries load automatically at startup from app data
  rather than depending on a local filesystem convention
- project persistence excludes bundled catalogs and built-in materials, then
  re-seeds them on load so project files stay portable
- hosted window placement now inherits wall orientation in the Definitions
  browser flow
- hosted/bundled definition parts can carry material assignments into generated
  occurrence geometry
- material authoring exposes a broader Bevy-backed control surface:
  specular tint, transmission, thickness, IOR, attenuation, clearcoat,
  anisotropy, unlit, fog participation, and depth bias
- viewport renderer settings are exposed through MCP via
  `get_render_settings` and `set_render_settings`
- the same renderer state is exposed in a Renderer window under the View menu

## Why It Matters

This proof point validates two adjacent principles:

1. reusable startup content must work in a browser-safe deployment model
2. renderer and material tuning must be AI-visible rather than hidden in
   engine-only code or UI-only affordances

Together they make appearance authoring more realistic for both local usage and
future SaaS/browser deployment.

## Verification

- `cargo test -p talos3d-core --all-features --no-run`
- `cargo test -p talos3d-core render_settings_round_trip_and_validate --all-features`
- `cargo run --all-features`
