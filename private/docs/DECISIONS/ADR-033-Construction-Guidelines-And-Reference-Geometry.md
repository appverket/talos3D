# ADR-033: Construction Guidelines And Reference Geometry

**Status**: Accepted  
**Date**: 2026-04-09

## Context

Talos3D can model architectural and mechanical geometry but gives users no way
to establish reference lines, angles, or construction aids that sit outside the
mesh. Precision modeling workflows depend on these: a guide line lets you
anchor snap and inference to an intent ("the wall goes at 45° to the corridor")
without that intent becoming permanent geometry.

Programs like SketchUp popularized the protractor-based guide line: click an
edge, drag to set an angle relative to it, optionally type a precise value. The
result is an infinite dashed line that lives only as a reference — it does not
triangulate, does not export, but it does participate in snap and inference for
as long as it is present.

Talos3D is missing this workflow entirely. Without guide lines users either
accept coarser geometry or litter the model with thin throwaway edges that they
must later delete. Neither is acceptable for architectural work.

A second forcing function is the AI-first principle. If the system has no formal
type for a construction guide, an agent cannot place one, cannot query the set
of guides, and cannot give the user precise structural intent in a form that
survives iteration. Guide lines need to be authored, serialized, and exposed
through MCP on the same terms as any other entity.

## Decision

### 1. `GuideLine` is a first-class authored entity

A `GuideLine` is represented in the authored model (ECS) with:

- a stable `ElementId` (same id contract as geometry entities)
- an anchor `Point3` — the ray origin, usually snapped to a vertex or edge midpoint
- a unit direction `Vec3`
- an optional finite half-length (`Option<f64>`) — `None` means infinite, `Some(d)`
  constrains the guide to a finite segment from the anchor in both directions
- a `visible: bool` flag for per-guide toggle
- a human-readable label (`Option<String>`) used in the Properties panel and in
  MCP responses

The entity is authored like any other: it carries an `AuthoredEntity` marker,
participates in selection, appears in the element tree, and serializes with the
project.

No `GuideLine` vertex or edge is ever added to the mesh B-rep. The entity is
purely reference geometry.

### 2. Snap and inference integration

Guide lines enrich two existing systems:

**Snap.** When snap is active the anchor point of any visible guide line is
offered as a candidate snap point with the `GuideAnchor` snap type. This lets
users draw geometry that starts exactly at a guide intersection.

**Inference.** The direction of a visible guide line is registered with the
inference engine as a reference edge direction. This means the usual
parallel/perpendicular inference hints fire against guide lines the same way
they fire against real edges. A guide drawn at 45° to a wall lets the user infer
parallel 45° offsets on subsequent strokes without re-entering the angle each
time.

### 3. Guide Line tool mode

A dedicated `GuideLineTool` is added alongside the existing drawing tools. Its
interaction model mirrors SketchUp's tape measure / protractor workflow:

1. **Anchor phase.** The user clicks a point in the scene. Snap is active; the
   click may land on a vertex, edge midpoint, face, or guide anchor. The
   clicked geometry becomes the anchor. If the click lands near a face edge,
   the anchor edge-snaps to that edge and the edge is remembered as the
   *reference edge* for angle measurement. If an existing guide line is
   selected when the tool starts, that guide direction is also a valid
   reference baseline.

2. **Direction phase.** The user moves the cursor. A preview ray is drawn from
   the anchor through the cursor position. The preview is constrained to the
   selected face plane when face editing has a selected face; otherwise it uses
   the hovered face plane or the current drawing plane. If a reference edge was
   recorded, the current angle relative to that edge is shown as a numeric value
   and a protractor arc is rendered in the viewport around the anchor. `Ctrl`
   snaps the angular drag to 15° increments. `X`, `Y`, and `Z` lock the guide
   direction to projected world axes while keeping it on the host plane.

3. **Confirmation.** The user either:
   - **Clicks** to confirm the direction as-is, or
   - **Types a numeric angle** (e.g. `45`) while the tool is in the direction
     phase. The typed angle is interpreted relative to the reference edge
     direction if one was captured, or relative to the host-plane tangent
     otherwise. Pressing Enter commits without moving focus to a separate text
     widget.

4. **Result.** A `GuideLine` entity is created via the command queue
   (`PlaceGuideLineCommand`) so the action is undoable.

The tool does not distinguish between architectural and mechanical contexts; it
is available whenever geometry exists to snap to.

### 4. Protractor arc rendering

When a reference edge is active during the direction phase, a thin arc is
rendered in the viewport:

- centered at the anchor point
- spanning from the reference edge direction to the current cursor direction
- labeled with the angle value at the arc tip
- rendered in screen space at a fixed radius so it does not scale with zoom

The arc is purely a tool overlay; it is not persisted and does not appear in
renders or exports.

### 5. Visual style

Persisted guide lines render as infinite dashed lines clipped to the visible
frustum. The style is:

- **Color**: `#00CCCC` (cyan) at 70 % opacity — distinct from geometry edges and
  from inference hints which use blue/magenta
- **Line pattern**: 8 px dash, 4 px gap in screen space
- **Width**: 1 px regardless of zoom
- **Anchor glyph**: a small cross (`+`) at the anchor point, same color

Guide lines are drawn in a dedicated render pass after opaque geometry and
before UI chrome, so they are never occluded by faces but are always behind the
HUD.

### 6. Visibility controls

Two levels of visibility are provided:

- **Global toggle**: `View → Guide Lines` (keyboard shortcut `Shift+G`) shows or
  hides all guide lines at once. The toggle state is stored in the viewport
  resource, not in the project file — it is a display preference, not authored
  intent.

- **Per-guide toggle**: each `GuideLine` entity carries a `visible: bool` field
  that is authored and persisted. An invisible guide does not render, does not
  offer snap points, and does not contribute to inference. It remains in the
  element tree and is accessible through MCP.

Deleting a guide line (`Delete` key while selected) is undoable via the same
command infrastructure used for geometry deletion.

### 7. MCP exposure

Guide lines are exposed through the MCP surface with entity type `"guide_line"`.
The minimal tool surface mirrors the existing entity tooling:

- `list_entities` — returns guide lines when `entity_type` filter includes
  `"guide_line"` or is absent
- `get_entity` — returns anchor, direction, finite_length, visible, label for a
  given element id
- `place_guide_line` — creates a guide line from anchor + direction, anchor +
  through-point, or anchor + angular reference (`reference_direction`,
  `angle_degrees`, `plane_normal`), plus optional length/label; returns the
  new element id
- `update_entity` — accepts anchor, direction, finite_length, visible, label as
  partial updates
- `delete_entity` — removes the guide line and purges it from snap/inference

Agents can therefore place a construction grid, align geometry to it, then delete
the guides in a single session — all through the same contract used for mesh
entities.

### 8. Persistence

Guide lines are serialized into the project file under the existing authored
entity section using the same element-id-keyed format as other entities. The
schema addition is backward-compatible: files without guide lines load cleanly
on older builds; files with guide lines loaded on older builds will silently drop
the unknown entity type (existing forward-compatibility rule).

## Consequences

### Positive

- Precision workflows previously impossible in Talos3D (angled walls, offset
  construction planes, radial guides) become accessible without throwaway
  geometry.
- The snap and inference systems become richer for all tools, not just the guide
  line tool itself.
- Agents can place and remove construction aids programmatically, enabling
  AI-driven precision workflows where the AI reasons about geometry before
  creating it.
- The finite-length variant gives users segment guides for marking specific
  distances (comparable to SketchUp's tape measure dimension marks).

### Negative

- A new entity category must be maintained across serialization, MCP, UI, snap,
  inference, and rendering. Any future refactor that touches these systems must
  account for guide lines.
- Users who create many guide lines without deleting them will accumulate visual
  clutter. The global toggle mitigates but does not eliminate this.
- Snap candidate lists grow with each visible guide anchor, which may
  perceptibly degrade snap performance in scenes with hundreds of guides.
  Spatial indexing of guide anchors (already used for vertices) should handle
  typical counts; a cap or warning at a configurable threshold is advisable.

## Relationship To Existing Decisions

- **ADR-008 (AI-Native Design Interface)**: guide lines are authored,
  inspectable, and agent-manipulable — not a hidden viewport convenience.
- **ADR-017 (UI Chrome Architecture)**: the guide line tool is surfaced in the
  tool palette; visibility is in the View menu; per-guide properties appear in
  the standard Properties panel.
- **ADR-023 (Profile-Based Solids)**: profile drawing tools benefit from guide
  lines as snap and inference references when constructing sweep paths and
  section profiles.
- **ADR-031 (Scene Lighting As Authored, AI-Visible State)**: same
  authored-entity pattern applied to a different entity category.
