# ADR-030: Appearance And Renderer Control Surface

**Status**: Accepted  
**Date**: 2026-04-09

## Context

Talos3D already exposed materials and a small real-time render pipeline, but
the controllable surface was thinner than the underlying Bevy capabilities.
That created two problems:

- authors and agents could not reliably tune important appearance parameters
  such as transmission, IOR, attenuation, clearcoat, or anisotropy
- viewport rendering state could not be adjusted in a structured way even
  though Bevy already exposes controllable tonemapping, exposure, SSAO, bloom,
  and SSR components

Because Talos3D is AI-first, these controls must not live only in human-only UI
widgets or ad hoc engine code. They need a stable, inspectable contract.

## Decision

### 1. Material editing tracks Bevy's useful PBR controls

The Talos3D material contract will expose the practically useful subset of
Bevy `StandardMaterial`, including:

- specular tint
- diffuse and specular transmission
- thickness and IOR
- attenuation distance and color
- clearcoat and clearcoat roughness
- anisotropy strength and rotation
- unlit, fog participation, and depth bias

Talos3D does not need to mirror every Bevy field immediately, but it should
prefer a stable authored contract over hiding major renderer-relevant controls.

### 2. Viewport renderer state is a first-class resource

Viewport renderer controls are treated as explicit authored viewport state,
backed by a resource and surfaced through both UI and MCP. The supported
surface includes:

- tonemapping
- manual exposure
- SSAO enablement, quality, and thickness heuristic
- bloom enablement and shaping controls
- SSR enablement and quality controls

### 3. UI and MCP expose the same logical control surface

Human operators may use editor windows, while agents use MCP tools. Both must
target the same underlying structures so automation does not depend on UI
simulation.

### 4. Defaults remain conservative and interactive

The default control surface should start from visually stable defaults rather
than maximum effect intensity. Advanced controls can exist without forcing a
cinematic look on every document.

## Consequences

### Positive

- Material authoring becomes materially closer to the renderer's real
  capabilities.
- Agents can inspect and adjust appearance without brittle UI driving.
- Viewport look-development becomes repeatable and scriptable.

### Negative

- More appearance controls increase UI and API complexity.
- Some Bevy parameters are renderer-specific heuristics rather than portable
  physical truth, so documentation needs to describe them carefully.

## Relationship To Existing Decisions

- **ADR-008 (AI-Native Design Interface)**: the same appearance controls are
  available to humans and agents.
- **ADR-015 (Material and Texture Architecture)**: this decision extends the
  editable material contract rather than replacing it.
- **ADR-017 (UI Chrome Architecture)**: renderer controls are integrated as
  structured chrome, not hidden debug state.
