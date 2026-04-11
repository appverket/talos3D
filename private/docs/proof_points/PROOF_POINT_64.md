# Proof Point 64: Geometry-First Selection And Hidden Light Helpers

**Status**: Complete  
**Date**: 2026-04-11

## Goal

Editor selection should stay focused on authored geometry by default.
Authored scene lights must remain part of the lighting model, but they should
not behave like ordinary selectable solids unless the user explicitly exposes
them as viewport objects.

## Target Outcome

- light gizmos are hidden by default in the modeling viewport
- `Select All` and framing commands operate on geometry instead of silently
  including daylight rig helpers
- deselection is available through both `Esc` and `Ctrl/Cmd+D`
- when users intentionally expose light objects, they become selectable again
  through the same authored-light workflow

## Landed

- `SceneLightObjectVisibility` now persists with the document and defaults to
  hidden.
- Light hit-testing, selection outlines, and viewport gizmo drawing are gated
  behind that visibility state.
- The Lights window now exposes a direct `Show light objects in viewport`
  checkbox, and using `Select` in that window automatically reveals them.
- `core.select_all`, `core.zoom_to_extents`, `core.zoom_to_selection`, and MCP
  `frame_model` now exclude hidden light helpers.
- `core.deselect` now resets the pivot and is bound to `Esc` plus
  `Ctrl/Cmd+D`.

## Verification

- `cargo test -p talos3d-core select_all_skips_hidden_light_objects_by_default --all-features`
- `cargo test -p talos3d-core select_all_includes_light_objects_when_exposed --all-features`
- `cargo test -p talos3d-core scene_light_object_visibility_round_trips_through_json --all-features`
- `cargo test -p talos3d-core --all-features --no-run`
- live verification in a fresh `model-api` app instance:
  - created a box
  - confirmed `core.select_all` selected only the box
  - confirmed `core.deselect` cleared the selection
  - captured a viewport screenshot with no light gizmos exposed by default
