# ADR-024: Coordinate Render Modes And Camera View State

## Status

Accepted

## Date

2026-04-12

## Context

Talos3D exposes camera controls, drawing presentation commands, a renderer
window, toolbar buttons, and an MCP model API. They are all expected to act on
the same runtime state.

The implementation had two design faults:

1. drawing presentation commands were mutating overlapping low-level renderer
   flags without a coherent toggle/restore boundary
2. `OrbitCamera.radius` was doing double duty as both perspective camera
   distance and orthographic zoom scale

Those shortcuts created visible coupling:

- switching to orthographic presets could zoom far out unexpectedly
- framing commands behaved differently than the user expected in orthographic
  views
- paper drawing lacked a reversible state boundary
- UI behavior looked fragile because high-level intent was encoded as scattered
  booleans

## Decision

Talos3D core will coordinate these concerns explicitly:

- paper drawing is treated as a toggleable presentation mode with a restorable
  baseline renderer state
- orthographic zoom is represented independently from perspective camera
  distance
- camera projection transitions preserve apparent framing instead of reusing the
  same scalar for incompatible projection semantics
- framing helpers compute orthographic fit directly instead of relying on the
  perspective-distance heuristic
- named views and model API camera payloads carry the orthographic zoom state so
  saved/restored views remain coherent

## Consequences

### Positive

- view presets, framing commands, and paper drawing no longer fight over
  unrelated state
- the UI and MCP surface can observe the same camera/render intent
- orthographic navigation becomes predictable
- paper drawing can be entered and exited safely

### Tradeoffs

- saved view payloads become slightly richer
- the camera code becomes more explicit because projection transitions must
  convert between perspective distance and orthographic scale
- legacy saved views need a compatibility fallback when they do not contain an
  explicit orthographic scale

## Follow-On Guidance

- future high-level presentation modes should be added through shared helpers
  instead of ad hoc command-local flag mutations
- public API structures should expose state that matters for behavior, not just
  whichever internal field happened to be reused at the time
