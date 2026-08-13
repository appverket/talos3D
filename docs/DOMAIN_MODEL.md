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

### Linked-model relationships

A linked model is an explicit relationship between an external Talos3D
document and one instance root in the current document. It is not an import
that becomes ordinary copied geometry. The relationship records:

- the external source path and source root element id
- the content hash from the last successful load
- the instance root's local-to-scene frame
- stable source-to-scene element-id mappings for that instance
- the mapped members immediately owned by the source root

Multiple instances may reference the same source while retaining disjoint scene
ids and independent instance transforms. Linked members remain source-owned;
host-document capabilities should consume the relationship and instance root
instead of editing mapped members as if they were local content.

Placement capabilities consume `LinkedModelPlacementSubject`, which resolves
the instance as one bounded movable subject and validates its identity map
before producing an authored edit plan. The subject separates source-owned
scene ids from host-dependent ids. A host may attach derived placement content
(for example, a surface-conforming support) to the instance root without that
content becoming part of the external source. Refresh replaces mapped source
members while retaining those host-owned dependents.

Dependencies that need to survive refresh store a source-side
`LinkedModelDependencyAnchor`, not a transient mapped scene id. Resolving the
anchor reports `current`, `rebased` after a source revision, or a structured
`stale_mapping` refusal when its source identity disappeared. Translation and
rotation use one pure before/after placement plan for preview, command commit,
and history; a stale subject produces no mutation.

This contract is domain-neutral. Core does not infer that a linked model is a
building, vehicle, site, or any other domain object. Domain capabilities may
inspect the mapped authored content and contribute their own typed affordances
on top of the same relationship.

## Definition Graph Direction

ADR-023 establishes the direction that authored geometry is represented as
definition nodes with explicit dependencies.

Talos3D is not limited to one modeling paradigm. The authored model should stay
compatible with:

- primitive-centric entities
- profile-based solids and features
- explicit mesh-backed leaves where justified
- future parameterized geometry DAGs such as MultiSurf-style definitions

### Canonical Definition body

`Definition` and `Occurrence` are the only public lifecycle for reusable
components. Every Definition persists one versioned `body`; that body is the
inspectable authority for evaluator implementations, representation
declarations, unit-aware scalar expressions, constraints, derived parameters,
and compound child slots. Specialized evaluators are implementation choices
inside the body—not separately identifiable parametric components.

Body schema version 1 uses the relational `ScalarExpr`/`Predicate` substrate for
numeric evaluation and a bounded `BodyExpr` value layer for booleans, strings,
references, equality, conjunction, and conditionals. Dependencies are extracted
from those expressions and derived-parameter cycles are rejected before
publication.

Legacy Definition JSON with top-level `evaluators`, `representations`, or
`compound` fields and legacy `ExprNode` expressions remains readable. Loading
migrates it deterministically into the canonical body; saving emits only
`body`. A construct that cannot be translated without guessing is retained as
an inspectable `UnsupportedLegacyExpression` finding and blocks execution or
publication. A document may not contain both body forms, and unknown future
body schema versions are rejected.

### Definition parameter units

Numeric Definition parameters carry exchange-safe typed unit metadata. New
content serializes both the physical dimension and canonical unit, for example:

```json
{"dimension":"length","unit":"m"}
```

The shared vocabulary covers length, angle, area, volume, ratio, count, and
scalar dimensions and is also used by relational quantities. Legacy string
spellings such as `"metres"` remain readable and normalize to the typed form.
An unrecognized legacy spelling is retained explicitly as `unknown_legacy`; it
is never guessed, and it blocks validation/publication when the parameter can
affect geometry.

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
