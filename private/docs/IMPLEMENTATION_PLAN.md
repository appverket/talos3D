# Implementation Plan

## Completed: Proof Point 61 — Drawing Views And Paper Export

**Validates**: ADR-030 (renderer control surface), ADR-035 (drawing views and
paper export), and PP30 camera-view foundations.

The renderer now supports drawing-ready paper presentation through public
render settings: white background, grid suppression, white-paper fill, and
visible-edge overlays. Orthographic front/top/right/isometric camera states can
be composed through the named-view path and exported from the cropped modeling
viewport.

See [PROOF_POINT_61.md](./proof_points/PROOF_POINT_61.md).

## Planned: Proof Point 62 — Architectural Dimension Offsets

The current two-point dimension entity needs to evolve into a true
architectural drawing dimension with an explicit dragged offset from the
measured geometry.

See [PROOF_POINT_62.md](./proof_points/PROOF_POINT_62.md).

## Planned: Proof Point 63 — Inferred Guide Lines And Protractor Workflow

Guide lines need a deeper construction workflow: hosted planes, inferred
direction from edges and other guides, snap-aware drag placement, axis locks,
and protractor-driven angular creation.

See [PROOF_POINT_63.md](./proof_points/PROOF_POINT_63.md).

## Completed

### Proof Point 1: Interactive Wall Placement — Complete

Tool input → preview → command → authored domain data → derived mesh. See [PROOF_POINT_1.md](./PROOF_POINT_1.md).

### Proof Point 2: Selection, Deletion, and Undo — Complete

Selection via raycasting, deletion, `History` resource with undo/redo stacks. See [PROOF_POINT_2.md](./PROOF_POINT_2.md).

### Proof Point 3: Transform Operations and Snap System — Complete

Move (G), rotate (R), resize (S) with snap-to-element alignment. All operations use `ApplyEntityChangesCommand` with `EntitySnapshot` before/after data. See [PROOF_POINT_3.md](./PROOF_POINT_3.md).

### Proof Point 4: Basic Geometry Types and the Modeling Setup — Complete

Box, cylinder, plane, polyline alongside walls. Codebase split into `modeling/` and `architectural/` plugin groups, validating ADR-006's layered capability model. See [PROOF_POINT_4.md](./PROOF_POINT_4.md).

### Post-PP4 Cleanup — Complete

Unified `EntitySnapshot` as the single entity state type. Removed dead wall-specific commands (`MoveEntityCommand`, `RotateEntityCommand`, `ResizeWallCommand`). All modifications now flow through `ApplyEntityChangesCommand`.

### Proof Point 5: Wall Openings — Complete

Wall openings with entity dependencies, cascade delete, mesh generation with holes. See [PROOF_POINT_5.md](./PROOF_POINT_5.md).

### Proof Point 6: Property Editing and Persistence — Complete

Tab-based property editing, Cmd+S/Cmd+O save/load to JSON. See [PROOF_POINT_6.md](./PROOF_POINT_6.md).

### Post-PP6 Cleanup — Complete

DRY fixes: extracted `Wall::length()` and `Wall::direction()` methods to `architectural/components.rs`. Moved shared `snap_to_increment`, `rectangle_corners`, `draw_loop` to `plugins/math.rs`. Eliminated all duplicate utility functions across selection.rs, opening_tool.rs.

## Completed: Proof Point 7 — Model API and AI Comprehension

**Validates**: ADR-008 decisions 1-4 (AI as first-class participant, model comprehension, Model API, EntitySnapshot as AI data format).

MCP server exposes the authored model to external AI agents through stdio transport. Three tools: `list_entities`, `get_entity`, `model_summary`. Feature-gated behind `model-api` cargo feature.

See [PROOF_POINT_7.md](./PROOF_POINT_7.md).

## Completed: Proof Point 8 — Universal Transform System

**Validates**: ADR-010 decisions 1-4, 6 (universal transforms, verb→axis→value, numeric input, command pipeline).

All editing operations follow a single verb→axis→value keyboard pattern. Axis constraints and numeric precision are available during any transform on any entity type. Replaces per-type operation code with a unified transform pipeline.

See [PROOF_POINT_8.md](./PROOF_POINT_8.md) for full acceptance criteria.

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add `scale_by()` to EntitySnapshot

**Files**: `src/plugins/commands.rs`

- Add `scale_by(&self, factor: Vec3, center: Vec3) -> Self` to EntitySnapshot.
- For each variant: scale positions relative to center, scale dimensions by factor components.
- Unit tests for scale_by on Wall and Box variants.

**Verify**: `cargo check` passes.

#### Step 2: Extract TransformState and state machine

**Files**: `src/plugins/transform.rs` (new), `src/plugins/selection.rs` (modify), `src/plugins/mod.rs`

- Create `TransformState` resource with mode (Idle/Moving/Rotating/Scaling), axis constraint, numeric buffer, initial snapshots.
- Move transform-related systems from selection.rs into transform.rs.
- Unify per-operation update/commit/cancel systems into a single state machine.
- Keep selection clicking, outline drawing, and deletion in selection.rs.

**Verify**: `cargo check` passes. Existing G/R/S shortcuts behave as before.

#### Step 3: Add axis constraint and numeric input

**Files**: `src/plugins/transform.rs`

- During active transform: X/Y/Z keys toggle axis constraint, Shift+X/Y/Z set plane constraint.
- Digit keys, `.`, `-` start/append numeric input. Backspace removes last char.
- Constraint axis drawn as colored gizmo line. Status bar shows operation/axis/value.
- Numeric input overrides cursor-following with exact typed value.

**Verify**: `cargo check` passes. G X 2.5 Enter moves 2.5m along X. R 45 Enter rotates 45°. S 2 Enter scales by 2×.

#### Step 4: Cleanup and verification

- Remove dead code from selection.rs.
- `cargo clippy` clean. All existing tests pass.
- Undo/redo works for move, rotate, and scale.
- Snap system still functions during moves.

**Verify**: all acceptance criteria from [PROOF_POINT_8.md](./PROOF_POINT_8.md) are met.

## Completed: Proof Point 9 — AI Write Operations

**Validates**: ADR-008 decisions 3, 4 (Model API write path, EntitySnapshot as AI data format) and ADR-010 decision 7 (editing model maps to AI operations).

Five new MCP tools: `create_entity`, `delete_entities`, `transform`, `set_property`, `list_handles`. All write operations flow through the command pipeline and are fully undoable. The `transform` tool maps directly onto PP8's universal transform system.

See [PROOF_POINT_9.md](./PROOF_POINT_9.md) for full acceptance criteria.

### Step Sequence

Each step must end with `cargo check --features model-api` passing.

#### Step 1: Extend channel bridge with write request variants

**Files**: `src/plugins/model_api.rs`

- Add `CreateEntity`, `DeleteEntities`, `Transform`, `SetProperty`, `ListHandles` variants to the MCP request enum.
- Add match arms in the Bevy system handler (placeholder errors initially).

#### Step 2: Implement `create_entity` and `delete_entities`

**Files**: `src/plugins/model_api.rs`

- Parse JSON `type` field, extract parameters, send appropriate `Create*Command` event.
- `delete_entities` sends `DeleteEntitiesCommand`.
- Error handling for unknown types and invalid fields.

#### Step 3: Implement `transform` and `set_property`

**Files**: `src/plugins/model_api.rs`

- `transform`: capture snapshots, compute delta (move/rotate/scale with optional axis), send `ApplyEntityChangesCommand`.
- `set_property`: capture snapshot, modify named field, send `ApplyEntityChangesCommand`.
- Error handling for invalid property names (return valid options).

#### Step 4: Implement `list_handles` and register MCP tools

**Files**: `src/plugins/model_api.rs`

- `list_handles`: compute handles per ADR-010 section 4, return as JSON.
- Register all 5 write tools with `#[tool]` attributes and request schemas.
- `cargo clippy --features model-api` clean.

## Completed: Proof Point 10 — Crate Extraction and Extension Architecture

**Validates**: ADR-009 (extension architecture, AuthoredEntity trait, CapabilityRegistry, workspace structure).

The architectural setup moves into its own crate. `EntitySnapshot` closed enum is replaced by trait-based dispatch. Core platform modules lose all imports from `talos3d-architectural`.

See [PROOF_POINT_10.md](./PROOF_POINT_10.md).

## Completed: Proof Point 11 — Command Palette and Menu Bar

**Validates**: ADR-011 (unified command surface), ADR-009 (extension registration in a user-facing context).

A `CommandRegistry` holds descriptors for every user-facing operation. The command palette provides searchable textual access; the menu bar provides persistent categorized access. Both are generated from the registry. Community capabilities contribute commands with icons, hints, and shortcuts — appearing in all surfaces with a single registration.

See [PROOF_POINT_11.md](./PROOF_POINT_11.md).

## Completed: Proof Point 12 — File Import Framework

**Validates**: ADR-009 §10 (import/export format extensibility).

An `ImportRegistry` allows capabilities to register file format importers. The core modeling setup gains a `TriangleMesh` authored entity type. An OBJ importer validates the end-to-end pipeline: file picker → parser → entity creation through the command pipeline. The import command appears in the menu bar and command palette (PP11).

See [PROOF_POINT_12.md](./PROOF_POINT_12.md).

## Recommended Execution Order

The next implementation pass should **not** follow proof point number order literally.

Recommended execution order:

1. **PP31** — File Management (Save As, file dialogs, dirty tracking). Foundation for real project workflows.
2. **PP32** — Grouping and Hierarchical Editing. Enables imported DWG selection-as-unit and enter/exit editing.
3. **PP33** — Layer Authoring. Evolves read-only imported layers into a full authoring layer system.
4. **PP34** — Selection Enhancements (box select, selection filters). Professional selection workflow for large models.
5. **PP14** — Terrain Setup (remaining: constrained triangulation, breakline enforcement).
6. **PP30** — Camera Projection and Named Views.
7. **PP15** — Terrain Refinement and Excavation.

Rationale:

- PP31 is low-risk, high-value: users cannot do real work without Save As and file dialogs.
- PP32 (grouping) is prerequisite for PP33 (layers) because imported DWG content needs both group-as-unit selection and layer-based organization.
- PP33 (layers) replaces the current read-only `ImportedLayer` with a full authoring system, unifying imported and user-created content.
- PP34 (selection enhancements) depends on PP32 (group context) and PP33 (layer visibility/locking) for correct filtering behavior.
- PP14 has partial terrain work already done; the remaining constrained triangulation step can proceed independently of the editor UX improvements.
- PP30 (camera views) is independent and can be slotted in whenever convenient.

## Completed: Proof Point 13 — DWG/DXF Import

**Validates**: import framework with a real-world format; elevation data preservation for terrain workflows.

A DXF importer parses 2D/3D geometry, preserves layer structure, and recognizes elevation curves from survey data. Polylines with Z coordinates are tagged with `ElevationMetadata`. Layer-based filtering and coordinate system handling (Z-up → Y-up, unit scaling) are supported.

See [PROOF_POINT_13.md](./PROOF_POINT_13.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add DXF importer registration and conversion settings

**Files**: `crates/talos3d-core/src/importers/dxf.rs` (new), `crates/talos3d-core/src/importers/mod.rs`, `crates/talos3d-core/src/plugins/import.rs`, `crates/talos3d-core/src/plugins/modeling/mod.rs`

- Add `DxfImporter` to the import registry.
- Define import settings for axis mapping, unit scaling, origin offset, and layer filtering.
- Return a clear best-effort error for `.dwg` files when no conversion path is available.
- Status: native DXF parsing is implemented through the `dxf` crate, registered in the modeling setup, and `.dwg` is selectable again. Runtime DWG import is native-first and does not silently shell out to external conversion by default. Temporary converter fallback remains available only when `TALOS3D_ALLOW_DWG_CONVERTER=1` is explicitly set; that development-only fallback probes a custom `TALOS3D_DWG_CONVERTER` wrapper first, then bundled converter binaries near the app/project, ODA File Converter, and LibreDWG CLI tools. The bundled ODA path has been validated locally against a real `AC1032` file, and the converter lookup handles macOS `.app` bundles plus Unicode output filename normalization.

#### Step 2: Map DXF entities into authored entities

**Files**: `crates/talos3d-core/src/importers/dxf.rs`, `crates/talos3d-core/src/plugins/modeling/snapshots.rs`

- Import `LINE`, `POLYLINE`, `LWPOLYLINE`, `ARC`, `CIRCLE`, `3DFACE`, and `INSERT`.
- Map line/polyline/arc/circle geometry to `Polyline` and 3D faces to `TriangleMesh`.
- Expand DXF blocks recursively with a bounded depth.
- Status: implemented for the core geometry path with recursive INSERT expansion, paper-space filtering, bulged-polyline tessellation, and test coverage against both synthetic fixtures and a real converted survey DWG. Metadata preservation and import settings remain.

#### Step 3: Preserve layers and elevation metadata

**Files**: `crates/talos3d-core/src/importers/dxf.rs`, `crates/talos3d-core/src/plugins/import.rs`, `crates/talos3d-core/src/plugins/model_api.rs`

- Attach imported layer metadata and parsed elevation metadata to entities.
- Expose imported layer information through the Model API.
- Add import filtering and post-import layer visibility toggles.
- Status: imported polylines and triangle meshes now retain layer metadata in authored snapshots, imported contour polylines can carry elevation metadata derived from consistent Z values or layer-name parsing, the viewport renders elevation-tagged polylines in repeating elevation bands, and the egui import workflow now supports per-layer filtering, entity counts, configurable unit/origin transforms, and post-import layer visibility toggles backed by Bevy `Visibility`.

#### Step 4: Async/progress plumbing and verification

- Ensure long imports show progress and avoid blocking the UI.
- Verify OBJ import remains unchanged.
- `cargo clippy` clean.
- Status: import parsing now runs in the background with a delayed progress window after 500ms, successful parses transition into an explicit review/commit step instead of committing blindly, the DXF importer now has explicit tests for supported-entity mapping plus elevation inference rules, and the import path remains compile-clean with the existing OBJ importer intact.

#### Step 5: Native DWG reader scaffold

**Files**: `Cargo.toml`, `crates/cadio-ir/Cargo.toml` (new), `crates/cadio-ir/src/lib.rs` (new), `crates/cadio-dwg/Cargo.toml` (new), `crates/cadio-dwg/src/lib.rs` (new), `docs/DWG_READER_PLAN.md` (new)

- Add extraction-ready workspace crates for a neutral CAD IR and native DWG reader.
- Implement a real DWG header/version probe as the first executable DWG slice.
- Keep the crates free of Bevy and Talos-specific entity types so they can move into a separate open-source repository later.
- Status: complete for PP13 scope. `cadio-ir` now defines the neutral document/entity model, and `cadio-dwg` can probe common DWG version sentinels, extract stable UTF-16 metadata fragments from real DWGs, emit a serializable structural summary with candidate layer hints and keyword counts, decrypt the AC18-style front header used by the local `AC1032` survey sample, decompress native `SectionMap` / `SectionPageMap` pages, parse them into structured section descriptors plus page records, reconstruct real logical section byte buffers such as `AcDb:Header`, `Handles`, `Classes`, and `Objects`, decode a native `Handles` map from the reconstructed stream, decode the real sample's eight ObjectDBX class definitions from the merged `Classes` stream, and build a sorted native object index from handle offsets into the concatenated `Objects` section. The semantic lift now canonicalizes related block handles, resolves real block-backed inserts, preserves top-level authored entities from the sample, and is covered by sample-backed tests in both `cadio-dwg` and the Talos native importer path.

## In Progress: Proof Point 14 — Terrain Setup

**Validates**: ADR-012 (surface geometry and terrain modeling), ADR-009 (new domain setup as independent crate).

A new `talos3d-terrain` crate introduces `ElevationCurve` and `TerrainSurface` entity types. TIN generation from elevation curves via constrained Delaunay triangulation. A `TerrainProvider` trait enables cross-setup queries. The terrain setup is the second independent domain setup, validating the extension architecture with a genuinely different domain.

Status: the workspace now includes `talos3d-terrain`, the app enables it by default behind an optional feature, `TerrainPlugin` enforces `RequireSetup<ModelingSetup>`, authored `ElevationCurve` / `TerrainSurface` factories are registered, imported contour polylines can be converted into elevation curves through undoable commands, the terrain generation command opens a preview/review window before committing, contour gizmos and mesh regeneration run from authored `TerrainSurface` data, and a real `TerrainProvider` now serves elevation/surface/volume queries over generated terrain caches. The remaining PP14 gap is the hard geometry step: constrained breakline-aware triangulation and stronger terrain quality controls.

See [PROOF_POINT_14.md](./PROOF_POINT_14.md).

### Step Sequence

Each step must end with `cargo check` passing for the workspace.

#### Step 1: Add the terrain crate and register terrain entities

**Files**: `Cargo.toml`, `src/main.rs`, `crates/talos3d-terrain/Cargo.toml` (new), `crates/talos3d-terrain/src/lib.rs` (new), `crates/talos3d-terrain/src/components.rs` (new)

- Add `talos3d-terrain` to the workspace and wire it into the binary.
- Define `ElevationCurve` and `TerrainSurface`.
- Register terrain factories with the capability registry.

#### Step 2: Convert imported contour data into terrain entities

**Files**: `crates/talos3d-terrain/src/components.rs`, `crates/talos3d-terrain/src/lib.rs`, `crates/talos3d-core/src/plugins/command_registry.rs`

- Add a "Convert to Elevation Curves" command.
- Preserve elevation/layer metadata from imported polylines.
- Make the conversion undoable.
- Status: implemented. Selected imported polylines carrying `ElevationMetadata` can now be copied or converted into authored `ElevationCurve` entities, preserving source layer/elevation data and using a command-group path so undo/redo remains intact.

#### Step 3: Add TIN generation and terrain mesh regeneration

**Files**: `crates/talos3d-terrain/src/tin_generation.rs` (new), `crates/talos3d-terrain/src/mesh_generation.rs` (new), `crates/talos3d-terrain/src/contour_generation.rs` (new)

- Generate a TIN from elevation curves with constrained triangulation.
- Render the terrain as a derived mesh and derived contours as gizmos.
- Regenerate when source curves change.
- Status: partially implemented. Terrain surfaces already regenerate a derived mesh plus contour gizmos from authored source curves and re-mark on source-curve changes, but the current triangulation is still unconstrained and does not yet enforce contour breaklines.

#### Step 4: Add terrain command/tool surface and provider API

**Files**: `crates/talos3d-terrain/src/tools/terrain_tool.rs` (new), `crates/talos3d-terrain/src/terrain_provider.rs` (new)

- Add a terrain generation command/tool with preview.
- Register a `TerrainProvider`.
- `cargo clippy` clean across the workspace.
- Status: mostly implemented. The terrain setup now provides a `Generate Terrain` command with an egui review/preview window, registers a real `TerrainProvider`, and the workspace is clippy-clean. The remaining missing piece is richer boundary editing/preview controls tied to the final constrained triangulation path.

## Future: Proof Point 15 — Terrain Refinement and Excavation

**Validates**: ADR-012 (cross-setup terrain integration), terrain as a working design surface.

Terrain editing, slope/aspect visualization, cut/fill analysis between existing and proposed surfaces, and excavation integration with the architectural setup. The architectural setup gains a `BuildingPad` entity that queries the `TerrainProvider` for excavation volumes. `surface_to_solid()` extrusion is added to the core modeling setup as a general-purpose geometric operation.

See [PROOF_POINT_15.md](./PROOF_POINT_15.md).

### Step Sequence

Each step must end with `cargo check` passing for the workspace.

#### Step 1: Add terrain editing and proposed-surface flow

**Files**: `crates/talos3d-terrain/src/editing.rs` (new), `crates/talos3d-terrain/src/tools/spot_elevation.rs` (new)

- Support adding, editing, and deleting elevation inputs.
- Add a "Create Proposed Surface" command path.
- Keep all changes command-based and undoable.

#### Step 2: Add terrain analysis modes

**Files**: `crates/talos3d-terrain/src/visualization.rs` (new), `crates/talos3d-terrain/src/cut_fill.rs` (new)

- Add slope, aspect, and elevation-band visualizations.
- Add cut/fill calculations between two surfaces or against a datum.

#### Step 3: Add architectural excavation integration

**Files**: `crates/talos3d-architectural/src/components.rs`, `crates/talos3d-architectural/src/tools/building_pad_tool.rs` (new), `crates/talos3d-architectural/src/excavation.rs` (new)

- Introduce `BuildingPad`.
- Query the `TerrainProvider` without depending on terrain concrete types.
- Surface excavation volume in the authored model and UI.

#### Step 4: Add shared surface-to-solid utilities and MCP endpoints

**Files**: `crates/talos3d-core/src/plugins/modeling/solid_ops.rs` (new), `crates/talos3d-core/src/plugins/model_api.rs`

- Add `surface_to_solid()` as a reusable modeling utility.
- Expose terrain analysis through MCP.
- `cargo clippy` clean across the workspace.

## Completed: Proof Point 16 — Visual Gizmo Handles

Selected entities expose direct-manipulation handles for move, scale, rotate, and entity-specific edits. Handles are computed from `HandleInfo`, rendered as camera-scaled gizmos, and integrate with the universal transform and property-edit pipelines.

See [PROOF_POINT_16.md](./PROOF_POINT_16.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add handle resources and rendering

**Files**: `crates/talos3d-core/src/authored_entity.rs`, `crates/talos3d-core/src/plugins/handles.rs` (new), `crates/talos3d-core/src/plugins/mod.rs`, `src/main.rs`

- Add structured handle kinds/display modes.
- Render handles for the selected entity using gizmos.
- Reset handle context when selection changes.

#### Completion Summary

- Screen-space hover hit testing now takes priority over mesh selection.
- Move, scale, and rotate handle drags enter the existing transform pipeline with axis/pivot presets.
- Entity-specific authored handles route through property drag previews and commit as `ApplyEntityChangesCommand`.
- Camera-scaled sphere/cube gizmos keep handles visually consistent across zoom levels.

## Completed: Proof Point 17 — Property Panel UI

A dedicated property panel replaces status-bar-only editing with direct field-based editing for the selected entity type. Property changes continue to flow through `ApplyEntityChangesCommand` and remain fully undoable.

See [PROOF_POINT_17.md](./PROOF_POINT_17.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add panel shell and viewport inset handling

**Files**: `crates/talos3d-core/src/plugins/property_panel.rs` (new), `crates/talos3d-core/src/plugins/cursor.rs`, `crates/talos3d-core/src/plugins/mod.rs`, `src/main.rs`

- Add the right-side panel root and viewport inset updates.
- Show title and mixed-selection state.
- Status: implemented in egui chrome with selection-aware visibility and automatic viewport inset handling via `available_rect()`.

#### Step 2: Render property rows from `property_fields()`

**Files**: `crates/talos3d-core/src/plugins/property_panel.rs`

- Render labels and values for a homogeneous selection.
- Add unit-aware formatting and mixed-value display.
- Status: implemented for scalar, vec2, vec3, and text property values.

#### Step 3: Add field editing and command integration

**Files**: `crates/talos3d-core/src/plugins/property_panel.rs`, `crates/talos3d-core/src/plugins/property_edit.rs`

- Support click-to-edit, keyboard navigation, apply/cancel, and multi-selection apply.
- Disable the legacy status-bar property mode while the panel is active.
- Status: implemented in the egui panel with click-to-edit, Enter apply-and-advance, Tab/Shift+Tab navigation, Escape cancel, and undoable `ApplyEntityChangesCommand` writes.

#### Step 4: MCP/UI synchronization and verification

- Keep the panel live during transforms and external property changes.
- `cargo clippy` clean.
- Status: complete. Transform-preview-backed panel data is live during edits, the model API exposes matching `get_entity_details` and `set_entity_property` tools, and property definitions now include editable vs read-only metadata so computed fields like wall length can be shown safely. Snapshot property surfaces were also normalized to match the UI contract: walls expose Vec3 endpoints plus computed length, box/cylinder use `center`, and planes expose Vec3 corners.

## Completed: Proof Point 18 — Transform Enhancements — Pivot Selection and Menu Dispatch

Rotation and scaling can use an explicit pivot point, handle clicks establish that pivot directly, and command-surface support includes palette-driven pivot placement/clearing. This closes the remaining pivot/menu interaction gaps in the editing workflow.

See [PROOF_POINT_18.md](./PROOF_POINT_18.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add pivot resource and transform integration

**Files**: `crates/talos3d-core/src/plugins/transform.rs`, `crates/talos3d-core/src/plugins/selection.rs`

- Add `PivotPoint`.
- Use it for rotate/scale center calculations.
- Reset it when the selection changes.

#### Step 2: Add handle-driven pivot selection

**Files**: `crates/talos3d-core/src/plugins/handles.rs`, `crates/talos3d-core/src/plugins/transform.rs`

- Clicking a handle sets the pivot.
- Draw the pivot gizmo and preserve it across transforms.
- Status: implemented for handle click and rendered as a persistent gizmo until selection changes.

#### Step 3: Fix menu dispatch at the root cause

**Files**: `crates/talos3d-core/src/plugins/menu_bar.rs`

- Stop rebuilding the dropdown every frame.
- Preserve button entity identity so `Interaction::Pressed` is observable.
- Status: implemented by caching the dropdown signature and skipping rebuilds when unchanged.

#### Completion Summary

- `PivotPoint` persists across transforms and is cleared on selection reset or empty-space click.
- Handle clicks set the pivot directly and show a distinct pivot indicator.
- The palette now supports `Set Pivot x y z` and `Clear Pivot` commands through the shared command registry.
- Menu dispatch reliability is provided by the egui command path introduced in PP29.

## Completed: Proof Point 19 — Performance Architecture — Transform Feedback and Rendering Pipeline

Transform preview avoids mutating authored state during preview, skips `NeedsMesh` for move/rotate commits, keeps selection outlines aligned with preview transforms, and includes a feature-gated egui perf overlay plus remesh-discipline regression tests.

See [PROOF_POINT_19.md](./PROOF_POINT_19.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Fix transform outline feedback and scheduling

**Files**: `crates/talos3d-core/src/plugins/selection.rs`, `crates/talos3d-core/src/plugins/transform.rs`

- Draw selection outlines during active transforms when the entity is using live `Transform` preview.
- Draw preview gizmos for entities that do not use live `Transform` preview during move/rotate.
- Add explicit system ordering so preview updates happen before outline/preview drawing.

#### Step 2: Audit preview entity lifecycle and remesh discipline

**Files**: `crates/talos3d-core/src/plugins/transform.rs`, `crates/talos3d-core/src/plugins/modeling/snapshots.rs`, `crates/talos3d-architectural/src/snapshots.rs`

- Verify rapid confirm/cancel cycles do not leak preview entities.
- Verify `apply_with_previous()` only remeshes on true shape changes.

#### Step 3: Add `perf-stats` feature-gated profiling

**Files**: `crates/talos3d-core/Cargo.toml`, `Cargo.toml`, `crates/talos3d-core/src/plugins/perf_stats.rs` (new), `crates/talos3d-core/src/plugins/mod.rs`, `src/main.rs`

- Add `PerfStats` resource and top-right overlay.
- Toggle visibility with `F11`.
- Compile everything out when the feature is disabled.

#### Step 4: Verification and acceptance sweep

- Validate move/rotate/scale preview with boxes, walls, openings, planes, cylinders, polylines, and triangle meshes.
- `cargo clippy` clean with and without `--features perf-stats`.

## Completed: Proof Point 20 — Extensible Toolbar System

An extensible toolbar surface provides dockable, one-click access to commands from the shared command and icon registries, including visibility controls and model-API layout access.

See [PROOF_POINT_20.md](./PROOF_POINT_20.md).

- Status: rendered docked toolbars, active-tool mapping, viewport inset integration, layout persistence, grip-driven drag-to-dock, right-click visibility toggles, and model-API `list_toolbars` / `set_toolbar_layout` support are all implemented.
- Note: the bevy_ui rendering portion of PP20 will be superseded by PP29 (egui migration). The data model (ToolbarRegistry, ToolbarDescriptor, layout persistence) is preserved.

## Completed: Proof Point 29 — UI Chrome Migration to egui

All application UI chrome — menu bar, toolbars, status bar — migrates from hand-built `bevy_ui` node trees to egui via `bevy_egui`. The 3D viewport and all ECS logic remain in native Bevy. This fixes viewport contention, input boundary conflicts, and reduces ~1835 lines of manual UI to ~250 lines of egui widget calls. Establishes the foundation for the property panel (PP17).

See [PROOF_POINT_29.md](./PROOF_POINT_29.md).

## Future: Proof Point 31 — File Management — Save As, File Dialogs, and Document State

**Validates**: professional file management workflow; replaces PP6's fixed-path save/load.

Native file dialogs for Open, Save, Save As via the `rfd` crate. Document dirty tracking with save-point awareness in the History. Window title shows current file name and modified indicator. Unsaved changes warning on open/new/close. `Cmd+N` for new document.

See [PROOF_POINT_31.md](./PROOF_POINT_31.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add DocumentState resource and dirty tracking

**Files**: `crates/talos3d-core/src/plugins/document_state.rs` (new), `crates/talos3d-core/src/plugins/history.rs`, `crates/talos3d-core/src/plugins/mod.rs`

- Add `DocumentState` resource with current file path, dirty flag, save-point index.
- Hook into `History` to set/clear dirty flag on command execute/undo/redo.
- Update window title from `DocumentState` each frame.

#### Step 2: Add file dialogs and Save As

**Files**: `crates/talos3d-core/src/plugins/persistence.rs`, `Cargo.toml`

- Add `rfd` dependency.
- Replace hardcoded path with `DocumentState.current_path`.
- Implement Save As (`Cmd+Shift+S`) with native dialog.
- Implement Save (`Cmd+S`) — save to current path or fall through to Save As.
- Implement Open (`Cmd+O`) with native dialog.

#### Step 3: Add unsaved changes warning and New Document

**Files**: `crates/talos3d-core/src/plugins/persistence.rs`, `crates/talos3d-core/src/plugins/egui_chrome.rs`

- Show confirmation dialog before discarding unsaved changes on Open/New/Close.
- Implement `Cmd+N` for new document.
- Register File > New, File > Save As in command registry.

#### Step 4: Verification

- Verify dirty tracking through command execute, undo, redo, save cycles.
- Verify window title updates correctly.
- `cargo clippy` clean.

## Future: Proof Point 32 — Grouping and Hierarchical Editing

**Validates**: hierarchical entity composition, imported DWG as selectable unit, enter/exit group editing.

A `Group` authored entity type wraps multiple entities. Clicking a group member selects the group. Double-click enters the group for individual member editing. Imported DWG/DXF files produce a group automatically. Groups can be nested and named.

See [PROOF_POINT_32.md](./PROOF_POINT_32.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add Group entity type and snapshot

**Files**: `crates/talos3d-core/src/plugins/modeling/group.rs` (new), `crates/talos3d-core/src/plugins/modeling/snapshots.rs`, `crates/talos3d-core/src/plugins/modeling/mod.rs`

- Implement `Group` as `AuthoredEntity` with name and member `ElementId` list.
- Register factory with `CapabilityRegistry`.
- Add `Cmd+G` (group) and `Cmd+Shift+G` (ungroup) commands.

#### Step 2: Add group-aware selection

**Files**: `crates/talos3d-core/src/plugins/selection.rs`

- Add `GroupEditContext` resource (stack of `ElementId` for current editing path).
- Click on group member → select group (at root level).
- Double-click selected group → enter group editing mode.
- Escape → exit group editing mode.
- Filter selection queries by current editing context.

#### Step 3: Group transforms and visual feedback

**Files**: `crates/talos3d-core/src/plugins/transform.rs`, `crates/talos3d-core/src/plugins/egui_chrome.rs`

- Group transform propagates to all members.
- Dim non-group entities when in group editing mode.
- Show breadcrumb trail in status bar for editing context.

#### Step 4: Import-as-group and verification

**Files**: `crates/talos3d-core/src/plugins/import.rs`

- Wrap imported entities in a `Group` named after the source file.
- Verify group persistence, undo/redo, and Model API access.
- `cargo clippy` clean.

## Future: Proof Point 33 — Layer Authoring

**Validates**: full authoring-time layer system; evolves PP13's read-only imported layers.

Users can create, name, rename, and delete layers. Entities are assigned to layers. Layer visibility and locking control rendering and editability. Imported DXF layers become authored layers. A layer panel provides the management UI.

See [PROOF_POINT_33.md](./PROOF_POINT_33.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add Layer registry and layer data model

**Files**: `crates/talos3d-core/src/plugins/layers.rs` (new), `crates/talos3d-core/src/plugins/modeling/primitives.rs`, `crates/talos3d-core/src/plugins/mod.rs`

- Add `LayerRegistry` resource with named layers (name, color, visible, locked).
- Add default "Default" layer.
- Replace `ImportedLayer` component with `LayerAssignment` component.
- Persist layer definitions in the project file.

#### Step 2: Add layer panel UI

**Files**: `crates/talos3d-core/src/plugins/egui_chrome.rs`, `crates/talos3d-core/src/plugins/layers.rs`

- Add egui layer panel with visibility/lock toggles, rename, add, delete.
- Active layer indicator and selection.
- New entities are created on the active layer.

#### Step 3: Layer visibility, locking, and selection integration

**Files**: `crates/talos3d-core/src/plugins/selection.rs`, `crates/talos3d-core/src/plugins/layers.rs`

- Hidden layers: entities not rendered, not selectable.
- Locked layers: entities rendered but not selectable/editable.
- Layer operations flow through command pipeline (undoable).

#### Step 4: Import migration and verification

**Files**: `crates/talos3d-core/src/plugins/import.rs`

- Import creates authored layers for each DXF layer.
- Remove `ImportedLayerPanelState` in favor of the authored layer panel.
- Model API integration: `list_layers`, layer assignment in entity details.
- `cargo clippy` clean.

## Future: Proof Point 34 — Selection Enhancements — Box Select and Selection Filters

**Validates**: professional selection workflow for large models.

Click-and-drag box selection with window (left-to-right, fully contained) and crossing (right-to-left, intersecting) modes. Selection filters by entity type and layer. Select All respects group context and layer state. Invert selection.

See [PROOF_POINT_34.md](./PROOF_POINT_34.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add box select with window/crossing modes

**Files**: `crates/talos3d-core/src/plugins/selection.rs`, `crates/talos3d-core/src/plugins/cursor.rs`

- Click-and-drag in empty space starts box selection.
- Draw selection rectangle with directional styling.
- Window select (L→R) requires full containment; crossing select (R→L) requires intersection.
- Shift+drag adds to selection.

#### Step 2: Integrate with group context and layers

**Files**: `crates/talos3d-core/src/plugins/selection.rs`

- Box select respects `GroupEditContext` (PP32).
- Box select skips hidden/locked layer entities (PP33).
- `Cmd+A` respects group context and layer state.

#### Step 3: Add selection filters and invert

**Files**: `crates/talos3d-core/src/plugins/selection.rs`, `crates/talos3d-core/src/plugins/egui_chrome.rs`

- Selection filter dropdown in status bar.
- `Cmd+I` for invert selection.
- `cargo clippy` clean.

## Future: Proof Point 30 — Camera Projection and Named Views

Perspective/Orthographic projection toggle, configurable FOV, named standard-view presets (Top, Front, Right, Left, Back, Bottom, Isometric) with keyboard shortcuts, smooth animated transitions, view bookmarks, and a status-bar projection indicator. All view commands integrate with the unified command surface. No setup-level changes — operates entirely within `talos3d-core`.

See [PROOF_POINT_30.md](./PROOF_POINT_30.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add ProjectionMode and FOV to OrbitCamera

**Files**: `crates/talos3d-core/src/plugins/camera.rs`

- Add `projection: ProjectionMode` and `fov: f32` fields to `OrbitCamera`.
- Add `sync_projection()` helper that writes the correct Bevy `Projection` variant.
- Call `sync_projection` at the end of `orbit_camera` each frame.
- Orthographic scroll path: adjust scale, keep radius in sync.
- Register `ToggleProjection` command under View category (`P` key).

#### Step 2: Add named standard-view commands

**Files**: `crates/talos3d-core/src/plugins/camera.rs`, `crates/talos3d-core/src/plugins/command_registry.rs`

- Add `CameraAnimation` resource with from/to `CameraState`, elapsed, duration.
- Implement `angular_lerp` (shortest arc) and `ease_out_cubic`.
- Register Front, Back, Right, Left, Top, Bottom, Isometric, Reset View commands with numpad shortcuts.
- Each command writes to `CameraAnimation`; `tick_camera_animation` system advances it each frame.
- Orthographic planar views switch projection; Isometric/Reset restore perspective.

#### Step 3: Add frame operations, LastView, and view bookmarks

**Files**: `crates/talos3d-core/src/plugins/camera.rs`, `crates/talos3d-core/src/plugins/command_registry.rs`

- Animate `FrameAll` and `FrameSelected` through `CameraAnimation` instead of snapping.
- Add `LastView` (Backquote): store previous `CameraState` on every named-view command; swap on keypress.
- Add `SaveView`/`RestoreView` (`Ctrl+Shift+1–5` / `Ctrl+1–5`); persist in `DocumentProperties`.

#### Step 4: Status bar indicator and verification

**Files**: `crates/talos3d-core/src/plugins/egui_chrome.rs`

- Add clickable projection chip to the egui status bar.
- `cargo clippy` clean. Verify all existing navigation gestures unchanged.
- Verify numpad shortcuts work in both Perspective and Orthographic modes.

## Future: Proof Point 21 — Material System Foundation

A persistent authored material model will make visual properties first-class across rendering, persistence, undo/redo, and AI access.

See [PROOF_POINT_21.md](./PROOF_POINT_21.md).

## Future: Proof Point 22 — Texture Mapping and Rendering

The material system will gain texture assets, world-space mapping controls, and triplanar-capable rendering.

See [PROOF_POINT_22.md](./PROOF_POINT_22.md).

## Future: Proof Point 23 — Material Library and Domain Materials

Material libraries and a browser workflow will provide domain-authored visual and physical materials for assignment and reuse.

See [PROOF_POINT_23.md](./PROOF_POINT_23.md).

## Future: Proof Point 24 — Export Framework and glTF Conduit

An export pipeline will complement the existing import framework, with glTF as the first full interchange conduit.

See [PROOF_POINT_24.md](./PROOF_POINT_24.md).

## Future: Proof Point 25 — IFC Conduit — BIM Interchange

Semantic IFC import/export will connect Talos3D's authored architectural model to BIM workflows without flattening everything to meshes.

See [PROOF_POINT_25.md](./PROOF_POINT_25.md).

## Future: Proof Point 26 — Mesh Format Conduits — STL, PLY, FBX

Additional mesh conduits will extend the import/export surface for fabrication, scanning, and generic content pipeline workflows.

See [PROOF_POINT_26.md](./PROOF_POINT_26.md).

## Future: Proof Point 27 — 2D Conduits — SVG and DXF Export

Projected 2D outputs will add plan-view SVG and DXF export paths for lightweight documentation and CAD-centric drawing workflows.

See [PROOF_POINT_27.md](./PROOF_POINT_27.md).

## Future: Proof Point 28 — USD Conduit and Advanced Interchange

USD, MaterialX, and heightmap terrain conduits will extend interchange into higher-end rendering, spatial computing, and terrain data workflows.

See [PROOF_POINT_28.md](./PROOF_POINT_28.md).

## Future: Proof Point 41 — Face Selection and Sub-Object Editing Context

**Validates**: ADR-019 decisions 1-2 (face detection as factory responsibility, sub-object context following group pattern).

Users can enter a face-editing context on supported solid geometry, detect and highlight individual faces under the cursor, and select faces for subsequent operations. This is the foundation for push/pull direct modeling and geometric inference.

See [PROOF_POINT_41.md](./PROOF_POINT_41.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add FaceHitCandidate and factory method

**Files**: `crates/talos3d-core/src/capability_registry.rs`, `crates/talos3d-core/src/plugins/modeling/snapshots.rs`

- Define `FaceId`, `FaceHitCandidate` types.
- Add `hit_test_face()` to `AuthoredEntityFactory` with default `None` return.
- Implement for BoxFactory (6 axis-aligned faces with rotation).

#### Step 2: Implement face detection for remaining types

**Files**: `crates/talos3d-core/src/plugins/modeling/snapshots.rs`

- CylinderFactory: top cap, bottom cap, side surface.
- PlaneFactory: single face (both sides).
- TriangleMeshFactory: per-triangle Moller-Trumbore intersection.

#### Step 3: Add FaceEditContext and face highlighting

**Files**: `crates/talos3d-core/src/plugins/selection.rs`, `crates/talos3d-core/src/plugins/handles.rs`

- Add `FaceEditContext` resource.
- Double-click selected entity to enter face-editing context.
- Draw face highlight (overlay or outline + normal arrow) for face under cursor.
- Escape exits face-editing context.

#### Step 4: Add face selection and snap points

**Files**: `crates/talos3d-core/src/plugins/selection.rs`, `crates/talos3d-core/src/plugins/snap.rs`

- Click highlighted face to select. Shift-click for multi-select.
- Contribute face vertices, centroids, and edge midpoints to snap system.
- Show face info in property panel.
- `cargo clippy` clean.

## Future: Proof Point 42 — Push/Pull Modeling

**Validates**: ADR-019 decisions 3-4 (push/pull as constrained move, face extrusion modifies authored entity through snapshots).

SketchUp-style push/pull: select a face, drag along its normal, and the geometry extrudes or indents in real time. Works through the existing universal transform pipeline with numeric input, snapping, and undo.

See [PROOF_POINT_42.md](./PROOF_POINT_42.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add Custom axis constraint and push/pull activation

**Files**: `crates/talos3d-core/src/plugins/transform.rs`, `crates/talos3d-core/src/plugins/selection.rs`

- Add `AxisConstraint::Custom(Vec3)` for arbitrary direction constraint.
- Wire G key in face-editing context to enter push/pull (face-normal-constrained move).
- Project move delta onto face normal in `compute_preview`.

#### Step 2: Implement push_pull_snapshot for Box and Cylinder

**Files**: `crates/talos3d-core/src/plugins/modeling/snapshots.rs`

- Box: adjust half_extents and centre along push axis, keep opposite face stationary.
- Cylinder: adjust height and centre for cap push/pull.
- Enforce minimum dimensions.

#### Step 3: Implement plane-to-box promotion and TriangleMesh push

**Files**: `crates/talos3d-core/src/plugins/modeling/snapshots.rs`, `crates/talos3d-core/src/plugins/commands.rs`

- Plane push/pull creates a BoxPrimitive (type-changing snapshot replacement).
- TriangleMesh: translate face vertices, generate side faces, recompute normals.

#### Step 4: Visual feedback, distance echo, and Model API

**Files**: `crates/talos3d-core/src/plugins/transform.rs`, `crates/talos3d-core/src/plugins/model_api.rs`

- Add push/pull distance indicator and reference outline.
- Add `LastPushPullDistance` resource and Tab-repeat.
- Add `push_pull` operation to Model API.
- `cargo clippy` clean.

## Future: Proof Point 43 — Geometric Inference Engine

**Validates**: ADR-019 decision 5 (inference engine as shared resource feeding into snap system).

Full SketchUp-style geometric inference: edge parallelism/perpendicularity, face-plane alignment, distance matching, and distance echo. Extends PP39's axis inference into a cross-cutting precision system that works during all transforms and placement tools.

See [PROOF_POINT_43.md](./PROOF_POINT_43.md).

### Step Sequence

Each step must end with `cargo check` passing.

#### Step 1: Add InferenceEngine resource and edge collection

**Files**: `crates/talos3d-core/src/plugins/inference.rs` (new), `crates/talos3d-core/src/capability_registry.rs`

- Define `InferenceEngine`, `InferenceCandidate`, `InferenceKind`.
- Add `collect_edges()` to factory trait.
- Implement edge collection for box, wall, plane.

#### Step 2: Implement edge-parallel and perpendicular inference

**Files**: `crates/talos3d-core/src/plugins/inference.rs`, `crates/talos3d-core/src/plugins/snap.rs`

- Detect drag direction alignment with scene edges.
- Apply hysteresis (15° enter, 25° exit).
- Write inference candidate to `SnapResult.inference`.
- Draw dashed magenta (parallel) or cyan (perpendicular) gizmo lines.

#### Step 3: Implement face-plane and distance inference

**Files**: `crates/talos3d-core/src/plugins/inference.rs`

- Detect push/pull distance matching a nearby face plane.
- Detect distance matching existing entity dimensions.
- Priority ordering: locked > face-plane > edge > distance.

#### Step 4: Inference lock, distance echo, and integration

**Files**: `crates/talos3d-core/src/plugins/inference.rs`, `crates/talos3d-core/src/plugins/transform.rs`

- Shift-lock for inference commitment.
- Tab-repeat for last distance.
- Integration with placement tools.
- View menu toggle.
- `cargo clippy` clean.

## Dependency Order

```
PP1 (wall placement)           — COMPLETE
PP2 (selection, undo)          — COMPLETE, depends on PP1
PP3 (transform, snap)          — COMPLETE, depends on PP2
PP4 (geometry types, split)    — COMPLETE, depends on PP3
PP5 (openings)                 — COMPLETE, depends on PP4
PP6 (properties, save/load)    — COMPLETE, depends on PP5
PP7 (model API, AI read)       — COMPLETE, depends on PP6
PP8 (universal transforms)     — COMPLETE, depends on PP7
PP9 (AI write operations)      — COMPLETE, depends on PP8
PP10 (crate extraction)        — COMPLETE, depends on PP9
PP11 (palette + menu bar)      — COMPLETE, depends on PP10
PP12 (file import framework)   — COMPLETE, depends on PP11
PP13 (DWG/DXF import)          — COMPLETE, depends on PP12
PP14 (terrain setup)           — IN PROGRESS, depends on PP13, PP10
PP15 (terrain refinement)      — FUTURE, depends on PP14
PP16 (visual gizmo handles)    — COMPLETE, depends on PP15, PP11
PP18 (pivot + menu dispatch)   — COMPLETE, depends on PP16, PP11
PP19 (performance pipeline)    — COMPLETE, implemented out of sequence
PP20 (toolbar system)          — COMPLETE, depends on PP19, PP11
PP29 (egui chrome migration)   — COMPLETE, depends on PP20, PP11
PP17 (property panel UI)       — COMPLETE, depends on PP29, PP16
PP30 (camera projection/views) — FUTURE, depends on PP29
PP31 (file management)         — FUTURE, depends on PP6, PP29
PP32 (grouping)                — FUTURE, depends on PP13, PP17
PP33 (layer authoring)         — FUTURE, depends on PP32, PP13
PP34 (selection enhancements)  — FUTURE, depends on PP32, PP33
PP21 (material foundation)     — FUTURE, depends on PP29
PP22 (texture mapping)         — FUTURE, depends on PP21
PP23 (material library)        — FUTURE, depends on PP22
PP24 (export + glTF conduit)   — FUTURE, depends on PP23
PP25 (IFC conduit)             — FUTURE, depends on PP24, PP51
PP26 (mesh conduits)           — FUTURE, depends on PP25
PP27 (2D conduits)            — FUTURE, depends on PP25
PP28 (USD + advanced interchange) — FUTURE, depends on PP27
PP51 (definition/occurrence foundation) — FUTURE, depends on PP6, PP9
PP39 (axis inference)            — FUTURE, depends on PP40
PP40 (transform reliability)     — IN PROGRESS, depends on PP16
PP41 (face selection)            — FUTURE, depends on PP40, PP34
PP42 (push/pull modeling)        — FUTURE, depends on PP41
PP43 (geometric inference)       — FUTURE, depends on PP42, PP39
```
