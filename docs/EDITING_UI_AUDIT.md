# Editing interaction audit — October 2026

The desktop audit used native pointer and keyboard input on a saved copy of a
nested model. Model API reads corroborated selection, hierarchy, parameters and
history outcomes; they did not substitute for the editing gestures.

## Verified episodes

| Operation | Interaction and outcome |
| --- | --- |
| Selection | Click, nested double-click entry, Escape exit, containing and crossing marquees. Component boundaries remain explicit. |
| Face drawing | Selected planar face → circle, opposite-corner rectangle, or closed lines → authored profile → push/pull. |
| Push/pull | Positive depth, zero/flush depth, through cut, numeric entry, preview cancellation and undo. |
| Transforms | Move via grip plus numeric distance; rotate with axis and angle; uniform scale; context-menu rotation; sequential undo/redo. |
| Hierarchy | Group within an entered parent, select the new group, ungroup, select released members, undo and redo. |
| Clipboard | Menu copy/paste and duplicate, delete the copy, then select the surviving profile. |
| Properties | Numeric property editing, commit, undo, and keyboard focus isolation from viewport shortcuts. |
| Visibility | Select a generated body edge, hide it, and undo. |
| Persistence | Save As, Save, restart and reopen the verification copy. |

These are representative editing episodes, not a claim that every capability,
modifier gesture, geometry type or combination has been exhaustively tested.

## Repairs

- Profile selection highlights its authored operand instead of tinting its entire
  evaluated host. The Bevy mesh/material overlay is cached and removed on deselect.
- Status, breadcrumbs, coordinates and operation hints have separate layout space.
  Face and edge selection now report their actual state.
- Earlier flush profile outlines follow the visible feature result through later
  operations and undo, instead of disappearing with a hidden operand.
- Group/Ungroup update parent membership and entity lifecycle in one captured
  authored edit plan, then select the result. The command handlers are shared with
  agent command execution.
- A transform explicitly requested from a menu waits for UI input ownership to
  end. Escape consumes that pending intent without exiting a group.
- Removing a copied profile keeps the shared source hidden while a surviving
  feature still owns its presentation, preventing the source from stealing picks.
- Cancelling an incomplete face drawing consumes Escape and retains the support
  face for the next drawing tool.
- Shortcut dispatch reconstructs modifiers at the key press from ordered input
  events, including complete press/release chords delivered within one frame.

All geometry and hierarchy edits continue through authored snapshots, commands
and history. Highlights, visibility ownership and outline meshes are derived.

## Validation and performance

Core plugin tests cover the command path and distinct interaction presentation
paths: cached overlays, feature outline ownership across undo, parent visibility,
nested grouping history, deferred menu transforms, drawing cancellation and
ordered key events. The full plugin suite passed before the final cancellation
regression; that regression is also run separately. Formatting and the project's
Clippy configuration are checked. Existing unrelated Clippy warnings remain.

The installed release desktop composition was exercised on macOS with the perf
HUD. Representative settled views measured about 80–118 FPS; selected grouped
views measured about 94 FPS. Idle mesh regeneration remained zero. Feature
meshes are rebuilt only when their authored inputs or ownership change, not every
frame. These are observed local samples, not a cross-device performance guarantee.

## Verification boundaries

The native automation provider delivered modified-key gestures as letter events
without modifier key events. Toolbar/menu history and clipboard actions were
verified; physical Command shortcuts and modifier-dependent selection/navigation
were not certified by this run. The complete-event chord regression protects
Talos3D's own dispatch, but cannot recover events absent from the platform stream.

The dependency investigation covered Bevy 0.20's winit modifier reconciliation.
A possible upstream test should exercise aggregate macOS modifiers with unknown
left/right identity and modifier changes surrounding keys in the same event batch.
No dependency patch is shipped: the missing events were observed with synthetic
input, without a physical-keyboard reproduction establishing a Bevy defect.

Flush profile outlines are retained drawing boundaries, while generated body
edges are a separate subobject selection surface. This run verifies flush region
selection/push-pull and generated-body edge hide/undo; it does not certify direct
edge editing of a zero-depth profile or independent booleans on overlapping
copies of the same feature.
