# Drawing Metadata and Section Views Plan

## Goal

Bring Talos3D's paper-mode drawing workflow into line with orthographic drafting
practice by separating authored model geometry from drawing metadata.

## Implemented In This Iteration

- dimensions persist as document-scoped drawing metadata
- section-view clipping planes persist as document-scoped drawing metadata
- legacy saved dimension and clipping-plane entity records are migrated into the
  drawing metadata store on load
- framing/model extents prefer authored geometry and stop treating dimensions as
  model geometry
- orthographic dimension placement resolves the dimension line outside the host
  element projection
- document unit and precision defaults are exposed in the renderer window for
  drawing labels and exports

## Standards Direction

The implementation direction follows common drafting guidance:

- dimensions should read outside the object outline wherever practical
- witness/extension lines should project from measured geometry to a separate
  dimension line
- section cuts are view-state notation, not authored solid geometry

## Next Steps

- add first-class section-line graphics and named section views
- add hatch/fill treatment for section cuts in paper mode
- add a dedicated drawing settings surface for units, precision, annotation
  visibility, and export defaults
- move drawing export from cropped viewport capture toward a more explicit sheet
  and view composition model when vector fidelity work begins
