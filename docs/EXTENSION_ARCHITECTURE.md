# Extension Architecture

## Purpose

This document defines how Talos3D is extended without changing the core
platform.

## Extension Hierarchy

```text
Workbench
  -> curated workflow built from capabilities

Capability
  -> primary extension, packaging, and product unit

Contribution Types
  -> tools, commands, entities, semantic vocabularies, panels, formats, analyzers, AI semantics
```

## What A Capability Owns

A capability may contribute:

- authored entities or definition nodes
- assembly and relation vocabulary
- commands and schemas
- tools and interaction systems
- import/export integrations
- panels and UI surfaces
- domain rules and validators
- AI-readable inspection metadata

This is true whether the capability is:

- shipped with the public repository
- community maintained
- enterprise private
- commercial and closed source

## Architecture As Reference Extension

The architectural package in this repository should be treated as a reference
extension that happens to be maintained in-tree. It demonstrates how a domain
package composes on top of the same public registries and contracts that future
third-party extensions will use.

The concrete boundary for that is the `talos3d-capability-api` crate. In-tree
reference extensions should prefer that SDK surface over direct registration
imports from `talos3d-core`.

## Dependency Model

Capabilities form an explicit dependency graph.

Typical shape:

```text
core platform services
  -> modeling capabilities
  -> domain capabilities
```

Setups activate a dependency-consistent subset of capabilities. They do not
bypass capability boundaries.

## AI-Native Contract

Capabilities must preserve Talos3D's AI-first guarantees:

- model state is inspectable
- commands are invokable through structured APIs
- semantics are discoverable without renderer archaeology
- command execution is deterministic and replayable

## Packaging Intent

The extension model should support:

- open-source crates
- marketplace capability packs
- private/internal bundles
- paid first-party extensions

That means the registration surface must stay explicit, documented, and stable.

The expected repo shape is:

- `talos3d` for the platform and reference capabilities
- separate private or public repos for capability packs
- capability packs targeting the SDK crate and declaring explicit manifest
  metadata
