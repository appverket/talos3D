# ADR-026: Material Architecture Boundary

## Status

Accepted

## Date

2026-04-20

## Context

Talos3D already has a rendering-material system in
`plugins::materials::MaterialDef`, but recent design work clarified that
"materials" actually spans three distinct concerns:

1. shared texture/media infrastructure
2. rendering appearance in the viewport and export pipeline
3. construction-material semantics for architecture/BIM-like workflows

At the same time, Talos3D now has a curation substrate for evidence-governed
knowledge kinds. That substrate is appropriate for construction material
specifications, but not for every media asset the renderer touches.

External render libraries also matter operationally: Appverket cannot author a
large first-party starter material library in-house, so Talos3D needs a
bootstrapping pipeline for high-quality external PBR sources without confusing
import/interchange formats with the canonical authoring model.

## Decision

Talos3D will use a three-layer material architecture plus one authored binding
layer:

1. `TextureAsset`
2. `MaterialDef`
3. `MaterialSpec`
4. `MaterialAssignment`

### `TextureAsset`

- first-class shared media infrastructure
- content-addressed and de-duplicated
- persists with projects and packs
- may carry provenance metadata when useful
- is **not** a curated knowledge kind and does not carry `CurationMeta`

### `MaterialDef`

- remains the canonical internal rendering-material model
- is the source of truth for viewport appearance, texture binding, and render
  export mapping
- stays Rust-native and serde-serializable
- may optionally point at a `MaterialSpec` through `spec_ref`

### `MaterialSpec`

- is a distinct curated kind on the curation substrate
- carries architectural/construction-material identity, standards references,
  and performance semantics
- may carry an optional advisory `default_rendering_hint`

### `MaterialAssignment`

- is authored object state, not geometry `ParameterSchema`
- binds authored definitions/occurrences to material semantics and preferred
  rendering
- ships in the MVP with `Single` and `LayerSet`
- defers `ConstituentSet` until a concrete workflow requires it

### Persistence and interchange

- canonical authoring persistence remains text/serde-friendly project and pack
  data
- Bincode and KTX2/Basis are confined to compiled/cache/runtime layers
- glTF remains an interchange boundary, not the canonical internal schema

### UX boundary

- end-user material browsing, editing, assignment, and import belong in the
  Talos3D app
- operator curation is an entitlement-gated mode of the same app
- headless ingestion/build/publication jobs may exist separately, but they are
  not a separate human-facing product

## Consequences

### Positive

- Talos3D no longer has to overload one "material" type with incompatible
  rendering and construction semantics
- texture de-duplication and derivative generation gain a stable first-class
  anchor
- `MaterialSpec` can ride the curation substrate without dragging the full
  governance model into the render-material browser
- the system can bootstrap render libraries from external sources while keeping
  construction semantics explicitly governed

### Tradeoffs

- the material system becomes more explicit and slightly more layered
- persistence and MCP boundaries need compatibility shims during migration
- some rendering workflows will carry soft links to construction semantics that
  are optional rather than enforced

## Follow-On Guidance

- keep KTX2/Basis invisible in normal authoring UX; expose it only in
  operator/build diagnostics
- treat source-tier policy as contractual, not format-driven
- prefer typed MCP operations over generic JSON Patch for all material editing
- keep `TextureAsset` first-class without over-curating it
