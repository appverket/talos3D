# Terrain-Conforming Foundations & Building Planting

Decision: [ADR-059](../../decisions/ADR-059-Terrain-Conforming-Foundations-And-Building-Planting-Mode.md).
Proof points: PP-PLANT-A…E (`proof_points/PROOF_POINT_212..216.md`).

A **conforming solid** is a footprint extruded *down* onto the terrain: a flat
horizontal top and an underside that is the inverse of the grade beneath it. Used
as a foundation, it lets a building "hug" the surface — the terrain itself is the
foundation's lower boundary.

## Pieces (all in `talos3d-terrain`, domain-neutral)

- **`TerrainHeightfield`** (`heightfield.rs`) — a regular IDW grid + inside-boundary
  mask derived from a `TerrainSurface`. O(1) `height_at`, `max_over(footprint)`,
  `sample_grid`. Rebuilt as a component whenever the surface changes. This is the
  query layer everything else reads (decoupled from the render mesh).
- **`ConformingSolid`** (`conforming.rs`) — authored entity. A rectangular
  footprint (`position`, `half_extents`, `yaw`) over a target `surface_id`, with
  `min_thickness` and `max_depth`. Derived watertight mesh:
  - `Y_top = max(grade under footprint) + min_thickness` (flat top; thinnest point
    is exactly `min_thickness`).
  - underside `= max(surface_height, Y_top - max_depth)` (benches flat where grade
    dips more than `max_depth` below the top; thickness never exceeds `max_depth`).
  - vertical perimeter walls.
- **Planting** (`planting.rs`) — reversible `plant_on_surface` /
  `unplant_on_surface` commands.

## Planting behaviour

- **Y rides the surface.** Only `position` (XZ) and `yaw` are authored; `Y_top`
  is derived. Moving the solid in XZ (drag with the move tool, or set the
  `position` property) re-conforms the underside and re-seats the flat top at
  `max(grade)+min_thickness`. Full conforming-mesh rebuild is ~0.75 ms (debug,
  ~3.3k tris), so live drag needs no throttling.
- **Plant an existing object.** `plant_on_surface` creates a hugging foundation
  under a target's footprint, raises the target so its base sits on the
  foundation's flat top, and (optionally) **hides** an existing foundation.
  `unplant_on_surface` reverses all three, so it is non-destructive.
- **Reactivity.** Editing the surface invalidates its `TerrainHeightfield`, which
  re-marks dependent conforming solids for rebuild — planted foundations follow
  grade edits automatically.

## MCP surface

```jsonc
// Create a hugging foundation directly:
create_entity { "type": "conforming_solid", "surface_id": <id>,
                "position": [x, z], "half_extents": [hx, hz],
                "min_thickness": 0.5, "max_depth": 3.0, "resolution": 0.5 }

// Move / re-conform it (Y auto-adjusts):
set_property  { "element_id": <id>, "property_name": "position", "value": [x, 0, z] }
set_property  { "element_id": <id>, "property_name": "min_thickness", "value": 0.4 }

// Plant / unplant an existing object (reversible):
invoke_command { "command_id": "terrain.plant_on_surface",
                 "parameters": { "target_id": <obj>, "surface_id": <id>,
                                 "min_thickness": 0.5, "hide_element_id": <old_foundation?> } }
invoke_command { "command_id": "terrain.unplant_on_surface",
                 "parameters": { "target_id": <obj> } }
```

## Known follow-ups

- **Architecture-domain `HuggingFoundation` recipe** (PP-PLANT-E remainder):
  register the conforming foundation as a curated recipe / parametric type in
  `talos3d-architecture` so `select_recipe` / `discover_curated_paths` surface it
  for the `foundation_system` element class (ADR-042). The geometry is grounded in
  real terrain, so it is a legitimate recipe, not an anti-bluff stand-in.
- **Arbitrary (non-rectangular) footprints** — today the footprint is a rectangle;
  real building outlines need polygon footprints.
- **Heightfield build speed** — IDW per node is O(contour points); a contour-point
  spatial index would make it O(k)/node (queries are already O(1)).
- **Undo-stack integration** — plant/unplant are reversible via the explicit
  command pair; wiring them through the history group for Ctrl+Z is open.
