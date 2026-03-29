# Drone Survey Pipeline Notes

## Purpose

This note captures follow-on work implied by the Nybyggnadskarta request but
not implemented in the first contour-drape slice.

## Separate Concerns

The current terrain drape workflow and a future drone/LiDAR workflow should
share downstream terrain reconstruction concepts, but they should not be forced
into the same importer path.

Nybyggnadskarta input is primarily:

- vector survey contours
- sparse semantic linework

Drone and LiDAR input is primarily:

- dense points
- multi-view imagery
- classification and inference products

These are adjacent, not identical.

## What Should Converge

Both workflows should eventually feed a common terrain reconstruction model
with support for:

- sample points
- hard breaklines
- soft contour constraints
- classification masks
- authored boundaries
- derived terrain surfaces

That shared representation is the important architectural seam.

## Terrain Implications

The current `drape_sample_spacing` and contour-repair work suggest the future
terrain pipeline should grow toward a richer constraint set, not a single raw
mesh import.

Likely future authored inputs:

- contour curves
- breaklines from curbs, walls, embankments, ditch edges
- dense surveyed points
- inferred building footprints
- inferred tree canopies and trunks
- ground / non-ground classification

## Texture And Mapping Implications

Drone imagery implies eventual support for:

- orthophoto draping over terrain
- local texture atlases
- UV projection strategies that survive terrain edits
- metadata linking terrain patches to imagery provenance

The current planar UV generation is only a compatibility step. Full imagery
support likely needs:

- explicit projection metadata on terrain surfaces
- authored texture regions or layers
- support for multiple texture sources and resolutions

## Likely Future Slices

1. Introduce a public terrain constraint model above raw mesh generation.
2. Add point-cloud and raster import surfaces.
3. Add classification-aware terrain reconstruction.
4. Add orthophoto/texture projection metadata.
5. Add derived authored entities for buildings, trees, and survey artifacts.

## MCP Implications

External agents should be able to inspect and control this through MCP using
structured tools for:

- importing survey datasets
- listing inferred terrain constraints
- promoting inferred constraints into authored entities
- rebuilding terrain surfaces deterministically

That should remain command-driven and inspectable in the same way as the rest
of Talos3D.
