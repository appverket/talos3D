# Platform Architecture

## Core Shape

Talos3D is organized as:

```text
Core Platform
  -> shared ECS runtime, commands, history, viewport, registries, AI/model API

Capability Modules
  -> feature delivery and extension packaging unit

Setups
  -> curated bundles of capabilities plus UI defaults
```

Capabilities are the primary extension unit. Setups are packaging and workflow
bundles.

## Core Platform Responsibilities

The core platform owns:

- app assembly and plugin composition
- command execution and history
- authored persistence boundaries
- semantic assembly and relation primitives plus vocabulary registries
- selection, transform, and viewport state
- shared UI chrome and command surfacing
- model inspection and AI control surfaces
- public registries for capabilities, commands, icons, toolbars, formats, and
  authored entity factories
- the `talos3d-capability-api` SDK boundary that re-exports the supported
  extension surface

The core platform should not own discipline-specific entities.

## Capability Modules

A capability can contribute:

- authored entities and definition nodes
- assembly and relation vocabulary
- tools and interaction systems
- commands and schemas
- panels and UI surfaces
- import/export formats
- analysis and validation logic
- AI-visible semantics

A capability is the unit that should be buildable, packageable, and explainable
to third parties.

## Setups

A setup bundles capabilities into a workflow:

- modeling setup
- architectural setup
- terrain setup
- future naval/mechanical/manufacturing setups

Setups may be open-source, curated by a community, or sold as commercial
bundles. That only works if capabilities remain the real architectural unit.

## Public Product Boundary

Talos3D should be open-source as a platform. The architecture should support:

- open reference capabilities
- third-party community capability crates
- private enterprise capability packs
- premium first-party or third-party add-ons

The architectural setup currently in-tree is the reference example of a domain
extension, not a special architectural tier.

The platform is publishable as a source-level Rust extension system before
dynamic plugin loading exists. The practical bar is a stable SDK crate,
manifest metadata, validation, and successful out-of-tree capability builds.

## Geometry Direction

The platform geometry model follows ADR-023:

- authored definitions remain primary
- evaluated bodies sit above mesh generation
- profile-based solids and authored features are first-class
- semantic geometry summaries are exposed to AI
- the definition model remains compatible with future DAG-based paradigms

## Architectural Summary

Talos3D is a platform first. Features arrive through capabilities. Setups bundle
those capabilities for a domain. The public codebase should make that layering
obvious.
