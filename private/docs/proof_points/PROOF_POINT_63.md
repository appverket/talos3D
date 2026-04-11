# Proof Point 63: Inferred Guide Lines And Protractor Workflow

**Status**: Complete  
**Date**: 2026-04-11

## Goal

Construction guides should match professional reference-line workflows rather
than acting as simple grid-plane lines.

## Outcome

- guide lines can be hosted by the selected face plane or, when no face is
  selected, by the current drawing plane / hovered face plane
- guide direction can be inherited from a hovered face edge or a selected guide
  line
- drag placement snaps through the existing snap system and also edge-snaps to
  the nearest face edge when appropriate
- `X`, `Y`, and `Z` lock guide direction to projected global axes while keeping
  the guide on its host plane
- angular guide creation supports a protractor-style preview with live angle
  feedback and direct numeric angle entry
- MCP `place_guide_line` now accepts either a direct `direction`, a `through`
  point, or an angular contract (`reference_direction`, `angle_degrees`,
  `plane_normal`)

## Notes

- The tool now stays active after placing a guide. `Esc` returns to Select.
- Angle snapping is applied with `Ctrl` during drag; numeric entry bypasses drag
  ambiguity entirely.
- The host plane is not persisted as separate authored data. A placed guide
  only needs `anchor` plus `direction`; lying on a face/plane is implied by the
  authored line geometry itself.
