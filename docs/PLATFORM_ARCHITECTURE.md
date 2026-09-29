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

## Content Catalogs And Storage

The platform distinguishes between:

- bundled catalogs shipped with the app
- project-local authored data persisted with the document
- future remote catalogs or project stores provided by a backend

Built-in reusable definitions should load from bundled data rather than
desktop-only filesystem conventions. That keeps the same authored content model
usable in native builds and browser deployments.

Project persistence should cross an abstract storage boundary. A native setup
can interpret storage keys as file paths, while a browser or hosted deployment
can map the same operations to Firebase, Supabase, or another backend-owned
store for accounts, projects, and shared catalogs.

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

## Workbenches

A workbench presents a curated workflow built from capabilities:

- modeling workbench
- architectural workbench
- terrain workbench
- future naval/mechanical/manufacturing workbenches

Workbenches may be open-source, curated by a community, or sold as commercial
bundles. That only works if capabilities remain the real architectural unit.

## Public Product Boundary

Talos3D should be open-source as a platform. The architecture should support:

- open reference capabilities
- third-party community capability crates
- private enterprise capability packs
- premium first-party or third-party add-ons

The architectural capability currently in-tree is the reference example of a
domain extension, not a special architectural tier.

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

## Captured edit plans

`plugins::authored_edit_plan` owns the transient `AuthoredEditPlan` carrier.
Capability planners provide ordered before/after authored snapshots, semantic
intents, and diagnostic context. The published candidate is immutable and has a
distinct ID, document/revision fence, and content digest. `AuthoringScript`
remains the durable procedural representation; plans are not project data.

`HistoryPlugin` installs a bounded registry (64 candidates and 64 interactions;
4 MiB serialized content per candidate and per interaction's original snapshots).
An interaction retains its initial revision and originals while replacing its
active candidate. Cancellation, eviction and consumption remove access by ID;
republishing recovered content creates a new identity. New interactions release
those from older revisions. Consumers holding an `Arc` must release it when
presentation or history no longer needs it; registry limits do not bound history.

`queue_captured_plan` refuses pending history work, stale candidates, occupied
created identities, and mismatched before snapshots. History repeats the guard
immediately before mutation and runs the shared semantic admissibility kernel.
A successful candidate applies its captured snapshots as one undoable command;
redo uses the same content. Snapshot implementations own their authored fields
and dependency ordering. Retyping needs an explicit capability migration;
arbitrary resource or library writes are outside this snapshot carrier.

The shared modifier stage merges capability callbacks and the existing transform
callbacks by descending priority, with stable registration order inside each
registry. At equal priority generic callbacks run before compatibility callbacks.
`prepare_transform_edit` is the pure stage consumed by viewport calculation and
the retained `apply_transform_plan_modifiers` compatibility wrapper. Dependent
original snapshots come from the same capability factories for both paths.
Model API transforms and linked-model placement still call that wrapper; the
existing terrain planting callback runs unchanged through an adapter.

`talos3d_capability_api::edit_plans` exposes the transient planning surface.
Callbacks mutate `EditPlanDraft` before capture and receive capability-owned,
typed ephemeral input. They must not mutate the world. Request kinds keep
unrelated operations out of a callback. Duplicate generic modifier IDs refuse
registration. Draft context and semantic intents are consumed by captured-plan
callers; the legacy snapshot wrapper currently only returns before/after state.

Exact-candidate interactive application and MCP exposure are the next integration
gate. It must restore any transient authored preview before preflight, apply the
exact displayed candidate, exercise the actual presentation path, and pass the
full measured performance gate. Modifier-stage measurements alone do not establish
frame-time or drag latency.

## Architectural Summary

Talos3D is a platform first. Features arrive through capabilities. Setups bundle
those capabilities for a domain. The public codebase should make that layering
obvious.
