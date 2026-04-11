# Proof Point 65: View Commands And Drawing Mode Toolbars

**Status**: Complete  
**Date**: 2026-04-11

## Goal

Camera views and drawing-style render modes should be first-class editor
commands, not hidden inside ad hoc UI. Users and agents need the same
projection, viewpoint, and drawing-presentation surface through menus,
toolbars, the command palette, and MCP command invocation.

## Target Outcome

- dedicated `Views` and `Render` toolbars are visible by default
- front, back, top, bottom, left, right, and isometric views are explicit
  commands
- perspective and orthographic projection are explicit commands
- drawing presentation controls are explicit commands:
  paper preset, grid, outline, and wireframe
- menu and toolbar hover affordances show the command label plus any shortcut
  and hint text

## Landed

- `view.presets` toolbar now exposes projection commands and view presets.
- `view.render` toolbar now exposes paper drawing, grid, outline, and
  wireframe commands.
- Camera now supports a true `Back` preset alongside front/top/bottom/left/right/isometric.
- View and drawing commands are registered in the command registry, so they
  automatically appear in the menu bar, command palette, toolbar surfaces, and
  MCP `invoke_command`.
- Menu and toolbar tooltips now show richer command help instead of only a
  terse label.

## Verification

- `cargo test -p talos3d-core top_and_bottom_views_align_to_world_vertical_axis --all-features`
- `cargo test -p talos3d-core paper_preset_enables_white_background_and_outline_mode --all-features`
- `cargo test -p talos3d-core --all-features --no-run`
- live verification in a fresh `model-api` app instance:
  - `list_toolbars` reported visible `view.presets` and `view.render` toolbars
  - `invoke_command` successfully drove `view.front`, `view.isometric`,
    `view.apply_paper_preset`, and `view.toggle_wireframe`
  - cropped viewport screenshots confirmed the resulting front, paper, and
    wireframe states in the running app
