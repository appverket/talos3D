# Talos3D

Talos3D is a 3D platform built around authored geometry, AI-assisted
inspection, and capability-driven extensibility.

Talos3D is also a time-permits open source project. The code is public and
reusable, but there is no default support or roadmap commitment from the
maintainer. Read [../GOVERNANCE.md](../GOVERNANCE.md) and
[../SUPPORT.md](../SUPPORT.md) alongside the technical docs.

The platform is designed so that:

- humans and AI operate on the same authored model
- capabilities can be loaded, composed, and packaged independently
- domain extensions such as architecture remain examples of the public
  extension model rather than hard-coded exceptions
- geometry semantics stay legible even when realized through evaluated bodies
  and render meshes

## Start Here

- [Product Overview](./PRODUCT.md)
- [Render UX Stabilization Plan](./PLAN_RENDER_AND_UX_STABILIZATION.md)
- [ADR 024: Render/View State Coordination](./ADR-024_RENDER_VIEW_STATE_COORDINATION.md)
- [Drawing Metadata and Section Views Plan](./PLAN_DRAWING_METADATA_AND_SECTION_VIEWS.md)
- [ADR 025: Drawing Metadata Boundary](./ADR-025_DRAWING_METADATA_BOUNDARY.md)
- [MCP Model API](./MCP_MODEL_API.md)
- [Scenario Files](./SCENARIO_FILES.md)
- [Assistant Chat](./ASSISTANT_CHAT.md)
- [Developer Onboarding](./DEVELOPER_ONBOARDING.md)
- [Engine Fork Workflow](./ENGINE_FORK_WORKFLOW.md)
- [Core Principles](./CORE_PRINCIPLES.md)
- [Platform Architecture](./PLATFORM_ARCHITECTURE.md)
- [Extension Architecture](./EXTENSION_ARCHITECTURE.md)
- [Capability Plugin API](./CAPABILITY_PLUGIN_API.md)
- [System Architecture](./SYSTEM_ARCHITECTURE.md)
- [Domain Model](./DOMAIN_MODEL.md)
- [User Manual](./USER_MANUAL.md)
- [Terrain Site Drape Workflow](./TERRAIN_SITE_DRAPE_WORKFLOW.md)
- [Drone Survey Pipeline Notes](./DRONE_SURVEY_PIPELINE_NOTES.md)
- [Glossary](./GLOSSARY.md)

## Current Direction

Talos3D is currently oriented around:

- profile-based solids are first-class authored geometry
- face-drawn protrusions and recesses remain semantic features after commit
- fillet and chamfer remain authored feature nodes instead of mesh-only edits
- interaction semantics are derived from authored affordances
- evaluated body summaries expose facts such as connectedness and volume to AI
- the authored model now includes first steps toward semantic assemblies and
  typed relations for higher-order domain structure
- the MCP surface now exposes vocabulary discovery plus assembly and relation
  inspection/creation tools
- viewport appearance and scene lighting are exposed through public renderer
  and lighting state rather than hidden editor-only startup code
- the in-app assistant is a first-class client of the same MCP surface rather
  than a privileged private editor hook
- the geometry layer stays compatible with future definition DAGs, including
  parameterized paradigms like MultiSurf

## Public Platform Positioning

Talos3D is being documented as an open-source platform with a strong extension
surface.

The architecture supports:

- open-source community capabilities
- first-party reference extensions
- private enterprise capability packs
- commercial add-ons built against the same registries and contracts

The architectural capability in this repository is the canonical example of how
a domain extension can compose on top of the shared platform.

That public positioning should be read as an ecosystem model, not a promise
that Appverket LLC will build or support every part of that ecosystem itself.
