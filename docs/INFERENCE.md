# Cursor inference

Inference supplies a precise point to an interactive tool. It does not create
constraints, change the model, or add history entries by itself. The preview
and accepted edit still use the existing command or authored edit plan. Agents
can use those same commands with explicit coordinates; hover timing, pixel
apertures and transient guide locks are intentionally presentation-only.

## Using inference

Hover a visible endpoint, edge midpoint, center or surface. A marker and a
near-cursor label identify the relation. Endpoint markers are squares, midpoints
triangles, centers circles with a cross, and intersections diamonds. Markers
remain approximately twelve logical pixels across at different zoom levels.

While moving or drawing a polyline/Draft line, pause over a point or edge for
350 milliseconds to acquire it as a reference. Up to four recent references
are retained. Continue drawing to find parallel, perpendicular, extended-edge
and from-point alignments. Perpendicular means perpendicular in the drawing
plane. Guide intersections must intersect in 3D, not merely appear to cross on
screen. Acquired edges and relative guides are magenta; axis guides use Talos3D's
existing axis colors. Exact point hits take precedence over unlocked guides.

| Control | Behavior |
| --- | --- |
| Hold Shift | Hold the current direction; release to resume automatic inference |
| Right arrow | Toggle X lock |
| Up arrow | Toggle Y lock (Talos3D's vertical axis) |
| Left arrow | Toggle Z lock |
| Down arrow | Lock the current parallel/perpendicular guide, or release an arrow lock |
| I | Cycle all linear relations, edge relations only, and linear inference off |
| Hold Ctrl | Temporarily bypass snapping/inference |
| Tab | Cycle overlapping point candidates; when there is no ambiguity, repeat the last Move distance |
| Esc | Cancel the current tool stage and clear its inference lock |

A locked guide is solid and says **Locked**. Moving the pointer updates the
position along the guide: locking never freezes a cursor coordinate. Referencing
an exact point while locked projects that point onto the guide. Numeric transform
entry and explicit transform constraints remain authoritative.

Draft tools only accept points and directions on their authored Draft plane.
Modeling Polyline and primitive placement also retain their existing drawing
plane, so an axis lock normal to that plane is unavailable; use the intended
drawing plane for these tools. This preserves closed-polyline profile creation
without silently projecting a 3D preview into a different committed shape.
Move and free Tape support 3D targets. Draft Rectangle also infers a square. Analytic tangent points are available from
an external coplanar point to an authored cylinder's cap circles; arbitrary
imported curves do not claim analytic tangency. Such curves still expose their
available authored topology. This is not a claim of complete SketchUp parity.

Point feedback is shared by creation, Move, Tape, guide and dimension tools.
Directional inference is enabled for Move, Polyline, Draft Line/Polyline and
free point-to-point Tape. Tape's explicit axis/perpendicular measurement modes
retain their existing meaning. Guide and dimension tools retain their own
host-plane and placement constraints.

## Saved construction guides

Cyan lines with a small cross at their anchor are saved **Guide Line** objects.
They are independent of the red X / blue Z ground axes and gray grid. Magenta
lines are temporary inference highlights, including acquired guide references;
the “On edge” label indicates snapping, not selection.

Press **G** (Create → Guide Line), click an anchor or edge, then drag/click the
second point. **X/Y/Z** constrain direction; a typed angle followed by Enter
sets an angle in the current host plane. Guides may lie on faces or tilt in 3D;
they are not restricted to the ground plane. Press Esc to return to selection.

Guides created while editing a group belong to that group. A normal click on
any part of such a guide selects its enclosing group, even beyond the building:
the guide extends there. **Cmd-click** (macOS) / **Ctrl-click** selects the guide
itself through nested groups. Alternatively, enter the group first or select
the guide in the Outliner.

With the guide selected, drag its anchor grip to move it, or edit **anchor** in
Properties for exact coordinates in metres. Change **direction** to reorient
it. To put a guide on the horizontal ground plane, both anchor Y and direction
Y must be zero; direction must remain nonzero. **Label** names a new guide.
**Half-length (m; 0 = infinite)** limits its reach on each side of the anchor;
a value of 5 gives a 10 m segment. Infinite guides are displayed for 500 m on
each side. These edits use the normal undo history.

Use the anchor and line as snap references while drawing or moving geometry.
Hovering the guide can show “On edge”; acquiring it can highlight it magenta.
**Shift+G** (View → Guide Lines) hides/shows guides and disables/enables their
picking and snapping. It does not delete them or change the world axes.

## Implementation and performance

The existing cursor surface ray identifies one visible owner. Its canonical edge
cage is cached by owner and model revision, with at most 4,096 edges. Dense
editable bodies are rejected before cloning topology. Supported non-solid
entities can provide their existing `snap_segments` instead. Bounds corners are
not geometric endpoints and are no longer offered as such.

Factory snap points and hovered authored handles are cached across unchanged
frames. The cache invalidates on model revision, hover owner, editing context,
transform mode, X-ray mode and layer/guide visibility changes. Candidate processing
is bounded to 4,096 factory points plus 4,096 authored handles; X-ray snapping in
very dense scenes may therefore omit candidates. Hovering a visible surface
narrows the source. The ordinary visibility policy remains in force.

Acquisition retains four references. Guide generation and intersection checks
operate only on that bounded set. All line/marker geometry uses a dedicated Bevy
gizmo batch, with constant-size markers, a small depth bias, and at most sixteen
dash segments per active guide. egui only lays out the tooltip text.

Regression coverage exercises Shift release, arrow height projection, tool/UI
ownership, point precedence, drawing-plane compatibility, true 3D intersections,
circle tangency, cache scaling, and the actual Move preview/commit/undo path.
