# Developer Onboarding

## Purpose

This is the fastest path to becoming productive in the Talos3D codebase.

It is also the fastest way to understand the project's practical boundaries:
Talos3D is open source and intentionally extensible, but it is maintained on a
time-permits basis and should not be approached as a staffed product team.

## What Talos3D Is

Talos3D is an AI-first 3D platform, not just a modeling application. The code
is organized so that:

- the core platform provides shared runtime, commands, viewport, and registries
- capabilities deliver feature sets
- setups bundle capabilities into workflows

The architectural package in this repository is a reference extension that
demonstrates the public platform model.

## Read In This Order

1. [README.md](../README.md)
2. [AGENTS.md](../AGENTS.md)
3. [Engine Fork Workflow](./ENGINE_FORK_WORKFLOW.md)
4. [MCP Model API](./MCP_MODEL_API.md)
5. [Governance](../GOVERNANCE.md)
6. [Support](../SUPPORT.md)
7. [Core Principles](./CORE_PRINCIPLES.md)
8. [Platform Architecture](./PLATFORM_ARCHITECTURE.md)
9. [Extension Architecture](./EXTENSION_ARCHITECTURE.md)
10. [Capability Plugin API](./CAPABILITY_PLUGIN_API.md)
11. [System Architecture](./SYSTEM_ARCHITECTURE.md)
12. [Domain Model](./DOMAIN_MODEL.md)
13. [Glossary](./GLOSSARY.md)

## Workspace Shape

```text
src/
  main.rs                      -> app bootstrap

crates/talos3d-core/
  core platform
  modeling workbench
  command/history substrate
  model API / MCP server

crates/talos3d-architectural/
  architectural reference capability pack

crates/talos3d-terrain/
  terrain/site reference capability pack
```

## Core Mental Model

### 1. Authored state comes first

Meshes, highlights, previews, and caches are derived artifacts.

### 2. Commands are the write path

Tools collect intent. Commands commit authored changes. History owns undo/redo.

### 3. Capabilities are the extension unit

A feature should usually live inside a capability with explicit registration.

### 4. Geometry semantics matter

AI should inspect authored geometry meaning directly. Evaluated bodies provide
derived facts such as connectedness, manifold status, and volume.

### 5. Definition graphs must stay graph-friendly

Do not bake tree-only assumptions into the platform. The current system must
stay compatible with future DAG-based parameterized geometry as well as current
primitive/profile/feature workflows.

## Development Commands

```bash
cargo check
cargo clippy -- -W clippy::all
cargo test
cargo run
cargo run --features model-api
```

## Engine Dependency Work

If your task touches:

- Bevy
- egui
- `bevy_egui`
- editor panel sizing behavior
- engine upgrades or dependency tracking

read [Engine Fork Workflow](./ENGINE_FORK_WORKFLOW.md) before making changes.

That document defines:

- which repository should own which kind of fix
- which branches are allowed to track upstream `main`
- how local path-based builds should be wired
- how to refresh forks without increasing project confusion

## Documentation Workflow

The public docs are Markdown-first and can be turned into a site with MkDocs.

```bash
mkdocs serve
```

When terminology changes, update:

- `README.md`
- the relevant files in `docs/`
- glossary entries if a term becomes architectural vocabulary

## Where To Put New Work

- Core runtime or registries -> `talos3d-core`
- Generic modeling entities or tools -> `talos3d-core/src/plugins/modeling`
- Domain-specific rules and entities -> domain crate such as
  `talos3d-architectural`
- Public docs -> `docs/`

## What Good Contributions Look Like

- explicit authored semantics
- stable extension boundaries
- deterministic command behavior
- AI-visible model state
- documentation updated alongside code
- low maintainer overhead relative to project value
