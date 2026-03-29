# Talos3D Public Agent Notes

Talos3D is intentionally AI-friendly. This file helps AI-assisted contributors
and automation clients work against the same public architecture that human
contributors use.

## Read First

1. [README.md](./README.md)
2. [docs/MCP_MODEL_API.md](./docs/MCP_MODEL_API.md)
3. [docs/DEVELOPER_ONBOARDING.md](./docs/DEVELOPER_ONBOARDING.md)
4. [docs/PLATFORM_ARCHITECTURE.md](./docs/PLATFORM_ARCHITECTURE.md)
5. [docs/EXTENSION_ARCHITECTURE.md](./docs/EXTENSION_ARCHITECTURE.md)
6. [docs/CAPABILITY_PLUGIN_API.md](./docs/CAPABILITY_PLUGIN_API.md)

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
- Do not treat the `private/` directory as public project content.

## MCP Guidance

If you are integrating an external agent:

- enable the model API with `cargo run --features model-api`
- prefer structured MCP tools over scraping UI state
- inspect first, then invoke commands or property updates
- expect the MCP surface to reflect authored entities and command metadata

See [docs/MCP_MODEL_API.md](./docs/MCP_MODEL_API.md) for the transport and
tool surface.
