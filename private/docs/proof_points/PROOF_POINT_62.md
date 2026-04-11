# Proof Point 62: Architectural Dimension Offsets

**Status**: Complete  
**Date**: 2026-04-11

## Goal

Dimension annotations should behave like architectural drawing dimensions:

- first click sets the start witness point
- second click sets the end witness point
- third click sets the offset distance from the measured geometry
- the dimension line, witness lines, and label remain readable in orthographic
  and isometric drawing views

## Target Outcome

- dimensions store explicit offset geometry rather than only automatic
  overhang
- the offset is author-controlled through dragging and editable numerically
- units remain configurable at document level and per-dimension override level
- the same authored dimensions appear in viewport exports and future sheet
  workflows
- MCP can place and edit drawing-ready dimensions without UI simulation

## Landed

- `DimensionLineNode` now persists `line_point` and exposes derived `offset`
  editing, so authored dimensions retain a true dragged placement outside the
  measured geometry.
- The live tool now follows the architectural 3-step gesture:
  start point, end point, then offset placement.
- Dimension visuals now render witness lines, extension overhang, and paper-legible
  labels in drawing exports.
- MCP `place_dimension_line` accepts either `line_point` or `offset`, alongside
  per-dimension unit and precision overrides.

## Verification

- Unit coverage for default offset synthesis, explicit offset parsing, geometry
  projection, visibility gating, and MCP round-trip.
- Live verification through the running app with:
  - front orthographic view
  - white paper background with grid off
  - a box plus width dimension offset below the geometry
  - cropped viewport screenshot confirming witness lines and readable label
