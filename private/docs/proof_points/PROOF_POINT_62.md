# Proof Point 62: Architectural Dimension Offsets

**Status**: Planned  
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
