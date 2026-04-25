# Scenario Files

## Purpose

A scenario file is a portable, data-only authoring plan for Talos3D.

It describes what should exist in a model, which capabilities and recipes
should be used, which refinement states are required, which validations must
pass, and which obligations are intentionally deferred. It is not product code,
not a hot-loaded binary, and not a replacement for capability registration.

Scenarios make AI-first workflows repeatable. An assistant can produce,
inspect, revise, and execute a scenario through public model APIs instead of
hiding domain expertise inside a prompt transcript or a compiled test fixture.

Scenario files are not intended to be the normal user interface. Most users
should never see JSON, Rust, command-line runners, or schema terminology. They
should describe goals in ordinary language, fill guided forms, import reference
material, choose between visual alternatives, and read plain-language
validation explanations. The scenario is the durable internal plan that the
assistant, product UI, automation, or hosted service can inspect and execute.

## Product Concept

Scenario files are a bona fide Talos3D concept.

They sit between user intent and the authored model:

```text
prompt / source brief / imported context
  -> scenario file
  -> generic scenario runner or MCP authoring loop
  -> authored model
  -> validation, exports, and refinement reports
```

The generic scenario mechanism belongs in `talos3d-core` because every domain
can use it. Domain-specific scenario content belongs in the relevant
capability, project, or private workspace. For example, the concept of a
scenario is generic; the knowledge that a Swedish villa should have a
slab-on-grade foundation, light-frame exterior walls, and a gable roof belongs
to an architecture capability or architecture project artifact.

The product may expose scenarios under different user-facing names such as
brief, plan, template, checklist, or showcase. The storage and interchange
format can be JSON, but that is an implementation detail.

## What A Scenario May Contain

A scenario may declare:

- required capabilities or vocabulary ids
- entities to create or resolve
- semantic relations between entities
- recipe selections and parameters
- target refinement states
- validation expectations
- allowed deferred obligations
- required exports or named views
- references to external source material, catalog entries, or jurisdiction
  packs
- temporary compatibility claims that expose known product gaps explicitly

A scenario should not contain compiled logic. If a scenario needs behavior that
cannot be expressed through registered commands, recipes, validators, or
relations, that is a capability gap.

## Where Scenarios Come From

Scenarios may be:

- shipped with a capability or product as examples, templates, regression
  fixtures, or showcases
- generated from a prompt by an assistant
- generated from external sources such as a brief, spreadsheet, survey,
  regulatory source, drawing import, catalog, or customer standard
- saved from an interactive user session as a repeatable plan
- maintained privately by a team as project or domain know-how

Shipping a scenario with Talos3D is optional. The important contract is that a
scenario is data, and the runner/interpreter is generic.

## Runtime Workflow

A scenario-aware workflow should:

1. inspect the loaded capabilities and available vocabulary
2. check whether the scenario references supported classes, recipes, relations,
   validators, and exporters
3. create or resolve the declared entities
4. apply the declared relations
5. promote entities through the requested refinement states
6. run validation after meaningful steps
7. report unresolved obligations and expected deferrals
8. produce requested summaries, drawings, views, or export artifacts
9. turn missing vocabulary or recipe knowledge into explicit drafts or gaps

This workflow may be executed by the in-app assistant, an MCP client, a CI
test, a desktop command, or a hosted service. It should not depend on a hidden
editor hook.

For end users, the same workflow should appear as a guided design conversation:
Talos3D asks for missing intent, proposes a plan, previews the model, explains
validation gaps, and offers next refinements. The scenario file is the
machine-readable memory of that process, not something the user must author.

## Relationship To Capabilities

Capabilities provide the executable vocabulary:

- element classes
- recipe families
- relation types
- validators
- commands
- catalogs
- priors
- exporters

Scenarios select and combine that vocabulary for a concrete goal.

If a scenario references an unknown architecture recipe, the correct outcome is
not to compile a new Rust binary. The system should either load a registered
capability that provides the recipe, use a session-scoped draft where the
product supports draft consultation, or report a missing-capability gap.

## Relationship To Recipes

A recipe is reusable domain capability knowledge. It answers a local
refinement question:

> Given one entity of this class, these parameters, and the current authored
> context, how should Talos3D promote it to a more detailed refinement state?

A scenario is a concrete authoring plan. It answers a project or workflow
question:

> For this brief, which entities should exist, how are they related, which
> recipes should be selected, in what order should refinement happen, and what
> validations or exports prove that the result is good enough?

Use this boundary:

- recipe: reusable, capability-owned, applicable across many projects
- scenario: concrete, project-owned or template-owned, applicable to one goal
  or family of goals
- recipe parameter: local input to one refinement operation
- scenario variable: higher-level design value that may feed several recipe
  parameters and validation expectations
- recipe validation: checks obligations created by one refinement path
- scenario validation: checks whether the whole requested workflow is complete
  enough for the declared showcase, template, or project milestone

Example: `light_frame_exterior_wall` is a recipe. It knows how to refine a wall
assembly into studs, plates, sheathing, insulation, membranes, cladding, and
finishes. A Swedish villa shell scenario may use that recipe four times with
different wall ids and lengths, relate those walls to a slab, relate a gable
roof to the bearing walls, and require all shell entities to validate.

If knowledge appears in many scenarios, promote it into a recipe, pattern,
prior, catalog, or validation rule. If knowledge is specific to one brief or
showcase, keep it in the scenario.

## Lifecycle

Scenario maturity should be explicit:

- `draft`: useful planning artifact, not expected to run cleanly
- `probe`: used to discover product or capability gaps
- `regression`: expected to run and validate repeatedly
- `template`: intended for users or agents to copy and parameterize
- `showcase`: curated demonstration of a product workflow

The authored model remains the source of truth after execution. A scenario is a
recipe for constructing or refining model state, not the model itself.
