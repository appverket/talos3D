# Native viewport performance investigation — 2026-10-27

A large-model navigation report after the Bevy 0.20 upgrade exposed false ECS
change notifications in Talos3D chrome. Merely passing mutable resources to the
closed Materials and Renderer windows invalidated material caches/settings on
every frame. Bevy then propagated material changes to Mesh3d and recalculated
mesh bounds. This is a Talos3D invalidation defect, not evidence of a lost GPU
rendering path.

## Repair

- Materials UI preserves resource change ticks while browsing. Its existing
  material generation marks real edits; Apply also invalidates texture resources.
- Renderer UI edits a small settings snapshot and publishes only changed values.
- Occurrence evaluation returns before cloning the Definition registry when no
  occurrences need evaluation. Frozen mesh definitions made that idle clone costly.
- Bloom starts disabled; users can still enable it in Renderer settings.
- Loading releases the source JSON buffer before restoring the parsed project and
  logs read/parse/restore timings separately from derived render preparation.

Neither authoring commands nor model serialization changed. The fixes preserve
Bevy material extraction, bounds calculation, GPU preprocessing and batching.

## Measurement

Apple M4 Max, Metal, optimized native Architecture composition, Bevy 0.20,
perf-stats enabled. A private 1.72 GB JSON fixture contains 5,187 authored entities
and 2,847 rendered meshes. Fixture, screenshots and raw profiles remain local.
The originally reported app was a debug executable; the controlled comparison
below uses optimized builds on both sides, with bloom disabled on both sides.

An identical 120-step middle-button pan produces 123 input trace samples:

| Metric | Before p50 / p95 | After p50 / p95 |
| --- | --- | --- |
| Main frame | 22.74 / 23.16 ms | 5.16 / 5.43 ms |
| Input injection to swapchain submission | 28.06 / 32.27 ms | 8.47 / 12.41 ms |

These are main-frame and input-to-submission timings, not GPU timestamps or
physical display latency; HTTP transport is excluded. Do not infer interactive
performance from the FPS overlay while idle: reactive rendering deliberately
sleeps between inputs. The comparison holds camera, projection, viewport and
renderer settings constant. Profiling was disabled for the reported timing runs.

A separate baseline CPU sample during pan attributed 1,377 of 2,726 main-thread
samples to calculate_bounds and 223 to occurrence evaluation (mostly registry
cloning). Temporary change-counter instrumentation showed 2,847 Mesh3d and Aabb
changes every navigation frame with zero mesh asset changes. After repair, mesh,
bounds and material change counts become zero after initialization. Diagnostic
instrumentation is not shipped.

The runtime reports GPU preprocessing fully supported on Metal. Source comparison
with the prior engine line found the same material-to-mesh change propagation and
bounds machinery; Talos3D GPU feature flags were retained. The registry clone also
predates the upgrade. No old-engine runtime benchmark was performed, so this does
not establish that every 0.20 rendering cost is identical to 0.19.1.

## Load behavior and limits

Three optimized warm-cache loads measured 0.19–0.28 s reading, 1.45–1.46 s parsing,
and 1.02–1.06 s restoring authored state (about 2.7 s total). Derived geometry,
GPU uploads, shader startup and window initialization follow and are not included
in that log. The JSON contains about 1.22 GB of entity data and 498 MB of definition
data. Parsing/restoration remains synchronous; this repair does not claim instant
loading or remove the need for a future staged loader/compact geometry storage.

Regression tests exercise the actual Renderer window across closed and visible
idle frames, preserve external changes, check material-browser read/apply change
ticks, and verify default-off bloom can still be enabled. The plugin suite passes
1,760 tests (four existing ignored).
