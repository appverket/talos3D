# Talos3D User Manual

## Scope

This manual describes the current interaction model at a high level. The exact
available tools depend on which capabilities and setups are loaded.

## Core Interaction Pattern

- Select with the mouse
- Press a tool shortcut to create geometry
- Press `G`, `R`, or `S` to transform
- Use `Esc` to cancel or return to Select
- Use `Enter` to confirm numeric transforms when applicable

## Common Tool Shortcuts

| Key | Tool |
| --- | --- |
| `Esc` | Select |
| `B` | Box |
| `C` | Cylinder |
| `P` | Plane |
| `L` | Polyline |
| `W` | Wall (architectural capability) |
| `O` | Opening (architectural capability) |

## Common Edit Shortcuts

| Key | Action |
| --- | --- |
| `G` | Move or push/pull, depending on context |
| `R` | Rotate |
| `S` | Scale |
| `Delete` / `Backspace` | Delete selection |
| `Ctrl/Cmd+Z` | Undo |
| `Ctrl/Cmd+Shift+Z` | Redo |
| `Ctrl/Cmd+G` | Group |
| `Ctrl/Cmd+Shift+G` | Ungroup |

## Face Editing

Double-click a supported solid to enter face editing.

Current face-edit workflows include:

- selecting semantic faces
- cap-oriented push/pull on profile-based solids and face profile features
- drawing a closed profile on a face and turning it into an authored feature

Push/pull now follows authored semantics rather than blindly following rendered
topology. If a face is blocked, the reason should come from the authored solid
constraints.

## Camera

| Input | Action |
| --- | --- |
| Right-drag | Orbit |
| Middle-drag | Pan |
| Scroll | Zoom |

The camera toolbar provides:

- preset views for `Top`, `Left`, `Right`, and `Bottom`
- a `Perspective` / `Isometric` mode switch
- a focal length control for the perspective lens

## Status Bar

The status bar reports:

- active tool or mode
- contextual command hints
- transform and face-edit state
- validation or feedback messages

## Capability-Specific Workflows

Some interactions only appear when a capability is loaded. For example:

- walls and openings come from the architectural capability
- future domain packs may contribute other tools, panels, and commands

This is intentional. Talos3D is a platform, and the active capability set
defines the current workflow surface.
