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
- discover registered assembly types, reusable assembly patterns, and relation vocabulary
- inspect authored semantic assemblies and typed relations
- query registered commands, toolbars, layers, groups, and selection state
- invoke write operations through the same command pipeline the UI uses
- import files and capture viewport screenshots

Recent additions expose first steps toward higher-order semantic structure:
capabilities can register assembly types, reusable assembly patterns, and
relation vocabulary, and MCP clients can inspect or create authored assemblies
and relations through structured tools.

The recipe-discovery surface now also supports a session-scoped bridge for
dynamic recipe learning. Missing recipes can be captured as draft artifacts
linked to corpus gaps and source passages, marked installed for the current
session, and then surfaced back through recipe discovery tools without
requiring product-code changes or hot-loaded Rust.

The same bridge now exists for reusable layered assembly patterns. Missing wall
or roof stack knowledge can be captured as session-scoped assembly-pattern
drafts, linked to corpus evidence, marked installed for consultation, and
surfaced back through `list_vocabulary` without changing product code.

Desktop builds may also warm-start these session drafts from a storage-backed
local cache. That cache is non-authoritative: it exists so standalone desktop
sessions can resume useful learned context without assuming any backend.

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
unit overrides. For authoring workflows that should not reconstruct raw world
points, `place_dimension_between_handles` resolves stable handle ids such as
box `corner_0` … `corner_7` through the same public handle surface exposed by
`list_handles`.

Built-in definition libraries are also loaded at startup and show up through
the same definition-library inspection tools as project-local libraries. Their
reported scope is `Bundled`, which distinguishes shipped catalogs from
document-local or imported external libraries.

Material payloads expose a broader Bevy-backed appearance contract than the
initial MVP: specular tint, transmission, thickness, IOR, attenuation,
clearcoat, anisotropy, unlit/fog flags, and depth bias are all readable and
writeable through the material tools.

Texture mapping is also agent-addressable. Use `get_texture_mapping`,
`update_texture_mapping`, and `reset_texture_mapping` against exactly one target:
either `material_id` for the shared material default or `element_id` for an
assignment-scoped override. Mapping payloads cover projection, `uv_scale`,
`uv_offset`, `uv_rotation_deg`, `flip_u`, `flip_v`, and `blend_sharpness`.
Element-target inspection includes UV diagnostics so agents can distinguish a
bad mapping transform from missing or degenerate mesh UVs. Non-UV projections
are accepted as authored intent but reported as not yet rendered by the current
Bevy `StandardMaterial` path.

Viewport renderer state is also available over MCP through
`get_render_settings` and `set_render_settings`. Those tools expose
tonemapping, exposure, SSAO, bloom, SSR, background color, grid visibility,
paper fill, X-Ray face transparency, and drawing overlays so an agent can tune
the working view or compose export-ready drawing views without simulating UI
input. X-Ray is also available as the `view.toggle_xray` command; invoking it
without parameters toggles the user-facing view, while `enabled` can set an
explicit on/off state for automation. The default X-Ray face alpha is `0.5`.

Scene lighting is also available over MCP. Ambient lighting is explicit scene
state, while directional, point, and spot lights are authored entities with
stable element ids and editable properties.

## Standard Agent Loop

Agents should treat MCP as a semantic model contract, not as a geometry macro
recorder. The expected loop is:

1. Start with `negotiate_agent_session`. Confirm the returned instance id and
   follow its required bootstrap steps. The welcome composes the live guidance,
   active capability profile, bounded capability snapshot, relevant skills or
   card fallbacks, and refresh triggers for the current task.
2. Inspect the loaded capability surface with `list_vocabulary`,
   `list_element_classes`, `list_recipe_families`, `list_constraints`,
   `list_generation_priors`, and `list_catalog_providers` as directed by the
   welcome.
3. Inspect the current document with `model_summary`, entity queries, assembly
   queries, relation queries, selection state, camera state, and screenshots
   where visual grounding is useful.
4. Author or refine semantic structure through commands and Model API tools.
   Prefer authored entities, assemblies, relations, recipes, definitions, and
   stable handles over raw coordinate reconstruction.
   For repeatable work, capture the intended entities, relations, recipe
   choices, validation expectations, and deferrals as a scenario file rather
   than leaving the plan only in the prompt transcript.
5. Run validation after meaningful changes. Treat findings as part of the
   authoring loop, not as a final report bolted on at the end.
6. Explain unresolved findings and obligations in terms of refinement state:
   conceptual gaps, schematic coordination issues, constructible blockers, or
   explicitly deferred work.
7. When a needed recipe, assembly pattern, source, or rule is missing, create a
   gap or session draft instead of inventing unsupported structure silently.
8. Re-run validation after each refinement step and preserve accepted waivers
   or deferrals as authored state.
9. Produce named views, screenshots, and drawing exports from the same authored
   model when the result is ready to communicate.

This loop is intentionally agent-independent. The embedded assistant, Claude,
Codex, scripted tests, and future hosted agents should all follow the same
surface. If a workflow only works through hidden editor hooks or prompt luck, it
is not aligned with the platform direction.

## Running Talos3D With MCP Enabled

Start the public core app with the `model-api` feature:

```bash
cargo run --manifest-path app-core/Cargo.toml --features model-api
```

To run multiple MCP-enabled instances without collisions, provide a unique
instance id and port:

```bash
cargo run --manifest-path app-core/Cargo.toml --features model-api -- --instance-id codex --model-api-port 24842
cargo run --manifest-path app-core/Cargo.toml --features model-api -- --instance-id claude --model-api-port 24901
```

The core app target is `app-core/`. The sibling `app/` target is a product
composition used in the full Appverket workspace and may depend on private
domain packs such as architecture extensions.

If no port is provided, Talos3D prefers `24842` and automatically falls back to
an available port when that default is already in use.

When enabled, Talos3D exposes MCP endpoints in two forms:

- stdio transport for local process-based integrations
- streamable HTTP at `http://127.0.0.1:<port>/mcp`

On startup the app writes local, untracked `.mcp.json` files for the detected
Talos3D core checkout and outer multi-repo workspace, when those roots are
discoverable from the launch directory. That gives fresh MCP clients a
repository-scoped endpoint config with the actual bound port:

```json
{
  "mcpServers": {
    "talos3d": {
      "url": "http://127.0.0.1:24842/mcp"
    }
  }
}
```

Those files are intentionally ignored so local ports and instance choices do not
end up committed. They contain discovery facts only, never a pairing grant or
bearer credential. Set `TALOS3D_MCP_CONFIG_PATHS` to an OS path-list of explicit config
files when launching from a packaged app or another directory. Set
`TALOS3D_WRITE_MCP_CONFIG=false` to disable writing local client configs.

### Access control

The local HTTP transport uses a user-mediated, one-time pairing handoff. The
user-visible **Connect an AI Agent** prompt contains a random single-use pairing
code, never the MCP bearer. The intended agent sends the code once to
`POST /mcp/pair`; Talos3D atomically consumes it and returns a separate random
process-lifetime access token. Replaying the prompt fails. Clients then send:

```http
Authorization: Bearer <access-token-returned-by-pairing>
```

Missing or invalid credentials receive `401 Unauthorized`. Restarting the app
invalidates both values. Pairing proves that a user with access to the local
Talos3D desktop UX handed this running instance to the agent. It does not
establish a named user, delegated agent identity, or independent command
authorization policy; the local desktop/OS session is the user-presence trust
boundary.

The app defaults to a generated pairing code. A repeatable local test harness
may provision a code of at least 32 non-whitespace characters through
`TALOS3D_MODEL_API_TOKEN`; Talos3D still keeps that value out of logs,
manifests, discovery configs, and `InstanceInfo`.

This local pairing route is deliberately not presented as production OAuth.
A shared or remotely reachable Talos3D resource must follow the MCP
authorization specification: OAuth authorization-server and protected-resource
metadata discovery, Authorization Code with PKCE for user-delegated access,
resource/audience-bound tokens, least-privilege scopes, explicit consent,
expiry, revocation, and audit binding to the authenticated Talos3D user and MCP
client. A remote onboarding prompt carries discovery information only; it must
not carry an access token, refresh token, client secret, or reusable API key.

The bearer check complements the loopback access guard: the `Host` header must
name the loopback authority actually bound (defeating DNS-rebinding), and any
`Origin` header must be the matching loopback origin (defeating cross-origin
browser drive-bys). Requests that fail those checks receive `403 Forbidden`.
Capability profiles remain separate tool-surface filters and are never treated
as authentication or authorization.

## Agent Welcome And Onboarding

`negotiate_agent_session` is the Talos3D-native connection handshake. It is
available in every capability profile and accepts an Agent Hello with optional
client identity, task, requested profile, context budget, delegation mode, and
support flags for skills, MCP resources/prompts, images, notifications, and
interactive approval.

The Agent Welcome returns:

- the exact `InstanceInfo` for the running app;
- the active and available profiles without silently switching them;
- the security assurance known by this transport, including successful
  instance-bound bearer authentication derived from a single-use local pairing
  handoff, without claiming delegated user identity;
- a compact live capability snapshot and required guidance-card ids;
- at most a small context-budget-aware set of task-relevant agent-skill
  summaries when the client supports skills;
- tool/card fallbacks, ordered bootstrap calls, required invariants, and refresh
  triggers;
- revision anchors for the facts the runtime can version, and an explicit
  `null` knowledge epoch while no single authoritative mutable-knowledge epoch
  exists.

The handshake is safe to repeat after reconnect, task/profile change,
`tools/list_changed`, stale guidance, or curated-path/corpus changes. It
composes the same runtime registries as the normal tools; it is not a second
knowledge store.

Desktop apps expose this through **AI → Connect an AI Agent…**. The dialog shows
the live instance and endpoint and generates a ready-to-paste onboarding prompt
with one-click copy. The local prompt carries a single-use pairing grant,
rendezvous facts, and stable instructions; the bearer is returned only after
redemption. Current Talos3D knowledge still comes from the welcome and its
follow-up calls. Treat the copied local prompt as a short-lived secret and share
it only with the intended agent.

## Capability Profiles (tool gating)

The full router registers a large tool surface, whose schemas cost a connecting agent
roughly 196 KB (~35k tokens) of cold-start context. To keep sessions lean, the
advertised tool surface is gated by a named **capability profile**. The session
contract — `get_instance_info`, `negotiate_agent_session`, `get_authoring_guidance`,
`get_capability_snapshot`, `list_guidance_cards` / `get_guidance_card`,
`discover_curated_paths`, agent-skill discovery, and `set_session_profile`
itself — is present in **every** profile, so a fresh MCP-only agent can always
discover guidance and curated paths regardless of gating.

| Profile         | Scope                                                                                                                    |
| --------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `authoring`     | Default. The standard authoring loop: inspection, entity/geometry editing, materials, recipes/discovery and the ADR-042 corpus-gap flow, definitions/occurrences/hosted placement, parametric types, validation and structured geometric checks, refinement and obligations, camera/screenshot capture, project save/load/import, and the `list_commands`/`invoke_command` escape hatch (~102 tools, ~87 KB). |
| `inspection`    | Read-only: model/scene/semantic reads, validation checks, camera and screenshot. No model writes.                          |
| `curation`      | Knowledge curation: corpus passages, recipe/assembly-pattern draft management, definition libraries and workspaces, material specs, rule packs, procedural sessions, provenance/grounding, plus inspection and capture. |
| `ux-automation` | UI automation: `ux_*` input simulation, named views, clip planes, toolbars, render/lighting look-dev, command invocation, plus inspection and capture. |
| `full`          | The entire tool surface.                                                                                                   |

Selecting a profile:

- **At connect (HTTP):** each profile has its own endpoint —
  `http://127.0.0.1:<port>/mcp/authoring`, `/mcp/inspection`, `/mcp/curation`,
  `/mcp/ux-automation`, `/mcp/full`. Plain `/mcp` serves the default profile
  (`authoring`, or `TALOS3D_MCP_PROFILE` when set).
- **At runtime (any transport):** call `set_session_profile` with
  `{"profile": "full"}` (or any profile name; omit `profile` to report the
  current one). Subsequent `tools/list` calls return the new frozen list. On
  stdio the server also emits a `tools/list_changed` notification; the HTTP
  transport is stateless with JSON responses (no channel for server-initiated
  notifications between requests), so HTTP clients should re-fetch `tools/list`
  after switching — the tool's response reports the change either way. The
  switch is scoped to the endpoint/session you are connected to — fine for a
  single-user local app.

Gating is honest rather than silent: calling a tool outside the active profile
returns a structured error naming the profiles that contain it and pointing at
`set_session_profile`, and `get_capability_snapshot` filters its `next_tools`
steering list to the active profile so a gated session is never pointed at a
tool it cannot call. Per-profile tool lists are frozen, schema-sanitized once
per process, and shared across sessions.

Tool-to-profile membership lives in
`crates/talos3d-core/src/plugins/model_api/profiles.rs` (one explicit
name→category table plus prefix rules for namespaced families). A test fails if
a new tool is left unclassified, so additions land in a profile deliberately.

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

`get_instance_info` also reports the live `authoring_guidance_id` and
`authoring_guidance_version` when an authoring guidance resource is installed.
Treat the live MCP value as authoritative for the running app. If local docs or
source files claim a newer guidance version, rebuild/restart the app before
authoring; otherwise the agent is operating against a stale harness.

If MCP tool discovery is empty in a fresh agent session, that only means the
client did not load a Talos3D server yet. Check `.mcp.json` first, then fall
back to the instance registry above. Prefer manifests whose `pid` is still
running and whose `http_url` responds to MCP `initialize`; stale manifests can
remain after an app process exits.

## Tool Surface

Which of these tools a session actually sees depends on its
[capability profile](#capability-profiles-tool-gating); the lists below
describe the full surface. Current tool categories include:

### Model inspection

- `get_instance_info`
- `negotiate_agent_session`
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
- `preview_semantic_assembly_from_selection`

`list_vocabulary` now returns:

- `assembly_types`
- `assembly_patterns`
- `relation_types`

### Recipe discovery and session drafts

- `list_element_classes`
- `list_recipe_families`
- `select_recipe`
- `list_constraints`
- `list_generation_priors`
- `list_catalog_providers`
- `catalog_query`
- `list_corpus_gaps`
- `request_corpus_expansion`
- `lookup_source_passage`
- `draft_rule_pack`
- `list_recipe_drafts`
- `get_recipe_draft`
- `save_recipe_draft`
- `set_recipe_draft_status`
- `list_assembly_pattern_drafts`
- `get_assembly_pattern_draft`
- `save_assembly_pattern_draft`
- `set_assembly_pattern_draft_status`

### Validation and findings

- `run_validation`
- `explain_finding`
- `run_validation_v2`
- `explain_finding_v2`

Structured geometric checks (read-only, AABB-level, on demand — intended as a
cheap first verification pass before `take_screenshot`):

- `get_world_aabb` — world-space AABB per element plus the combined box
- `check_overlaps` — pairwise AABB intersections (group/member pairs excluded;
  capped with a `truncated` flag)
- `check_floating` — elements whose underside hangs above the nearest support
  (falls back to the y=0 plane when no terrain elevation is available)
- `check_clearance` — AABB distance between two elements against a minimum

### Agentic authoring run contract

The guidance-card surface carries an eval-style harness contract in addition to
plain prose. Bootstrap cards such as `dkg.authoring_run_contract` and
`dkg.trajectory_eval` expose:

- `required_trajectory_tool_ids` — tools expected in a well-formed run
- `success_criteria` — output/evidence rubric
- `stop_conditions` — when to record a gap or stop instead of improvising
- `observability_events` — facts the agent should be able to report or audit
- `recommended_profile` — suggested capability profile for the task shape

For non-trivial authoring, a final claim should be backed by intent, discovered
resources, selected execution path, validation findings, structured geometric
checks where relevant, screenshot review, unresolved CorpusGap ids, and the
active guidance version. This evaluates both the final model and the tool
trajectory that produced it.

### Editing and authored changes

- `create_entity`
- `create_box`
- `place_guide_line`
- `place_dimension_line`
- `place_dimension_between_handles`
- `create_assembly`
- `create_semantic_assembly_from_selection`
- `delete_entities`
- `transform`
- `set_property`
- `set_entity_property` (deprecated alias of `set_property`)
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
- `get_camera`
- `set_camera`
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
- `take_screenshot` (`include_ui: true` captures the full egui app window for UX QA)
- `export_drawing`

`model_summary` now also reports `assembly_counts` and `relation_counts` in
addition to entity counts and capability-defined metrics.

### Local coordinate frames (groups as scene-graph nodes) — ADR-058

A group carries a **local coordinate frame** (origin + rotation, identity by
default). Geometry authored while you are *inside* the group is expressed in that
rectified local frame and composed to world by the frame — the scene-graph /
SketchUp-component model. This is the correct way to build an angled or compound
volume: author it **axis-aligned** in clean local coordinates and let the frame
carry the angle, so every wall and gable-end inherits the same orientation and
can never be left disagreeing with the body of the volume.

Two equivalent workflows (no new tool needed):

1. **Frame-first.** Create the group with a frame, enter it, author axis-aligned:
   ```
   create_entity {"type":"group","name":"living_wing",
                  "frame_origin":[12,0,4],"frame_rotate_euler_deg":[0,18,0]}
   enter_group   {"element_id": <group>}
   create_box    {... axis-aligned coords in the wing's local frame ...}   // auto-joins the group, composed to world
   wall / opening / instantiate_recipe ...                                  // all inherit the 18° Y rotation
   exit_group    {}
   ```
2. **Author-then-rotate.** Create a plain group, enter, author axis-aligned at the
   origin, exit, then rotate the whole assembly as one rigid unit about its
   junction corner:
   ```
   transform {"element_ids":[<group>],"operation":"rotate","axis":"Y",
              "value":18,"pivot":[12,0,4]}
   ```

`get_editing_context` reports the active frame (`frame_is_identity`,
`frame_origin` in metres, `frame_rotate_euler_deg` in degrees) — the product of
all entered groups' frames, so nesting composes recursively. Transforming a group
moves/rotates every (recursive) member together and updates the group's frame,
so the assembly stays editable in its own rectified space afterward. Frames are
identity-default: plain groups and all non-grouped authoring are unaffected.

`instantiate_recipe` and `promote_refinement { recipe_id }` **execute**
registered recipes whose body is an `AuthoringScript`: the script replays
through the normal command pipeline (undoable), and the response carries the
created element ids, the number of steps run, the recipe id/revision used, and
any validation findings. Recipes whose `NativeFnRef` body does not resolve
return a structured not-executable error instead of silently recording a bare
state change. Trust the `executable` / `execution_path` fields on
`select_recipe` / `list_recipe_families` responses — they are computed from the
actual body type.

When the recipe body executes but the post-execution promotion gate blocks on
unsatisfied obligations, both tools return **partial success**, not an error:
the created geometry persists, so the response carries `created_element_ids`,
the unchanged refinement `state`, and a `promotion_blocked` object
(`unsatisfied_obligations` + `message`). Do not retry the call — that
duplicates geometry. Resolve each listed obligation with `resolve_obligation`
on the root element, then call `promote_refinement` again. A blocked
`promote_refinement` with no recipe side effects (no script ran, nothing
created) still returns a plain error. Real execution failures that occur after
elements were already created return an error whose message lists the
persisted element ids.

Session recipe drafts are still **not executable by `instantiate_recipe`**.
Installed drafts can be appended to `list_recipe_families` and `select_recipe`
when the caller opts in, but `select_recipe` marks them `executable: false`
unless they carry an evidence-backed `geometry_emission` runtime claim and a
`draft_script.parametric_create` replay payload. Executable learned assets are
materialized with `materialize_learned_asset`; consultable-only drafts and
corpus-gap records do not close an authoring gap.

Agent-acquired knowledge is durable at write time: recipe drafts,
assembly-pattern drafts, and corpus gaps flush to
`<knowledge_dir>/session/<instance_id>/` on every save/status change (atomic
writes; I/O failure logs a warning and never fails the authoring call) and are
recovered into the live registries on the next startup of the same instance.

Region-specific learned knowledge is keyed by scope, not by global defaults.
`discover_curated_paths` and `select_recipe` accept `jurisdiction`, `region`,
or `locale` in their context object. Generic assets remain visible in every
scope; jurisdiction-scoped learned recipes, recipe drafts, assembly-pattern
drafts, and curated manifests are returned only when the requested scope
matches. With no requested scope, discovery stays region-neutral and compact,
so an agent must infer or ask for the project/request region before pulling
regional construction knowledge.

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
- direct live camera control is also available through `get_camera` and
  `set_camera`
- white-paper presentation can be produced through `background_rgb`,
  `grid_enabled`, and `paper_fill_enabled`
- hidden-line-friendly export can be approximated with
  `visible_edge_overlay_enabled`
- drawing exports can be written directly as `png`, `pdf`, or `svg` through
  `export_drawing`; `take_screenshot` now accepts the same output formats when
  a path extension requests them, and can include app chrome/panels with
  `include_ui: true`
- the same viewpoint and drawing toggles are also reachable through
  `invoke_command` and discoverable through `list_commands` / `list_toolbars`
  using the `view.*` command family (`view.front`, `view.back`, `view.top`,
  `view.bottom`, `view.left`, `view.right`, `view.isometric`,
  `view.projection_perspective`, `view.projection_orthographic`,
  `view.apply_paper_preset`, `view.toggle_grid`, `view.toggle_outline`,
  `view.toggle_wireframe`, and `view.toggle_compass` — the latter shows or
  hides the corner compass rose, which is drawn in world orientation so it
  indicates geographic north (site `north_axis_deg` convention) even when
  the camera is tilted)

## Example: Box, Corner Dimension, Camera, Screenshot

For the basic interactive workflow an agent should be able to:

1. Create a box with `create_box`.
2. Discover its stable corner handles with `list_handles`.
3. Dimension between two corners with `place_dimension_between_handles`.
4. Reposition the live camera with `set_camera`.
5. Capture the viewport with `take_screenshot`, or pass `include_ui: true` when
   validating egui panels and other app chrome.

Example requests:

```json
{
  "center": [0.0, 1.0, 0.0],
  "size": [4.0, 2.0, 1.0]
}
```

`list_handles` on the created box will return entries such as `corner_0`,
`corner_1`, `corner_2`, and so on.

```json
{
  "start_element_id": 12,
  "start_handle_id": "corner_0",
  "end_element_id": 12,
  "end_handle_id": "corner_3",
  "offset": 0.5,
  "extension": 0.25
}
```

```json
{
  "focus": [0.0, 1.0, 0.0],
  "projection": "orthographic",
  "orthographic_scale": 3.0,
  "yaw": 0.75,
  "pitch": -0.4
}
```

```json
{
  "path": "/tmp/talos3d-box-dimension.png"
}
```

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

For bottom-up modelling, prefer the selection-driven flow over constructing a
raw `create_assembly` payload by hand:

1. Select authored primitives or a group.
2. Call `preview_semantic_assembly_from_selection` with optional `query` text
   such as `"wall"`. The response expands selected groups to leaf members,
   ranks registered assembly types, and returns valid member-role choices for
   the chosen assembly type.
3. Call `create_semantic_assembly_from_selection` with explicit
   `assembly_type` and `member_role`. The tool creates the semantic assembly,
   creates/selects a physical group for the same members, records
   bottom-up-selection metadata, and may annotate member
   `SemanticIntent.parameters.component_role` for later queries.

This is the programmatic equivalent of the UI command **Create Semantic
Assembly**: choose a semantic assembly from a searchable list, then choose what
component role the selected geometry represents inside that assembly.

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
