# ADR-031: Scene Lighting As Authored, AI-Visible State

**Status**: Accepted  
**Date**: 2026-04-09

## Context

Talos3D previously relied on hard-coded startup lights. That was workable for a
demo viewport, but it broke several core platform principles:

- lighting was not part of the authored scene contract
- agents could not inspect or manage light rigs through MCP
- project persistence could not distinguish between default bootstrap lighting
  and user-authored lighting intent
- UI behavior depended on implicit engine setup rather than explicit model state

The renderer control work in PP58 already made viewport look-development more
agent-visible. Lighting needed the same treatment.

## Decision

### 1. Scene lights are authored entities

Directional, point, and spot lights are represented as authored entities with
stable element ids, inspectable properties, selection support, and project
persistence.

### 2. Ambient lighting is explicit scene state

Ambient color, brightness, and lightmap participation are stored in a resource
that is serialized with the project and exposed through both UI and MCP.

### 3. Default lighting is a recoverable bootstrap rig, not hidden startup code

Talos3D seeds a daylight rig when a document has no authored lights, but that
rig is represented through the same authored light entity type as any
user-created light. A public `restore_default_light_rig` API restores it.

### 4. Human and agent workflows share one lighting model

The View -> Lights window is intentionally list-first and selection-oriented:

- create or restore lights in one place
- hand detailed editing off to the existing Properties panel
- expose the same light contract through MCP for agent use

This keeps UX compact while preserving one underlying authored model.

## Consequences

### Positive

- Light rigs are now inspectable, editable, and persistent.
- Agents can compose lighting changes through stable MCP tools.
- Startup lighting no longer depends on hidden engine-only state.
- Browser-hosted and native deployments share the same lighting contract.

### Negative

- The authored model now owns another category of scene state that must be kept
  synchronized with Bevy runtime components.
- Default-rig seeding and delete-dependency traversal need to tolerate worlds
  where some component types have never been instantiated.

## Relationship To Existing Decisions

- **ADR-008 (AI-Native Design Interface)**: lighting is exposed to agents as
  first-class scene state.
- **ADR-017 (UI Chrome Architecture)**: lighting management is surfaced through
  structured chrome rather than hidden debug menus.
- **ADR-030 (Appearance And Renderer Control Surface)**: renderer and lighting
  now form one coherent look-development contract.
