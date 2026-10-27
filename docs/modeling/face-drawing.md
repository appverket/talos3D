# Authored face drawing

With Select active, enter each enclosing group in order, then select the
individual solid/component. Click its planar face to select the face. A selected
group never allows face selection through its boundary.

Use **Create → On Face → Draw Circle on Face**, **Draw Rectangle on Face**,
or **Draw Lines on Face**. The ordinary Create Plane and Create Polyline commands
also draw on the selected face when one is active. Floor Plan tools create Draft
annotations and are separate from solid face modeling.
The selected face has a GPU stipple overlay. Press **C** for a circle (centre,
radius), **P** for a rectangle (opposite corners), or **L** for lines (Enter to
finish). Drawing and snapping stay on the selected plane. The new region stays
selected; **G** starts push/pull through the shared transform tool. Positive
depth adds material, zero is flush, negative depth cuts, and a cut extending
through the supporting body opens a hole. A line without area cannot extrude.
The transform measurement is a relative distance in metres: `0.4` pulls 400 mm,
then `-0.4` returns to flush. The agent depth command sets absolute depth.

For depth alignment, start **G**, hover a fixed edge until the **On edge**
inference appears, then click to confirm. The moving face aligns to the target
point along its normal, even when the edge is laterally offset. A supporting
face boundary returns the region flush; a farther target can set a through-cut.
The moving region is excluded as its own snap target, while the unchanged
support body remains available. Numeric entry overrides inference.

The support object keeps its authored type and identity. A circle is four
analytic quarter arcs in a `face_profile_feature`, with parent identity, anchor,
orientation and signed depth. Its mesh is a disposable evaluation cache. Native
save/reopen, undo/redo, pointer interaction and agent commands use that authored
representation. Subsequent regions form an ordered feature dependency chain.

## Agent workflow

Discover these commands through `list_commands`, then use `invoke_command`:

1. `modeling.inspect_faces` with `element_id` returns planar face IDs and their
   world-space drawing frames (`origin`, `u_axis`, `v_axis`, `normal`), boundary
   coordinates and capability-owned semantic surface references.
2. `modeling.select_face` with `element_id` and `face_id` selects the same face as
   a pointer click. Reinspect faces after topology changes; face IDs are derived.
3. `modeling.draw_face_circle` with `centre: [u,v]` and `radius`, or
   `modeling.draw_face_rectangle` with `a` and `b`, creates a flush region and
   returns its new `element_id`. `modeling.draw_face_polyline` takes `points`.
   Coordinates and lengths are metres in the selected drawing frame. Supplying
   `element_id` and `face_id` also selects the support in the same command.
4. `modeling.set_face_depth` with the region `element_id` and an absolute signed
   `depth` edits the same feature used by interactive push/pull.
5. Inspect both the support and feature with `get_entity_details`; the support
   remains its original authored type. Use geometric checks and look at a
   screenshot before claiming a successful edit.

Calling a drawing command without coordinates starts its interactive tool.
Profiles must have finite coordinates, bounded segment counts and no crossing
contour. Outlines on cached capability faces must remain inside the selected
surface and outside its existing openings. Arbitrary face crossings and new
construction semantics are not inferred from drawn outlines.

## Rendering and verification

Architecture supplies a derived `PlanarBody` when the wall mesh regenerates,
including recovered top profiles and existing openings. Picking tests only the
selected candidate body. The stipple uses the existing Bevy material/shader and
one cached mesh; it does not emit per-pixel CPU marks. Shape previews use bounded
Bevy gizmos. Flush region boundaries use retained line meshes that are rebuilt
only when authored geometry changes. No per-edge scene raycasts are used for
the selected outline. Boolean evaluation conforms shared triangle boundaries
and cancels opposite-wound slivers before publishing the derived closed shell.

Regression coverage exercises pointer input with snaps displaced off a tilted
plane, undo/redo, analytic circle radius, native snapshot round trips and
positive/zero/recess/through depth evaluation. The Architecture adapter has a
recovered-wall fixture with a real through-cut ray check. Live verification is
recorded on the owning issue, TALOS-czkhgugt.

## Editing boundaries

After entering the enclosing groups and selecting the individual component,
click a boundary edge to select it. **H** (or **Edit → More → Hide Edge**) hides its
linework without changing geometry. **Edit → More → Show Object Edges** restores the
selected object's hidden edges. These display overrides persist with the
native authored snapshot and are undoable.

For a supported authored boundary, **G** moves it along its semantic normal.
You can select a rectangular feature's side face instead. Hover the opposite
edge for **On edge** inference and confirm to collapse the feature. A negative
face-profile feature closes its cut when its width reaches zero; a positive
feature disappears when collapsed. **Delete/Backspace**, or **Edit → More → Close
Opening / Erase Feature**, removes the entire owning feature. It does not delete
an isolated mesh edge or leave an invalid shell. Undo restores the authored
feature, its dependencies and aggregate membership.

Capabilities can bind generated boundaries to their own semantic objects. The
Architecture capability binds rectangular opening reveals to the opening's
width, height, position and sill. Moving a reveal into the opening narrows it;
reaching the opposite side removes the opening and regenerates the wall.
For an opening containing a resizable product, moving a boundary also updates
its occurrence's Definition overrides, placement, hosting anchors and relation
in the same plan. The product keeps its identity and uses its authored geometry
controls; its mesh is not scaled. This is the same hosted-layout planner used by
ordinary property edits. Preview, commit, undo and agent edits change the opening
and product together. Unsupported Definition controls, invalid fitting sizes,
array-managed layouts and overlap return a refusal without partial changes.
Closing or erasing a filled opening still requires explicit product removal. Arbitrary mesh edges and ambiguous junctions do
not acquire invented semantics. Nonrectangular profile features support whole
feature erasure; freeform boundary deformation is not implemented.

Agents use `list_subobjects` and `expand_subobject_selection` to obtain stable
references. Invoke `modeling.inspect_boundary` with `reference` to discover its
owner, target, world-space normal and operation support/refusal. Then call
`apply_subobject_edit` with that reference, an operation (`move`, `push_pull`,
`erase`, `hide`, `show`) and parameters. Movement takes signed `distance` in
metres along the returned normal, or world `delta: [x,y,z]`. The registered
`modeling.edit_boundary` command offers the same operations. The viewport and
agent routes use the same representation-owned snapshot plan, including exact
preview/commit parity and reversible collapse. Edge caches use bounded derived
topology and retained Bevy line rendering; no scene-wide edge raycasts are added.

`ux_observe.inference` reports the actual snap kind, world point, hovered object,
and active boundary owner/target, so an agent can verify the same gesture without
inferring it from a screenshot. Muted geometry outside the current edit context
does not occlude inference to the active component's visible boundaries.
