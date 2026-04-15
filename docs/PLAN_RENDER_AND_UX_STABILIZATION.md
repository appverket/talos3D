# Render And UX Stabilization Plan

Date: 2026-04-12

## Context

The current Talos3D core runtime exposes a clean public architecture on paper:

- commands are the canonical user edit surface
- the MCP model API should observe and drive the same behavior as the UI
- render state, camera state, and UI chrome should compose instead of leaking
  hidden coupling

This stabilization pass exists because recent regressions showed that those
boundaries are not robust enough in the current implementation. In particular,
view rendering modes, camera presets, export behavior, and sidebar layout are
still coupled through low-level state in ways that produce surprising UX.

## User-Reported Issues

- Paper drawing should be a toggle with a clear path back to the default
  renderer.
- After paper view + outline, switching to front/left/isometric zooms out too
  far from the current grid context.
- In that same state:
  - paper view is not fully white
  - wireframe can make geometry appear to disappear
  - zoom to extents does not work
  - zoom to selection does not work
  - drawing export only appears to offer PNG, with no obvious SVG/PDF choice
- Several commands still miss icons:
  - Create Fillet
  - Create Chamfer
  - Dimension
  - Dimensions
  - Guide Lines
- The assistant configuration sidebar is too constrained, controls can be cut
  off, and the save behavior is unclear when entering an API key.

## Stabilization Goals

1. Make render/view commands coordinate through explicit shared helpers instead
   of partially-overlapping flag mutations.
2. Separate orthographic zoom from perspective camera distance so view presets
   and framing commands preserve intent.
3. Ensure the API, commands, and UI observe the same render and camera state.
4. Remove brittle UI width caps from the assistant sidebar and make persistence
   behavior explicit in the UX.
5. Verify each fix through targeted tests and model API checks where the API can
   observe the behavior.

## Verification Checklist

- [ ] Paper drawing toggles on and off cleanly.
- [ ] Paper drawing restores the previous renderer state, or falls back to the
      default renderer if no prior state exists.
- [ ] Front, left, right, top, bottom, and isometric views preserve the current
      apparent zoom when switching from perspective.
- [ ] Zoom to extents and zoom to selection frame correctly in orthographic
      views.
- [ ] Paper drawing renders with a true white background and paper fill.
- [ ] Wireframe remains legible in paper drawing mode.
- [ ] Export has an explicit SVG/PDF path in the UX and still works through the
      model API by file extension.
- [ ] Fillet/chamfer/dimension/guide-line commands have icons.
- [ ] Assistant sidebar width is not capped to a fixed hard maximum.
- [ ] Assistant settings remain usable at narrow widths and clearly state that
      changes save automatically.
