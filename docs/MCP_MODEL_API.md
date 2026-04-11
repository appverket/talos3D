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

The modeling layer also exposes authored edge features such as `fillet` and
`chamfer` through the same public APIs. AI clients can create them with
`create_entity`, edit them with `set_property`, or invoke the matching command
entries through `invoke_command`.

Reference annotations are also directly addressable through MCP:
`place_guide_line` creates construction lines from an anchor plus direction,
an anchor plus `through` point, or an angular contract
(`reference_direction`, `angle_degrees`, `plane_normal`). `place_dimension_line`
creates measured annotations from start and end points plus an explicit visible
line placement (`line_point` or scalar `offset`), with optional extension and
unit overrides.

Built-in definition libraries are also loaded at startup and show up through
the same definition-library inspection tools as project-local libraries. Their
reported scope is `Bundled`, which distinguishes shipped catalogs from
document-local or imported external libraries.

Material payloads expose a broader Bevy-backed appearance contract than the
initial MVP: specular tint, transmission, thickness, IOR, attenuation,
clearcoat, anisotropy, unlit/fog flags, and depth bias are all readable and
writeable through the material tools.

Viewport renderer state is also available over MCP through
`get_render_settings` and `set_render_settings`. Those tools expose
tonemapping, exposure, SSAO, bloom, SSR, background color, grid visibility,
paper fill, and drawing overlays so an agent can tune the working view or
compose export-ready drawing views without simulating UI input.

Scene lighting is also available over MCP. Ambient lighting is explicit scene
state, while directional, point, and spot lights are authored entities with
stable element ids and editable properties.

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
- `place_guide_line`
- `place_dimension_line`
- `create_assembly`
- `delete_entities`
- `transform`
- `set_property`
- `set_entity_property`
- `split_box_face`

For fillet/chamfer specifically:

- `create_entity` supports `type: "fillet"` with `source`, `radius`, and
  optional `segments`
- `create_entity` supports `type: "chamfer"` with `source` and `distance`
- `set_property` can update `radius` / `segments` on a fillet and `distance`
  on a chamfer
- `invoke_command` can call `modeling.create_fillet` or
  `modeling.create_chamfer`

### Document and UI state

- `get_document_properties`
- `set_document_properties`
- `list_toolbars`
- `set_toolbar_layout`
- `list_commands`
- `invoke_command`
- `get_selection`
- `set_selection`
- `get_render_settings`
- `set_render_settings`
- `get_lighting_scene`
- `list_lights`
- `create_light`
- `update_light`
- `delete_light`
- `set_ambient_light`
- `restore_default_light_rig`
- `view_list`
- `view_save`
- `view_restore`
- `view_update`
- `view_delete`

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
- `export_drawing`

`model_summary` now also reports `assembly_counts` and `relation_counts` in
addition to entity counts and capability-defined metrics.

## Lighting And Viewport Lookdev

The renderer and lighting surfaces are intentionally agent-facing:

- renderer tuning lives in `get_render_settings` and `set_render_settings`
- named camera states live in `view_list`, `view_save`, `view_restore`,
  `view_update`, and `view_delete`
- ambient scene lighting lives in `get_lighting_scene` and `set_ambient_light`
- authored lights live in `list_lights`, `create_light`, `update_light`, and
  `delete_light`
- the startup/default daylight setup is recoverable through
  `restore_default_light_rig`

Lighting is treated as authored scene state rather than a private startup
fixture. That means:

- agents can inspect and modify the active lighting contract directly
- user-created light rigs persist with the project
- the same concepts work in desktop and browser-hosted deployments

Renderer control also now supports drawing-style viewport composition:

- orthographic views can be saved/restored as named views
- white-paper presentation can be produced through `background_rgb`,
  `grid_enabled`, and `paper_fill_enabled`
- hidden-line-friendly export can be approximated with
  `visible_edge_overlay_enabled`
- drawing exports can be written directly as `png`, `pdf`, or `svg` through
  `export_drawing`; `take_screenshot` now accepts the same output formats when
  a path extension requests them
- the same viewpoint and drawing toggles are also reachable through
  `invoke_command` and discoverable through `list_commands` / `list_toolbars`
  using the `view.*` command family (`view.front`, `view.back`, `view.top`,
  `view.bottom`, `view.left`, `view.right`, `view.isometric`,
  `view.projection_perspective`, `view.projection_orthographic`,
  `view.apply_paper_preset`, `view.toggle_grid`, `view.toggle_outline`, and
  `view.toggle_wireframe`)

Light creation/update currently supports:

- `kind`: `directional`, `point`, or `spot`
- `name`
- `enabled`
- `color`
- `intensity`
- `position`
- `yaw_deg` and `pitch_deg`
- `shadows_enabled`
- `range` and `radius`
- `inner_angle_deg` and `outer_angle_deg` for spot lights

Example spot light creation:

```json
{
  "kind": "spot",
  "name": "Accent Rim",
  "position": [-3.0, 4.5, 3.5],
  "yaw_deg": 45.0,
  "pitch_deg": -32.0,
  "color": [0.72, 0.82, 1.0],
  "intensity": 3600.0,
  "range": 18.0,
  "inner_angle_deg": 12.0,
  "outer_angle_deg": 24.0,
  "shadows_enabled": true
}
```

Semantic assemblies are authored records, distinct from editing groups. They
are intended as a first step toward domain structures such as rooms, storeys,
houses, and future domain-specific assemblies.

## Example: Fillet Via MCP

Create a box, then add a fillet feature that references it:

```json
{
  "type": "fillet",
  "source": 12,
  "radius": 0.15,
  "segments": 4
}
```

Later, adjust the feature with `set_property`:

```json
{
  "element_id": 13,
  "property_name": "radius",
  "value": 0.2
}
```

This keeps the feature AI-readable as authored intent instead of collapsing the
operation into an opaque mesh edit.

## Design Contract

The MCP surface follows these rules:

- authored data stays primary
- writes go through commands and history
- entity semantics should be legible without reverse-engineering triangle data
- capability-specific commands and metadata should be discoverable

This is what allows Talos3D to be AI-first without relying on private editor
hooks.

The embedded Assistant chat lane follows the same rule. It does not receive a
private bypass API. Instead it uses the MCP endpoint through a generic
`mcp_list_tools` / `mcp_call_tool` bridge, which keeps in-editor automation
aligned with external agents.

## For Capability Authors

Capability packs should contribute enough metadata that MCP clients can:

- discover commands
- inspect authored state
- understand capability-specific semantics
- invoke operations through the public command surface

If a capability only works through UI-specific logic and cannot be understood
through MCP, it is not aligned with the platform direction.
