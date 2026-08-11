# ADR-026: One Unified Drafting Workspace

## Status

Accepted

## Date

2026-08-11

## Context

Talos3D currently exposes several things that look like drafting modes but do
not form an authoring workspace:

- `view.apply_paper_preset` toggles a reversible white renderer preset through
  `PaperDrawingState`;
- `drafting.toggle_visibility` only shows or hides dimension annotations
  through `DraftingVisibility`;
- `drafting.toggle_sheet_preview` opens a floating raster preview produced by
  `capture_sheet` and `DraftingSheet`;
- the app declares an unused `AppMode::Drafting`; and
- vector export, `extract_drawing_geometry`, `capture_sheet`, live viewport
  overlays, and annotation rendering have partially independent projection and
  state paths.

The visible result is two apparent menu modalities—Paper Drawing and
Drafting—neither of which lets the user draft. The implementation result is
more serious: presentation state, annotation visibility, live projection,
paper capture, and export can disagree because no single object owns the
drafting session.

Drafting must support a mixed document. Authored 3D model objects are projected
onto a chosen drawing plane, while authored 2D lines, polylines, rectangles,
circles, text, dimensions, symbols, and similar annotations remain planar.
Users and agents must be able to create and edit either kind while the same
orthographic workspace is active, then emit a 2D PDF/SVG/DXF/PNG without
creating a second model.

Domain capabilities also need to specialize this workspace. Architecture may
constrain model-bearing gestures to storey semantics; naval may constrain them
to deck semantics. Those policies must reuse Drafting rather than introduce a
floorplan canvas, domain renderer, command path, or undo stack.

## Decision

### 1. Talos3D has one user-visible Drafting workspace

Core owns one runtime state with this semantic shape:

```text
DraftingWorkspaceState =
    Inactive
  | Active {
      draft_id
      plane
      policy_id?
      captured_view_state
      captured_render_state
      active_layer?
      active_style?
    }
```

The exact Rust layout may split durable draft metadata from transient session
state, but there is one authoritative active/inactive state and one public
toggle command, `drafting.toggle`.

Entering Drafting captures the current camera and renderer state, activates the
selected or newly created `Draft`, and applies the Drafting invariants. Exiting
restores the captured state exactly when it is still applicable, or falls back
to a documented normal-view default. Cancelled or refused entry changes no
history or document state.

The following cease to be independent modes:

- Paper Drawing is an internal Drafting presentation preset, not a menu item or
  state authority;
- annotation visibility is a filter inside the active draft, not entry into
  Drafting;
- DraftingSheet preview is not a separate window or modality—the main viewport
  is the live drafting surface; and
- `AppMode::Drafting` is removed unless the application adopts the core state
  as its direct state adapter. It may not remain an unused second authority.

Style, layer, clipping, and annotation filters may still exist as subordinate
properties. They cannot independently claim that the application is or is not
in Drafting.

### 2. The active drafting surface is always orthographic, black on white

While Drafting is active:

- the camera projection is orthographic;
- the active `DraftPlane` defines the view normal and its +X/+Y axes;
- the main viewport background and paper surface are white;
- projected and authored linework is black by default, with explicit
  line-weight, dash, hatch, and fill roles;
- shaded, bloom, SSR, SSAO, perspective, and material-colour controls cannot
  silently break the invariant; and
- grid, guides, selection, snapping, findings, and transient previews may use
  clearly differentiated interaction colours, but they are not exported as
  drawing content.

Front, back, top, bottom, left, right, section, and named draft views are
different orthographic plane orientations, not different modes. An isometric
orthographic view may be represented by a draft plane when useful, but exact
projection semantics still apply.

An aligned box viewed normal to one face projects to a rectangle; a sphere
projects to a circle. Arbitrarily rotated or non-primitive geometry projects to
its exact visible/cut linework. The projector must not label an approximation
as a rectangle, circle, or semantic boundary merely because the source entity
has that broad 3D kind.

### 3. `Draft` is a first-class drawing-metadata container

Core owns a durable drawing-metadata entity with this semantic contract:

```text
Draft {
  stable identity
  name
  DraftPlane { origin, x_axis, y_axis, normal }
  scale and paper/layout policy
  layer/style defaults
  member references
  visibility/cut policy
  optional domain policy id
  provenance
}

DraftMember =
    ModelObject { element_id, projection_role, optional override }
  | Annotation { annotation_id, layer, style }
```

A `Draft` contains references, not cloned geometry. A 3D object remains one
ordinary authored model entity and may appear in more than one draft. Removing
membership removes it from that draft only; deleting the object remains an
explicit model operation. Draft membership, projection settings, and
annotations persist through the drawing-metadata boundary established by
ADR-025.

The draft plane owns the one coordinate conversion used by placement, picking,
snapping, annotation rendering, projection, and export:

```text
draft 2D <-> draft plane 3D <-> world 3D <-> normalized drawing scene
```

No tool or exporter may maintain a private version of this transform.

### 4. Drafting authors both 2D annotations and canonical 3D model entities

The first generic 2D primitive set is:

- `DraftLine` and `DraftPolyline`;
- `DraftRectangle`;
- `DraftCircle` and arc;
- `DraftText`; and
- existing dimensions, guide lines, symbols, hatches, and callouts after their
  migration to the same annotation contract.

These are stable drawing-metadata entities expressed in draft-plane
coordinates. They support selection, snapping, transforms, styles, layers,
atomic history, persistence, UI requests, MCP requests, and export. They are
not zero-thickness meshes and do not enter model AABBs or semantic validation
unless an explicit conversion request creates model content.

3D creation remains ordinary model authoring. While Drafting is active,
existing box, sphere, profile/extrusion, Definition/Occurrence, and domain
creation requests consume the active plane for placement and membership. They
still produce canonical authored model entities through the same edit-plan,
command, admissibility, and history path used outside Drafting.

Drawing a rectangle and creating a box are distinct typed intents even when
they begin with the same two planar points. The UI and agent surface must make
that distinction inspectable; a client may not infer depth or silently convert
annotation geometry into a model entity.

### 5. One normalized `DrawingScene` feeds live presentation and export

Core owns one backend-independent, transient projection result:

```text
DrawingScene {
  draft_id
  source_model_revision
  plane and visibility/cut policy
  primitives[] { stable primitive id, semantic owner id, geometry, role, style }
  annotations[]
  bounds
  findings[]
}
```

The exact representation may group primitives for performance. Stable semantic
ownership, normalized geometry, cut/visibility roles, and revision identity are
binding.

The live viewport consumes the scene through a Bevy-native, batched,
incremental presentation backend. PDF, SVG, DXF, and PNG consume the same scene
through serialization/layout backends. `DraftingSheet` may remain as a
paper-millimetre layout and serialization value derived from `DrawingScene`; it
is not a second projector, source document, live mode, or alternate semantic
scene.

`extract_drawing_geometry`, `capture_sheet`, live overlays, and export must
converge behind this contract. Compatibility wrappers may exist only during a
measured migration and must delegate to the one scene builder.

### 6. UI gestures and agents share one request and edit-plan path

Core exposes inspect, enter, exit, create/select draft, set plane, manage
membership, create/edit annotations, project, and export as typed requests.
Human tools translate pointer gestures into the same requests that agents call.

Every mutation uses the shared semantic edit-plan lifecycle, command queue,
admissibility checks, and history. The UI may cache presentation and hit-test
data; it may not reproduce placement, membership, geometry, conversion, or
domain-policy rules locally. MCP may not manipulate a private sheet model.

Entering/exiting Drafting is presentation/session state and is not itself an
authored model mutation. Creating a `Draft`, changing its durable plane or
membership, adding an annotation, or creating a model entity is an authored
document edit with normal preview/apply/undo semantics.

### 7. Domain specialization is a registered policy, not a new workspace

Core provides a `DraftPolicy` extension point that may:

- contribute typed construction requests and tools;
- admit, refuse, or attach obligations/findings to model-bearing requests;
- restrict eligible member classes or projection roles;
- contribute semantic projection primitives; and
- expose domain-specific inspection and repair guidance.

A policy cannot create a second active-mode state, coordinate transform,
projector, renderer, selection system, edit plan, command queue, history stack,
annotation substrate, or export path.

Architecture's Storey Plan is defined outside core as a constrained Drafting
policy. Naval deck plans and other domains follow the same composition rule.

### 8. Persistence separates authored drawing metadata from derived scenes

Persist:

- drafts and stable draft identity;
- planes, scale/layout and visibility/cut policy;
- member references and projection roles;
- 2D annotations, styles, layers, and provenance; and
- the registered domain policy id and policy-owned durable settings.

Do not persist as authority:

- `DrawingScene`;
- `DraftingSheet` capture output;
- raster previews;
- projected silhouettes, cut-line caches, hit-test indexes, GPU batches, or
  export bytes; or
- transient entry/exit baselines.

Derived state rebuilds deterministically from the authored model, drawing
metadata, active draft, and source model revision.

### 9. Migration removes duplicate authorities in an explicit order

1. Add `Draft`, `DraftPlane`, `DraftingWorkspaceState`, the policy registry,
   and `drafting.toggle` without changing export output.
2. Route Paper Drawing enter/exit through the one controller; remove the
   user-visible Paper Drawing command and `PaperDrawingState` authority.
3. Move `DraftingVisibility` under active-draft filters; retire
   `drafting.toggle_visibility` as a mode-like menu command.
4. Replace the floating DraftingSheet preview with the main viewport backend.
5. Introduce `DrawingScene` and adapters for every existing projector/capture
   caller; prove normalized parity before deleting compatibility wrappers.
6. Migrate legacy dimensions and guide lines into the generic annotation
   membership contract without losing stable ids or document metadata.
7. Remove unused application `AppMode::Drafting` state and stale Paper Drawing
   naming from UI, MCP descriptions, tests, and export gating.

During migration, old entry points may be aliases to `drafting.toggle`; they
may not mutate independent state. Projects with legacy drawing metadata load
through an idempotent migration and save only the canonical representation.

### 10. DRY ownership is an acceptance invariant

| Concern | Sole owner | Forbidden duplicate |
|---|---|---|
| Active/inactive session | `DraftingWorkspaceState` | Paper/App/preview mode flags |
| Drawing coordinates | `DraftPlane` conversion | tool- or exporter-local transforms |
| Durable drawing scope | `Draft` + membership | sheet-owned or canvas-owned model copies |
| Mutation planning | shared edit-plan/command path | UI/MCP/private drafting command stacks |
| 3D-to-2D semantics | normalized `DrawingScene` builder | separate live and export projectors |
| Live pixels | batched/incremental presentation backend | unbounded immediate-mode CPU redraw |
| Paper output | layout/serialization from `DrawingScene` | recapture of an unrelated viewport model |
| Domain constraints | registered `DraftPolicy` | floorplan/deck sibling workspaces |

Code review and proof must reject a change that makes two components
authoritative for any row, even when their current outputs happen to match.

### 11. Performance remains a shipping gate

The live drafting surface must use Bevy-native/GPU-batched or measured bounded
paths. It may incrementally invalidate only affected scene primitives and
spatial-index entries. Unbounded per-frame CPU mesh/plane intersection,
full-sheet PNG re-encoding, egui-painter per-edge drawing, and per-entity or
per-edge ray casting are not shippable live implementations.

CPU reference extraction may remain for offline export or regression oracles.
If exact live projection misses its recorded budget after batching, caching,
incremental invalidation, and spatial indexing are investigated, keep live
Drafting feature-gated rather than substituting approximate semantics.

## Consequences

### Positive

- The user enters one workspace that actually supports authoring.
- 2D annotations, 3D model objects, live projection, and paper output remain
  visibly and semantically coherent.
- The same Draft can produce several formats without format-specific model
  state.
- Storey, deck, and later domain plans reuse a public platform mechanism.
- Agents reason over stable draft/member/primitive identity instead of pixels.
- Removing duplicated authorities makes preview, undo, persistence, and export
  failures easier to localize.

### Tradeoffs

- Generic annotation primitives and draft membership require a real migration
  from dimension-specific document defaults.
- Exact visible/cut projection and incremental invalidation are more work than
  rasterizing a viewport every 500 ms.
- A 3D object referenced by several drafts needs explicit membership and
  per-draft projection policy rather than implicit global inclusion.
- Domain tools must register policies and typed requests instead of drawing
  directly into a private canvas.

## Rejected alternatives

- Keep Paper Drawing as a renderer mode and Drafting as annotation visibility.
- Rename the two menu items while retaining separate state.
- Treat the floating DraftingSheet preview as the drafting workspace.
- Persist projected 2D copies of 3D model objects inside each sheet.
- Implement 2D annotations as thin 3D meshes.
- Give live view and export separate projectors and test that they usually
  match.
- Let floorplan or deck plan own a separate camera, canvas, tool set, or undo
  stack.
- Infer 3D depth from an untyped rectangle gesture.

## Proof obligations

Acceptance requires:

1. one visible Drafting toggle and one observable state over UI and MCP;
2. exact camera/render restoration on exit and orthographic black-on-white
   invariants while active;
3. creation, selection, edit, persistence, undo/redo, and export of line,
   polyline, rectangle, circle/arc, text, and dimensions;
4. plane-aware creation and editing of representative 3D primitives and
   semantic objects without cloned geometry;
5. normalized live/PDF/SVG/DXF/PNG scene parity with stable owner ids;
6. save/reload of drafts, membership, and annotations with derived-scene
   rebuild;
7. UI/MCP request and edit-plan parity;
8. a policy-composition proof using at least one external domain; and
9. measured live-frame, interaction-latency, export-latency, and memory evidence
   with no forbidden per-frame path.
