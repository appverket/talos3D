# Capability Plugin API

## Purpose

This document describes the public registration surface that capability modules
use to extend Talos3D.

The current publishable entry point is the Rust crate
`talos3d-capability-api`. Capability crates should treat that crate as the
stable SDK boundary and avoid reaching into `talos3d-core` registration
internals directly.

## Design Goal

A capability should be able to register everything it needs without editing core
platform code.

That includes:

- authored entities and factories
- assembly and relation vocabulary descriptors
- commands and schemas
- tools and shortcuts
- panels and toolbar contributions
- import/export handlers
- icons and metadata
- AI-visible inspection semantics

The public boundary must be usable by:

- in-tree reference capabilities
- private first-party capability repos
- community-maintained capability crates
- commercial closed-source add-ons

## Registration Categories

### Capability registration

- capability id
- display name
- version
- capability API version
- dependencies
- optional dependencies
- declared conflicts
- maturity and distribution metadata
- licensing / repository metadata
- descriptive metadata

Capabilities register through `CapabilityDescriptor`.

Important current fields:

- `id`
- `name`
- `version`
- `api_version`
- `dependencies`
- `optional_dependencies`
- `conflicts`
- `maturity`
- `distribution`
- `license`
- `repository`

Talos3D validates these registrations at startup. A capability package should
fail fast if:

- it targets the wrong capability API version
- it depends on a missing capability
- it conflicts with another registered capability
- a setup references a capability that was not registered

### Authored model registration

- entity factories
- assembly and relation type registration
- serializers / persistence boundaries
- derived evaluation and mesh pipelines
- geometry semantics contributors

### Command registration

- command id
- parameter schema
- handler binding
- UI metadata
- AI visibility

### UI registration

- toolbar contributions
- panel contributions
- icon registrations

### Format registration

- importers
- exporters
- validation helpers

## SDK Surface

The intended import path for capability crates is:

```rust
use talos3d_capability_api::prelude::*;
```

The SDK currently re-exports the stable extension-facing pieces needed by
capability crates:

- capability and setup descriptors
- registry extension traits
- command descriptors and command registration
- toolbar registration
- assembly and relation vocabulary descriptors
- defaults contributors
- terrain provider registration
- active tool and tool activation helpers
- document properties and icon helpers

This is a source-level Rust capability API. Dynamic loading is not required for
the API to be publishable.

## Setup Packaging

Setups are workflow bundles over capabilities, not privileged application
tiers.

`SetupDescriptor` currently carries:

- `id`
- `name`
- `version`
- `capability_ids`
- `optional_capability_ids`
- `description`

This lets a public or private setup declare:

- the required capability pack
- optional companion capability packs
- the user-facing workflow bundle name

The important architectural rule is that setups compose capabilities; they do
not bypass capability boundaries.

## Compatibility Goal

The API should support capability modules that are:

- in-tree
- out-of-tree
- open-source
- proprietary
- distributed as paid add-ons

The architecture docs should never imply that only built-in domain packages get
special access.

## Public Stability Contract

The current intended stability contract is:

- capability crates target `CAPABILITY_API_VERSION`
- capability entrypoints depend on `talos3d-capability-api`
- authored geometry/domain logic may still depend on `talos3d-core`
- registration and packaging metadata should not require unpublished internals

The next extraction step after this is out-of-tree proof: a private capability
repo building against the SDK crate only.

## AI Expectations

Capabilities should contribute enough metadata that AI can:

- discover commands
- inspect authored state
- understand capability-specific semantics
- invoke operations through MCP or an equivalent structured API

## Architectural Summary

The capability plugin API is the public contract that turns Talos3D from a
single application into an extensible platform. The SDK crate, manifest
metadata, and startup validation are the first practical pieces of that public
contract.
