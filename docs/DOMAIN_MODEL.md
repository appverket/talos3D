# Domain Model

## Purpose

This document describes the authored model Talos3D treats as the source of
truth.

## Core Rule

Talos3D does not treat the render scene as the model.

The model consists of:

- authored entities
- authored semantic assemblies
- authored typed semantic relations
- authored definition relationships
- semantic parameters
- metadata
- constraints and invariants

Meshes, previews, and hit-test helpers are downstream artifacts.

## Current Authored Geometry Vocabulary

### Simple primitives

- `BoxPrimitive`
- `CylinderPrimitive`
- `PlanePrimitive`
- `Polyline`

### Profile-based geometry

- `ProfileExtrusion`
- `ProfileSweep`
- `ProfileRevolve`

### Authored features

- `FaceProfileFeature`

### Domain entities

- architectural walls
- architectural openings
- BIM metadata-bearing authored elements

### Higher-order semantic structure

- semantic assemblies such as `house`, `storey`, `room`, and future
  domain-specific aggregates
- typed semantic relations such as `hosted_on`, `bounds`, `adjacent_to`, and
  `supports`

These are authored model records, not mesh annotations. They are also distinct
from editing groups: an assembly can express multi-membership and semantic
participation without becoming an editing context or implying cascade deletion
of its members.

## Definition Graph Direction

ADR-023 establishes the direction that authored geometry is represented as
definition nodes with explicit dependencies.

Talos3D is not limited to one modeling paradigm. The authored model should stay
compatible with:

- primitive-centric entities
- profile-based solids and features
- explicit mesh-backed leaves where justified
- future parameterized geometry DAGs such as MultiSurf-style definitions

## Evaluated Bodies

Some facts are not authored parameters. They are evaluated body facts derived
from authored geometry, such as:

- connected component count
- closed/manifold status
- volume
- bounding box

These facts are now exposed for supported solid roots through semantic geometry
summaries instead of forcing AI to infer them from meshes.

## Authored Solid Envelope

For supported solids, Talos3D now exposes an AI-facing semantic wrapper:

- role in the definition graph
- topology intent
- definition inputs
- attached features
- invariants
- evaluated body summary

This is the **Authored Solid Envelope (ASE)**.

## Semantic Affordance Surface

Editing affordances should derive from authored semantics. If moving a face
would violate the meaning of a solid, the platform should block that edit
because of authored invariants, not because of a viewport-specific patch.

This is the **Semantic Affordance Surface (SAS)** direction.

## Extension Implications

Domain packages extend the model by contributing authored entities, semantics,
rules, and evaluation behavior through public capability registration surfaces.

Architecture is the reference example of that pattern. It should remain
possible for another domain package to contribute an entirely different
definition graph and still participate in the same platform.
