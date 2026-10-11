# Platform boundaries

Core mechanisms must be reusable by unrelated editions without changing their
meaning. Domain capabilities supply the vocabulary and knowledge consumed by
those mechanisms.

| Core owns | An edition or domain pack owns |
| --- | --- |
| Authored entity and factory contracts, geometry and history | Domain entities and their geometry interpretation |
| Definitions, occurrences, typed units and component-family schemas | Family assets, product templates and parameter defaults |
| Assembly/relation contracts and dependency traversal | Domain assembly kinds, member roles and relationship semantics |
| Captured edit plans, preview/commit orchestration | Domain placement, hosting and reseating plans |
| Refinement states, obligations, findings and provenance | Domain obligations, engineering constraints and validators |
| Recipe, catalog, prior and corpus registries | Executable recipes, catalogs, evidence and priors |
| Guidance-card registry and MCP transport/discovery | Domain guidance cards, skills and authoring policy |
| Terrain sampling and generic surface/solid operations | Building-specific support and foundation interpretation |
| Public Core app and its platform tests | Specialized app composition and domain integration tests |

Architecture supplies walls, roofs, openings, rooms, storeys, construction
systems, building products and IFC mappings. Jurisdiction and manufacturer
knowledge belong in narrower packs layered on that domain. Naval, mechanical
and community editions can contribute their own entities and content using the
same public contracts. Core must build without any of these repositories.

## Where to start in Core

`crates/talos3d-core/src` is arranged around these boundaries:

- `authored_entity.rs`: immutable snapshots, authored properties and restore
  contracts. Implement `dependency_order` for domain snapshots that must restore
  hosts before dependents; Core does not dispatch on domain type names.
- `capability_registry.rs`: feature registration and factory/vocabulary contracts.
- `plugins/`: editor capabilities, including `commands`, `history`,
  `authored_edit_plan`, `modeling`, `persistence`, `validation` and `model_api`.
- `relational/`: generic parameters, expressions and dependency execution.
- `curation/`: versioned knowledge assets, source/provenance and draft workflows.
- `semantics/`: capability-owned concept graphs and semantic resolution.
- `importers/` and `storage/`: drawing ingestion and artifact storage contracts.

The capability facade is `crates/talos3d-capability-api`. It exposes shared edit
plans, hosting contracts and `component_families` to extension authors. It is a
source-level API that evolves with Core, rather than an independently stable ABI.

## Shared component families

`plugins::component_family` parses versioned family assets, merges parameter
specifications, checks types and bounds, maps parametric drivers and instantiates
Definitions. Any edition can use it unchanged. Packs own their JSON family
files and Definition templates. Architecture retains its window-family assets
and command adapters while importing these shared operations.

## Capability placement and reservation policies

Core's `HostedGeometryPolicies` performs shared aperture/host geometry checks
using capability-supplied margins, tolerances, allowed flush edges and parameter
names. Host snapshots provide their authored axis and position mapping.
Architecture supplies the wall-opening contract and its dimensional rules;
Core does not inspect serialized Wall data or choose building thresholds.
The Definition browser consumes capability-owned `HostedSelectionContext` through
snapshot hooks and shares face anchors/orientation with API placement. It uses
the same contributed relation and parameter policy.

`SettingOutPolicies` selects participating classes and parameter aliases for
shared envelope reservation. An explicit `SettingOutContract` works for any
domain. Architecture registers its wall class and construction-system aliases.
Likewise, `SurfacePlacementVocabulary` lets the terrain capability place generic
support systems while Architecture contributes house/building/foundation roles.
Legacy command ids and saved Architecture vocabulary remain compatible.

Orthographic profile analysis also remains generic: default peak filtering scales
with the object. Architecture contributes its former building-scale prominence
and neighbourhood distances through `ElevationAnalysisPolicy`.

## Capability material defaults

`BuiltinMaterialDefaults` retains capability-supplied appearance presets across
new/open document operations. Core owns the material and texture machinery;
Architecture owns the manufacturer-specific cedar preset and its embedded
textures. Its existing material id remains unchanged. Generic glass, polymer
and metal appearance presets remain reusable public materials.

## Shared component-structure advice

`plugins::component_structure_quality` detects repeated same-role singletons
and excessive non-placement override variance. It uses generic refinement
relations, roles, Definitions and occurrences. `ModelingPlugin` installs this
advisory validator for any edition. Architecture retains a compatibility import
and uses the same plugin in headless compositions; it does not own a second
implementation. Findings remain advisory and keep their existing identifiers.

## Capability-owned guidance

Packs insert `GuidanceCardInfo` into `GuidanceCardRegistry` during plugin
composition. MCP lists and resolves those contributions alongside platform
cards. Core's baseline has no house, roof or terrain-specific guidance cards;
they appear when the owning capabilities are loaded. Keep every advertised
`next_card_id` resolvable in the actual app composition.

## Boundary checks

`python3 scripts/check_repository.py` checks workspace locality, package metadata,
local Markdown links and unwanted domain package dependencies. CI compiles and
tests the public app with and without MCP. This guards packaging; it is not proof
that every implementation is domain-neutral.

For a cross-repository extraction, preserve public identifiers and serialized
contracts where possible. Test both the shared mechanism with unrelated domain
fixtures and the existing domain consumer. Changes to knowledge discovery also
require a fresh MCP session through the actual app, all advertised guidance
cards and representative authoring/render verification. Domain words in tests
or examples alone are not evidence of a domain dependency; production dispatch
or knowledge that requires that domain is.
