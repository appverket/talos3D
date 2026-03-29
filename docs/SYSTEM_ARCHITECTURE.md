# System Architecture

## Overview

Talos3D is a Bevy-based ECS application assembled from plugins, registries,
authored entity factories, and capability bundles.

The current system should be understood in six layers:

1. platform runtime
2. core platform services
3. authored model and definition graph
4. evaluated body layer
5. render and interaction artifacts
6. capability and workbench composition

## 1. Platform Runtime

- Bevy app and schedules
- native/window integration
- WebGPU-capable renderer boundary

## 2. Core Platform Services

- command/history substrate
- selection and transform state
- viewport and camera systems
- status and UI chrome
- capability, command, icon, toolbar, and format registries
- model API / MCP surface

These services live primarily in `talos3d-core`.

## 3. Authored Model And Definition Graph

This is the source of truth.

Current authored geometry includes:

- primitives such as boxes, cylinders, planes, and polylines
- profile-based solids such as profile extrusions
- face profile features
- domain entities such as walls and openings

The direction from ADR-023 is that authored geometry should remain legible as a
definition graph, not collapse into mesh-led state.

## 4. Evaluated Body Layer

Between authored definitions and meshes sits the evaluated body layer:

```text
authored definition
  -> NeedsEvaluation
  -> evaluated body artifacts
  -> NeedsMesh
  -> render mesh
```

This is where the platform derives facts such as:

- connected component count
- manifold/closed-solid status
- volume
- semantic support for generated face references

## 5. Render And Interaction Artifacts

Derived artifacts include:

- render meshes
- previews
- highlights
- gizmos
- hit-testing support geometry

These are replaceable and must not become the authored truth.

## 6. Capability And Workbench Composition

The in-repo system currently demonstrates:

- `talos3d-core` as the shared platform plus modeling workbench
- `talos3d-architectural` as a reference domain extension
- `talos3d-terrain` as another reference extension path

This composition model is intentionally the same one future out-of-tree
capability crates should use.

## Geometry Semantics

Recent work introduced AI-facing geometry semantics for supported solid roots.

That surface currently exposes:

- role in the definition graph
- topology intent
- definition inputs
- attached features
- invariants
- evaluated body summaries

The current named concepts are:

- **Authored Solid Envelope (ASE)**: the semantic wrapper around a realized
  solid
- **Semantic Affordance Surface (SAS)**: the rule that editing affordances come
  from authored semantics rather than rendered topology

## DAG Compatibility

The system must stay compatible with more than one geometry paradigm. The
current profile/feature workflow is not a reason to bake in tree-only
assumptions. Future parameterized DAGs, including MultiSurf-style definition
graphs, should fit into the same authored -> evaluated -> rendered pipeline.
