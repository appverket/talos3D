# Terrain Site Drape Workflow

## Purpose

This document describes the first implemented workflow for turning imported
survey contours such as a Swedish Nybyggnadskarta into a terrain surface in
Talos3D.

The implemented tool is:

- command id: `terrain.prepare_site_surface`
- MCP tool: `prepare_site_surface`

The workflow is intentionally built on top of the public command and MCP
surface rather than private editor state.

## Problem Shape

Nybyggnadskarta material commonly arrives as DWG/DXF linework where:

- contours are imported as fragmented polylines
- fragments may be broken by annotation masks, symbols, or building overlays
- contours encode elevation either in geometry Z or in layer naming
- the desired output is not the raw mesh import but an authored terrain model

Talos3D already imported contour candidates as polylines with
`elevation_metadata`. The missing pieces were:

- contour repair
- drape-oriented terrain sampling
- a single tool that turns selected survey contours into authored terrain
- a dedicated MCP entrypoint for automation clients

## Slices

### Slice 1: Contour Identification

Use existing DXF/DWG import behavior:

- import polylines as authored `polyline`
- preserve `elevation_metadata` when elevation can be inferred
- classify explicit contour layers such as `HOJDKURVA` and
  `HOJDKURVA_HALV` by default, while rejecting obvious non-terrain layers like
  buildings, roofs, hydrography, decks, and text
- treat that authored contour data as the source for repair and terrain build

This avoids UI scraping and stays aligned with the authored-model contract.

### Slice 2: Contour Repair

Implemented in `crates/talos3d-terrain/src/reconstruction.rs`.

Current repair strategy:

- group fragments by quantized elevation and source layer
- normalize duplicate consecutive points away
- compare fragment endpoints within a short bridge tolerance
- require tangent alignment and forward continuation through the same gap
- only join mutual best endpoint pairs so crossings are avoided
- insert explicit bridge segments where masking layers interrupted a contour
- close loops when start/end nearly coincide and tangents are compatible

When a gap is healed, the repaired curve contains an explicit bridge segment.
That keeps the authored result legible and inspectable.

This is conservative by design. It prefers failing to merge over creating
incorrect long-range contour joins.

### Slice 3: Draped Surface Generation

Implemented as an improvement to terrain mesh generation.

The first drape strategy is:

- resample repaired contours at a configurable spacing
- infer a trim boundary from contour coverage without falling back to a simple
  bounding box
- relax the alpha-shape boundary when the initial trim becomes too overfit
- use contour termini as a fallback boundary signal only when they span the
  model extents
- add boundary support samples using inverse-distance weighted elevation
  interpolation
- triangulate the combined sample set
- clip triangles to the terrain boundary

This is not yet constrained triangulation. It is a drape-oriented sampled TIN.
That is good enough for the first Nybyggnadskarta workflow while preserving a
clear upgrade path toward:

- constrained contour/breakline triangulation
- explicit hard breaklines from survey features
- denser point clouds from photogrammetry or LiDAR

### Slice 4: Tool Exposure

Implemented as both:

- a command for local/UI/automation workflows
- a dedicated MCP tool for external agents

The command creates:

- repaired `elevation_curve` entities
- one `terrain_surface` entity referencing those curves

The MCP tool accepts explicit source element IDs so an external agent does not
need to rely on ambient selection state.

## Data Model Notes

`TerrainSurface` now includes `drape_sample_spacing`.

This matters because draping is not only a rendering concern. It affects how
the authored terrain surface is reconstructed from sparse contour constraints.

## Texture Readiness

Terrain meshes now receive planar UVs derived from X/Z extents instead of a
zeroed UV channel.

This is intentionally minimal but important:

- the current draped terrain can accept simple texture projection later
- future texture tools do not need a format migration just to get non-degenerate
  UVs

It is not a complete texture-mapping system. It is only the first compatible
step.

## Limits Of This First Version

- join heuristics are endpoint-based, not topology-aware
- triangulation is sampled Delaunay, not constrained Delaunay
- boundary support heights are interpolated, not inferred from explicit outer
  survey constraints
- no automated building/tree extraction is attempted here
- no direct DWG-specific Nybyggnadskarta classifier is implemented beyond the
  existing elevation-aware import path

## Why This Shape

This design keeps Talos3D aligned with its public architecture:

- imported survey geometry remains authored and inspectable
- repair and drape steps are command-driven
- automation clients can use MCP instead of private hooks
- the terrain workflow remains extensible toward richer survey inputs later
