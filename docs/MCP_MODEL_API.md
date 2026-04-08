# MCP Model API

## Purpose

Talos3D exposes a Model Context Protocol (MCP) surface so external AI agents
and automation clients can inspect and manipulate the authored model through a
structured interface.

This is a public part of the platform rather than a private editor hook. The
same command substrate backs keyboard shortcuts, toolbar actions, menus, the
command palette, and MCP operations.

## What It Exposes

The MCP surface is designed around the authored model rather than render
meshes. Clients can:

- inspect authored entities and normalized property data
- inspect model-level summaries and document properties
- discover registered assembly and relation vocabulary
- inspect authored semantic assemblies and typed relations
- query registered commands, toolbars, layers, groups, and selection state
- invoke write operations through the same command pipeline the UI uses
- import files and capture viewport screenshots

Recent additions expose first steps toward higher-order semantic structure:
capabilities can register assembly and relation vocabulary, and MCP clients can
inspect or create authored assemblies and relations through structured tools.

## Running Talos3D With MCP Enabled

Start the app with the `model-api` feature:

```bash
cargo run --features model-api
```

To run multiple MCP-enabled instances without collisions, provide a unique
instance id and port:

```bash
cargo run --features model-api -- --instance-id codex --model-api-port 24842
cargo run --features model-api -- --instance-id claude --model-api-port 24901
```

If no port is provided, Talos3D prefers `24842` and automatically falls back to
an available port when that default is already in use.

When enabled, Talos3D exposes MCP endpoints in two forms:

- stdio transport for local process-based integrations
- streamable HTTP at `http://127.0.0.1:<port>/mcp`

The repository includes a local `.mcp.json` example that points to the HTTP
endpoint.

## Instance Discovery

Each MCP-enabled instance writes a discovery manifest to:

- `/tmp/talos3d-instances/<instance-id>.json`

The manifest includes:

- `instance_id`
- `pid`
- `http_port`
- `http_url`
- `registry_path`

After connecting, clients should call `get_instance_info` to confirm they are
attached to the intended instance.

## Tool Surface

Current tool categories include:

### Model inspection

- `get_instance_info`
- `list_entities`
- `get_entity`
- `get_entity_details`
- `model_summary`

### Semantic vocabulary and structure

- `list_vocabulary`
- `list_assemblies`
- `get_assembly`
- `list_assembly_members`
- `query_relations`

### Editing and authored changes

- `create_entity`
- `create_assembly`
- `delete_entities`
- `transform`
- `set_property`
- `set_entity_property`
- `split_box_face`

### Document and UI state

- `get_document_properties`
- `set_document_properties`
- `list_toolbars`
- `set_toolbar_layout`
- `list_commands`
- `invoke_command`
- `get_selection`
- `set_selection`

### Groups and layers

- `get_editing_context`
- `enter_group`
- `exit_group`
- `list_group_members`
- `list_layers`
- `set_layer_visibility`
- `set_layer_locked`
- `assign_layer`
- `create_layer`

### Import and capture

- `list_importers`
- `import_file`
- `take_screenshot`

`model_summary` now also reports `assembly_counts` and `relation_counts` in
addition to entity counts and capability-defined metrics.

Semantic assemblies are authored records, distinct from editing groups. They
are intended as a first step toward domain structures such as rooms, storeys,
houses, and future domain-specific assemblies.

## Design Contract

The MCP surface follows these rules:

- authored data stays primary
- writes go through commands and history
- entity semantics should be legible without reverse-engineering triangle data
- capability-specific commands and metadata should be discoverable

This is what allows Talos3D to be AI-first without relying on private editor
hooks.

## For Capability Authors

Capability packs should contribute enough metadata that MCP clients can:

- discover commands
- inspect authored state
- understand capability-specific semantics
- invoke operations through the public command surface

If a capability only works through UI-specific logic and cannot be understood
through MCP, it is not aligned with the platform direction.
