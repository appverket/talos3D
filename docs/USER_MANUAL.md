# Talos3D User Manual

## Scope

This manual describes the current interaction model at a high level. The exact
available tools depend on which capabilities and setups are loaded.

Built-in definition libraries ship with the app and appear automatically in the
Definitions browser when Talos3D starts. Imported or project-local definitions
remain part of the saved document.

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

Fillet and chamfer are currently exposed through the Modeling toolbar, command
palette, and MCP/automation surfaces rather than a dedicated keyboard shortcut.
Select one source solid, then run `Create Fillet` or `Create Chamfer`.

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

## Definitions And Materials

The Definitions browser shows reusable authored families from two sources:

- bundled libraries that ship with the app
- project-local or imported libraries saved with the document

Material textures can either reference bundled app assets or embed image data
directly in the project. That keeps materials portable across native and
browser deployments even before a backend-hosted catalog exists.

The Materials window now exposes more of the physically based material model:
specular tint, transmission, thickness, IOR, attenuation, clearcoat,
anisotropy, fog participation, and depth bias are available alongside the
existing base color and texture controls.

The View menu also exposes a Renderer window for direct control over
tonemapping, exposure, ambient occlusion, bloom, and screen-space reflections.
The same renderer state is available over MCP for agent-driven workflows.

## Capability-Specific Workflows

Some interactions only appear when a capability is loaded. For example:

- walls and openings come from the architectural capability
- future domain packs may contribute other tools, panels, and commands

This is intentional. Talos3D is a platform, and the active capability set
defines the current workflow surface.
