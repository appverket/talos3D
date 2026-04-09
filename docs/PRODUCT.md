# Product

## Positioning

Talos3D is a 3D platform for authored design, semantic geometry, and
extensible domain workflows.

AI and automation are treated as first-class clients of the authored model
rather than as external systems forced to infer intent from the render scene.

It should be understood as:

- a shared core platform
- a command and model API substrate
- a capability ecosystem surface
- a host for both open and closed domain extensions

The platform is not limited to architecture, but architecture is the reference
extension currently present in the repository.

## Product Thesis

Most 3D tools make AI reverse-engineer geometry from meshes or force domain
extensions to live inside privileged product silos.

Talos3D takes the opposite approach:

- authored semantics remain primary
- geometry is represented through authored definitions and evaluated bodies
- commands are the common execution substrate
- capabilities are the main extension and packaging unit
- AI inspects the same platform concepts that humans edit

Recent work adds a first semantic assembly layer to that approach. The model
can now carry authored assemblies and typed semantic relations so higher-order
structures such as rooms, storeys, or houses can be represented directly
instead of only being inferred from geometry.

## Core Product Promises

### 1. AI-friendly authored geometry

The platform exposes authored entities, definition relationships, geometry
semantics, and evaluated body facts. AI should be able to answer questions like:

- what is this object?
- is it one closed solid or a composition?
- which features define it?
- what is its volume or connectedness?
- which edits are semantically valid?

That direction now extends beyond individual solids. The current platform also
has first steps toward authored semantic assemblies and relations for
higher-order domain structure.

Recent modeling work also treats fillet and chamfer as authored features rather
than destructive mesh edits. That matters for AI because the source entity,
feature parameters, and resulting derived mesh stay inspectable through the
same public model surface.

### 2. MCP as a public integration surface

Talos3D exposes a structured MCP model API so external AI systems and
automation clients can inspect the model, discover commands, and invoke edits
through the same public command substrate the UI uses.

That MCP surface now also exposes vocabulary discovery and assembly/relation
operations, which makes domain-level structure more legible to external tools.

It also exposes viewport appearance and scene-lighting controls so agents can
reason about visual presentation through structured state instead of hidden
editor startup logic.

### 3. Pluggability and extensibility

Talos3D is intentionally designed so domain functionality can be:

- community provided
- first-party maintained
- privately distributed
- monetized as premium capability packs

The architectural package should be read as a public example of that extension
model, not as a permanently privileged built-in layer.

### 4. One command surface

Toolbar buttons, shortcuts, menus, the command palette, automation, tests, and
the MCP model API all converge on the same command substrate.

The embedded assistant chat lane follows the same rule: it is a client of MCP,
not a privileged internal bypass.

### 5. Multiple modeling paradigms can coexist

The platform should support:

- simple primitives
- profile-based solids and authored features
- explicit mesh-backed leaves where justified
- future definition DAGs such as parameterized MultiSurf-style geometry

The platform should not lock itself to one representation style.

### 6. Hosted and partner-extensible content catalogs

Talos3D must support reusable content catalogs for both authored Definitions
and material/texture assets.

This is an explicit product requirement, not an optional deployment detail.

The requirement is:

- bundled repository content may provide bootstrap libraries, but it must not be
  the only publication path
- users, firms, and external partners must be able to publish new definition
  libraries and texture/material catalogs without requiring a desktop reinstall
  or browser client redeploy
- local deployments may satisfy this through a local API service backed by
  directory files
- hosted deployments must be able to satisfy the same contract through backend
  storage such as Firebase Firestore plus Storage, Supabase Postgres plus
  Storage, or an equivalent service pair
- the authored model, UI, and MCP surface must treat these catalogs as
  first-class sources, not as ad hoc import side channels

Example business cases include manufacturers publishing product families such as
windows, doors, finishes, or fixtures directly into the ecosystem so those
items can be selected during design and later flow into schedules, quantities,
or bills of materials.

## Embedded Assistant Delivery Model

Talos3D can host an in-app assistant, but that assistant must remain compatible
with both local and hosted deployments.

The delivery model is:

- in-app chat is a first-class UI surface
- model inspection and edits still flow through MCP
- browser/SaaS deployments should prefer a managed relay so provider access and
  account policy stay server-side
- direct vendor API keys may exist as a local fallback, but they are not the
  preferred browser deployment model

## Public Packaging Story

Talos3D should be easy to explain publicly:

- **Open platform**: the shared runtime, registries, command substrate, AI/model
  API, and reference capabilities
- **Reference extensions**: architecture, terrain, and other first-party domain
  packs that demonstrate the public extension model
- **Third-party ecosystem**: community packages, marketplace packages,
  enterprise-private capabilities, and closed-source premium add-ons

This packaging story is only credible if the technical extension boundaries stay
real in the codebase.

It is also only credible if the governance story is honest: Talos3D is not
positioned as a default support business. The platform is open, the extension
surface is real, and commercial options may exist around it, but maintainer
time remains discretionary.

## Business Posture

The current business posture is deliberately lightweight:

- keep the platform open and reusable
- retain trademark control over the Talos3D name and branding
- allow future monetization through optional services, add-ons, packaging, or
  commercial offerings
- avoid any default obligation that converts retirement into a support queue

This means the repository should be attractive to users and contributors, while
remaining safe for the maintainer to pause or stop at any time.

## Documentation Shape

The website, repository landing page, onboarding material, and architecture docs
should all be generated from Markdown in the repository. That keeps the public
story and the engineering story close to the same source.
