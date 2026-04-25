# Talos3D Public Agent Notes

Talos3D is intentionally AI-friendly. This file helps AI-assisted contributors
and automation clients work against the same public architecture that human
contributors use.

## Read First

1. [README.md](./README.md)
2. [docs/ENGINE_FORK_WORKFLOW.md](./docs/ENGINE_FORK_WORKFLOW.md)
3. [docs/MCP_MODEL_API.md](./docs/MCP_MODEL_API.md)
4. [docs/DEVELOPER_ONBOARDING.md](./docs/DEVELOPER_ONBOARDING.md)
5. [docs/PLATFORM_ARCHITECTURE.md](./docs/PLATFORM_ARCHITECTURE.md)
6. [docs/EXTENSION_ARCHITECTURE.md](./docs/EXTENSION_ARCHITECTURE.md)
7. [docs/CAPABILITY_PLUGIN_API.md](./docs/CAPABILITY_PLUGIN_API.md)

## Public Architecture Rules

- The authored model is the source of truth.
- Meshes, previews, highlights, and caches are derived artifacts.
- User-facing edits must flow through commands and history.
- Capabilities are the primary extension unit.
- Domain packs should compose on top of the same public platform contracts.
- AI tooling should prefer the MCP model API over ad hoc internal hooks.

## Contribution Guidance

- Keep changes aligned with the public platform surface.
- Prefer explicit capability boundaries over hidden cross-module coupling.
- Update public docs when architecture or user-facing behavior changes.
- When creating commits in this repository, use only
  `apphjon <dev@appverket.com>` for both author and committer.
- Never commit as any other identity or personal email address.
- Do not treat the `private/` directory as public project content.
- If a task touches Bevy, egui, `bevy_egui`, engine upgrades, or panel sizing
  regressions, read [docs/ENGINE_FORK_WORKFLOW.md](./docs/ENGINE_FORK_WORKFLOW.md)
  before changing dependencies or proposing branch structure.

## MCP Guidance

If you are integrating an external agent:

- enable the model API with `cargo run --features model-api`
- when running multiple Talos3D instances, pass a unique instance id and port:
  `cargo run --features model-api -- --instance-id claude --model-api-port 24901`
- prefer structured MCP tools over scraping UI state
- inspect first, then invoke commands or property updates
- expect the MCP surface to reflect authored entities and command metadata
- call `get_instance_info` after connecting to confirm you are talking to the
  intended app instance
- call `get_authoring_guidance` immediately after connecting, before any
  model-edit tool; the returned `prompt_text` is the Talos3D-owned
  `COMPONENT_STRUCTURE` contract for reusable Definitions, derived variants,
  singletons, and how they compose with progressive refinement

Each MCP-enabled app instance also writes a discovery manifest to
`/tmp/talos3d-instances/<instance-id>.json` by default. Agents can use that
registry to find the correct port before connecting.

See [docs/MCP_MODEL_API.md](./docs/MCP_MODEL_API.md) for the transport and
tool surface.
