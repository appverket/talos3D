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

## Site orbit and planting invalidation — 2026-11-09

A 5,604-entity linked site regressed to roughly 280 ms per input. CPU sampling
attributed 95% of main-thread samples to planting contract reconstruction:
per-frame group coverage recursively traversed members and searched the entire
ECS for each ID. Rendering remained GPU accelerated.

Planting now reconciles at its input boundary: history, authored geometry and
containment, terrain results, bounds, and missing runtime contracts invalidate
it. Camera and selection changes do not. Reconstruction indexes containment once
and intersects member ancestor sets; unchanged derived components retain their
change ticks. No geometry or authored payload cache is retained. The scheduled
regression test exercises 120 camera updates over 4,096 unrelated entities,
terrain reseating, component removal, and document replacement. Coverage tests
preserve ambiguous and cyclic containment behavior.

Apple M4 Max / 128 GiB, Metal, optimized Architecture with perf-stats, Bevy
0.20.1, 1280 x 720 viewport. Three 72-step Alt+right-button orbits move 300 x 80
pixels from the same camera, with no profiler or observer reads during motion.
The first run follows document load; subsequent runs reset the camera.

| Input to swapchain submission | Before | Planting repair |
| --- | --- | --- |
| Motion p95, each run | 280.94 / 281.95 / 277.49 ms | 16.83 / 16.46 / 16.51 ms |
| Worst observed, all runs | 306.95 ms | 20.73 ms |
| Activation, worst | 272.61 ms | 16.55 ms |
| Release, worst | 281.13 ms | 15.89 ms |

The acceptance budget is motion p95 <=33 ms in every run. Traces preserve model
revision and every authored world bound; camera-only motion regenerates no meshes.
The private fixture retains all linked geometry, materials and definitions, but
uses unavailable source paths in both versions to exclude automatic source-file
refresh. This benchmark measures navigation, not linked-file I/O or load time.
Reported latency ends at swapchain submission, not physical display scanout.

Repeat with `scripts/evaluation/native_orbit_benchmark.py PROJECT --credentials
LOCAL_CREDENTIALS --instance DISPOSABLE_INSTANCE --camera CAMERA_JSON --output
LOCAL_REPORT --build-label COMMIT --replace-disposable-document`. Keep the window
visible. Use `--record-baseline` on the old build and `--baseline LOCAL_REPORT`
on the candidate. Credentials contain `url` and `token`; camera JSON contains
`set_camera` parameters. Keep credentials, fixture and raw reports outside Git.
The runner fails incomplete presentation, unintended edits, camera mismatch,
changed fixture/view dimensions, or a missed latency budget. Idle overlay FPS is
not motion performance: reactive mode deliberately sleeps between inputs.

### Daylight occlusion at site scale

The default native/WebGPU Sky Fill now casts shadows. Its old unshadowed
2,000-lux directional contribution illuminated enclosed rooms through walls and
roofs. Existing authored light choices are preserved; an existing scene can
opt in through the shared `update_light` command. WebGL retains its documented
single-directional approximation and does not claim interior lighting parity.

Directional shadow coverage now follows twice the active orbit radius, with a
150 m floor and 50 m buckets. The previous fixed 150 m range omitted buildings
at the reported ~200 m overview distance. This is derived presentation only,
using Bevy cascaded GPU shadows with unchanged cascade count, map resolution and
near split. The small camera/light query does no model traversal; orbit/pan
preserves configuration change ticks. Tests cover zoom-out, zoom-in, new lights,
and 120 orbit updates without repeated invalidation. This is shadowed viewport
lighting, not a simulation of diffuse indoor light transport.

The Architecture product also spawned an unmanaged sun/fill pair alongside the
persisted lights. Those lights survived document replacement, bypassed the
lighting API, and kept Bevy's fixed 150 m cutoff. Removing that duplicate
product rig is required alongside this Core change. Distribution Architecture
and Studio now use the same document lighting owner. This removes redundant
light/shadow work; no CPU renderer, culling fallback, custom bias, or filter
workaround is enabled.

`native_shadow_acceptance.py` creates a generic two-box render fixture and
compares the same ground sample with sunlight shadows on/off at 50 m and 201 m.
It disables SSAO to isolate directional occlusion. The original product fails
at 201 m (shadow/unshadowed luminance ratio 1.000); the corrected optimized Metal
product passes at both distances (0.133 and 0.134; acceptance <0.75). Native
viewport inspection confirms distant shadows. Near/distant views of the actual
site also show roof, wall and balcony shadows and darkened glazing interiors.
The original Bevy GPU preprocessing/culling remains fully supported and active.

Final combined runtime acceptance uses Core `dc0f7cc` plus the Architecture
startup-owner repair, including object locking from `d11eaee`. With the existing
site sky fill shadows enabled, normal SSAO restored, unchanged 1280×720 window,
and the expanded cascade range active, three 72-step orbits measure motion p95
16.38 / 16.58 / 16.24 ms, worst motion 17.19 ms, activation <=16.55 ms,
release <=16.30 ms. Worst including initial pointer/modifier setup: 31.91 ms.
All three motion budgets pass; authored revision and every world bound remain
unchanged, and camera motion causes zero geometry regeneration. This second
fixture differs only in the authored fill-shadow setting; it is additional
combined-feature acceptance, not the identical-fixture comparison above.

Render regression command (Pillow required; use a disposable app instance):

```sh
python scripts/evaluation/native_shadow_acceptance.py --credentials PRIVATE.json \
  --instance DISPOSABLE_ID --output PRIVATE_OUTPUT --replace-disposable-document
```
