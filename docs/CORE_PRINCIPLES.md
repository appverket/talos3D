# Core Principles

## 1. The Authored Model Is The Source Of Truth

User intent lives in authored entities, definition nodes, relationships,
metadata, and constraints. Meshes, previews, highlights, and caches are derived
artifacts.

## 2. AI Operability Is A Primary Design Driver

AI is not a sidecar integration. The platform should expose enough authored
semantics that an AI can inspect, reason about, and modify a design without
reverse-engineering renderer internals.

## 3. Capabilities Are The Primary Extension Unit

Features should be delivered as capabilities with explicit registration,
dependencies, and contracts. Setups are curated bundles of capabilities, not the
main architectural boundary.

## 4. Extensibility Must Support Open And Closed Packs

The platform should support community capabilities, first-party reference
extensions, and proprietary add-ons without requiring forks of the core
platform.

## 5. Definition Graphs Must Stay Graph-Friendly

Do not hard-code tree-only assumptions into geometry semantics or editing
contracts. The current architecture must remain compatible with future
definition DAGs and parameterized geometry paradigms such as MultiSurf.

## 6. Commands Own Committed Edits

Interactive tools collect intent. Commands commit authored changes. History owns
undo/redo. This keeps human interaction, automation, and AI invocation aligned.

## 7. Interaction Affordances Come From Semantics

Editing affordances should be derived from authored meaning, not guessed from
rendered topology. The current work on Semantic Affordance Surfaces follows this
rule.

## 8. Evaluated Facts Are Distinct From Authored Intent

Volume, connectedness, manifold status, and bounding boxes are evaluated body
facts. They should be exposed clearly, but they should not be confused with the
authored definition itself.

## 9. Architecture Is A Reference Extension, Not A Special Exception

The architectural functionality in this repository demonstrates how a domain
package composes on top of the platform. It should be understandable as a public
example of the extension model.

## 10. Public Documentation Must Track The Code

The public website, repository README, onboarding material, and architecture
docs should all stay consistent with the current terminology and implementation
direction.
