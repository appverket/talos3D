# Authored face drawing

With Select active, click a solid or group, then click a planar face inside it.
The selected face has a GPU stipple overlay. Press **C** for a circle (centre,
radius), **P** for a rectangle (opposite corners), or **L** for lines (Enter to
finish). Drawing and snapping stay on the selected plane. The new region stays
selected; **G** starts push/pull through the shared transform tool. Positive
depth adds material, zero is flush, negative depth cuts, and a cut extending
through the supporting body opens a hole. A line without area cannot extrude.

The support object keeps its authored type and identity. A circle is four
analytic quarter arcs in a `face_profile_feature`, with parent identity, anchor,
orientation and signed depth. Its mesh is a disposable evaluation cache. Native
save/reopen, undo/redo, pointer interaction and agent commands use that authored
representation. Subsequent regions form an ordered feature dependency chain.

## Agent workflow

Discover these commands through `list_commands`, then use `invoke_command`:

1. `modeling.inspect_faces` with `element_id` returns planar face IDs and their
   world-space drawing frames (`origin`, `u_axis`, `v_axis`, `normal`).
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
Bevy gizmos. No per-edge scene raycasts are used for the selected outline.

Regression coverage exercises pointer input with snaps displaced off a tilted
plane, undo/redo, analytic circle radius, native snapshot round trips and
positive/zero/recess/through depth evaluation. The Architecture adapter has a
recovered-wall fixture with a real through-cut ray check. Live verification is
recorded on the owning issue, TALOS-czkhgugt.
