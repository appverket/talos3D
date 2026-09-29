# MCP Model API

## Purpose

Talos3D exposes a Model Context Protocol (MCP) surface so external AI agents
and automation clients can inspect and manipulate the authored model through a
structured interface.

This is a public part of the platform rather than a private editor hook. The
same command substrate backs keyboard shortcuts, toolbar actions, menus, the
command palette, and MCP operations.

## Captured edit plans

Use `list_edit_requests` to discover capability-owned request schemas, then
`preview_edit_plan` with `request_kind` and `parameters`. The initial request
is `core.transform`, using the same move/rotate/scale planner and ordered
semantic modifiers as the viewport. For example:

```json
{"request_kind":"core.transform","parameters":{"element_ids":[6],"operation":"move","axis":"x","value":0.2}}
```

Preview leaves authored state and history unchanged. Its transient `plan_id`
identifies immutable before/after snapshots, a content digest, the base document
revision, semantic intents and their capture-time verdict. `inspect_edit_plan`
also reports the current revision, `stale`, `can_apply_now` and `apply_refusal`.
`can_commit` is the captured semantic verdict, not a promise that the base is
still current. A snapshot preview does not establish visual correctness;
`geometry_reviewed` remains false.

`apply_edit_plan` takes only that `plan_id`: it never replans. It refuses stale
or changed before-state, pending history and semantic refusals; success is one
history item with exact undo/redo. IDs are bounded, session-local and consumed
once. Repeating an applied ID fails. Save durable work as native authored
content/AuthoringScript, not as these transient carrier objects.

Native move/rotate/scale retain the exact candidate displayed by the viewport.
Release applies it without recomputing from a newer cursor. Cancel restores
presentation without adding history. MCP writes and authored-state inspection
are fenced while an interactive gesture owns transient geometry; UX observation,
capture and plan inspection remain available. Finish or cancel the gesture
before inspecting an authored base. Superseded candidate IDs cannot apply.

Capabilities register `EditRequestDescriptor` schemas and pure planners in
`EditRequestRegistry`; domain behavior belongs in those planners and the shared
`EditPlanModifiers` stage. The existing transform-modifier adapter is retained
for linked-placement callers. Feature push/pull's type migration and CSG
finalization remain on their existing command path; they are not advertised as
captured request support. No new preview renderer is introduced here.

Compact profiles advertise the canonical `set_property` schema. Its deprecated
`set_entity_property` alias remains available in `full`.

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

Draft documents are agent-addressable through the registered command surface.
Use `list_commands` to discover the typed schemas for
`drafting.create_draft`, `drafting.select_draft`,
`drafting.update_membership`, and `drafting.inspect_drafts`, then invoke them
with `invoke_command`. A Draft is a semantic drawing-metadata container: it
references existing 3D model entities and 2D annotations by stable element id,
while `DrawingScene` projections and export bytes remain derived. The Draft's
plane reuses the same `DrawingPlane` consumed by interactive tools, so agent and
UI creation share one coordinate-frame contract.

Linked documents use the same registered command surface. Discover and invoke
`modeling.place_linked_model` to insert an external Talos3D document as a live
instance. The result contains an explicit `linked_model_instance` relationship,
including the source path/root/hash, instance root/frame, and source-to-scene
identity map. Placement is one history operation, so undo removes the complete
instance and redo restores the same relationship and scene ids.

Use the read-only `modeling.inspect_linked_models` command to inspect one or
more relationships without refreshing them. Pass `group_id`, `group_ids`, or
neither to inspect selected linked roots and otherwise all linked roots. Each
result reports whether the external source is `current`, `changed`, or
`unavailable` relative to the last successfully loaded hash. Use
`modeling.refresh_linked_models` for the separate mutating lifecycle step.
Downstream domain capabilities should resolve their semantics through the
reported instance root and identity map rather than treating mapped members as
ordinary host-owned geometry.

Use `modeling.inspect_linked_model_placement_subject` before a domain placement
operation. Pass `instance_root_id` (or select one linked root). An applicable
result includes resolved bounds, source-owned ids, host-dependent ids, a
mapping fingerprint, and a refresh-safe source anchor. `not_applicable` and
`stale_mapping` are structured non-mutating outcomes. Transform the returned
instance root as one subject; direct `transform`, `set_property`, or delete
requests against mapped source-owned members are rejected. Host-owned dependent
representations remain attached to the instance when its source is refreshed.

`get_camera` reports both the orbit controller's navigation values and the
actual live `view_position`, `view_forward`, `view_up`, and `view_right` axes.
The latter are authoritative while a Draft plane constrains orientation,
because yaw/pitch alone cannot express arbitrary Draft roll. `set_camera` may
still change focus and zoom during Drafting, but the active Draft constraint is
reapplied before the command response is returned.

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

For the implementation-specific protocol, trust-boundary analysis, threat
model, known limitations, hardening backlog, remote OAuth requirements, and
review checklist, see
[MCP Authentication Security Review](./MCP_AUTH_SECURITY_REVIEW.md).

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
a substantial cold-start context budget. Measure the current schemas and mandatory guidance together rather than relying on historical tool counts. To keep sessions lean, the
advertised tool surface is gated by a named **capability profile**. The session
contract — `get_instance_info`, `negotiate_agent_session`, `get_authoring_guidance`,
`get_capability_snapshot`, `list_guidance_cards` / `get_guidance_card`,
`discover_curated_paths`, agent-skill discovery, and `set_session_profile`
itself — is present in **every** profile, so a fresh MCP-only agent can always
discover guidance and curated paths regardless of gating.

| Profile         | Scope                                                                                                                    |
| --------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `authoring`     | Default. The standard authoring loop: inspection, entity/geometry editing, materials, recipes/discovery and the ADR-042 corpus-gap flow, definitions/occurrences/hosted placement, parametric types, validation and structured geometric checks, refinement and obligations, camera/screenshot capture, project save/load/import, and the `list_commands`/`invoke_command` escape hatch (149 tools / 149,528 compact schema bytes at the focused-authoring checkpoint). |
| `focused-authoring` | Bounded curated authoring and semantic edits: bootstrap/discovery, authored inspection and evidence, shared edit requests, recipes/Definitions/parametrics, gaps, validation, capture, history and save/reload. Omits raw primitive creation and bulk/advanced operations; switch profiles when required. Checkpoint: 83 tools / 70,365 schema bytes, below the fixed gates of 100 tools and 75% of authoring bytes. |
| `inspection`    | Read-only: model/scene/semantic reads, validation checks, camera and screenshot. No model writes.                          |
| `curation`      | Knowledge curation: corpus passages, recipe/assembly-pattern draft management, definition libraries and workspaces, material specs, rule packs, procedural sessions, provenance/grounding, plus inspection and capture. |
| `ux-automation` | UI automation: `ux_*` input simulation, named views, clip planes, toolbars, render/lighting look-dev, command invocation, plus inspection and capture. |
| `full`          | The entire tool surface.                                                                                                   |

Selecting a profile:

- **At connect (HTTP):** each profile has its own endpoint —
  `http://127.0.0.1:<port>/mcp/authoring`, `/mcp/inspection`, `/mcp/curation`,
  `/mcp/focused-authoring`, `/mcp/ux-automation`, `/mcp/full`. Plain `/mcp` serves the default profile
  (`authoring`, or `TALOS3D_MCP_PROFILE` when set).
- **At runtime (any transport):** call `set_session_profile` with
  `{"profile": "full"}` (or omit `profile` to report the current one). Re-fetch
  `tools/list` after changing profiles. Stdio also emits `tools/list_changed`;
  HTTP reports `changed` explicitly and does not require a notification listener.
- **HTTP session lifetime:** initialize returns `Mcp-Session-Id`. Send
  `notifications/initialized`, then include that id on subsequent requests.
  Each initialized session owns its profile, even when clients share a URL and
  instance bearer. Paths choose the initial profile only. A reconnect using the
  same live id retains the profile; a new initialize starts from the path's
  default. DELETE the session on exit. Sessions expire after 30 minutes of
  inactivity or when the app exits; on HTTP 404 initialize again and bootstrap.
  Stateful responses use SSE; read the response matching the request id and
  tolerate intervening notification/priming events. The bearer remains required
  on every request and is never used as a client identity.

The workspace one-shot MCP helper creates and closes a session per invocation.
Use a profile URL such as `/mcp/curation` for those calls; switching a temporary
session does not alter the profile of later invocations or other clients.

Gating is honest rather than silent: calling a tool outside the active profile
returns a structured error naming the profiles that contain it and pointing at
`set_session_profile`, and `get_capability_snapshot` filters its `next_tools`
steering list to the active profile so a gated session is never pointed at a
tool it cannot call. Curated Definition discovery identifies the registered
`definition.instantiate` or `definition.instantiate_hosted` path and includes
`library_id` in its instructions. When gated, `suggested_next_tool` is
`set_session_profile` and `required_profile` identifies the transition; the
asset retains its registered `instantiate_tool`. A profile gate is not a corpus
gap. Read-only authoring provenance and claim grounding are available in both
authoring and inspection without enabling curation writes. Per-profile tool lists are frozen, schema-sanitized once
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
on `promotion_blocked.obligation_element_id` (the aggregate group after
instantiation), then call `promote_refinement` on that element. A blocked
`promote_refinement` with no recipe side effects (no script ran, nothing
created) still returns a plain error. Hard execution or placement failures
roll back the instantiation. Successful `instantiate_recipe` calls create one
history item, including placement, group metadata and `ViaRecipe` provenance
on the locator, generated members and group. One undo removes the complete
result; redo restores the same IDs and captured metadata without rerunning the
recipe. A failed attempt preserves prior history, its redo branch and the
next element ID. This creation guarantee does not extend to arbitrary later
`promote_refinement` calls.

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

Semantic assemblies are authored records, distinct from editing groups. Each
assembly created through the model API is paired with one geometry-bearing
physical group by the internal `core.physical_representation` relation. The
semantic side remains authoritative for typed member roles and validation; the
physical side is authoritative for scene-tree containment, outliner rows,
selection, bounds, and transforms. Parent assemblies therefore retain semantic
assembly ids in their typed membership while their physical groups contain the
paired child groups. Existing project files with the older duplicated/DAG
membership shape are reconciled to this tree when loaded.

For bottom-up modelling, prefer the selection-driven flow over constructing a
raw `create_assembly` payload by hand:

1. Select authored primitives or a group.
2. Call `preview_semantic_assembly_from_selection` with optional `query` text
   such as `"wall"`. The response expands selected groups to leaf members,
   ranks registered assembly types, and returns valid member-role choices for
   the chosen assembly type.
3. Call `create_semantic_assembly_from_selection` with explicit
   `assembly_type` and `member_role`. The tool creates the semantic assembly,
   creates/selects a paired physical group, nests any existing physical
   subgroups without duplicating their leaves, records bottom-up-selection
   metadata, and may annotate member
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

### Editing imported mesh positions

`set_entity_property` / `set_property` accepts `vertices` for `triangle_mesh`
entities. Supply the complete array of finite `[x,y,z]` positions with the
existing vertex count. This changes positions through the shared undo/history
command while preserving entity id, faces, name, layer, materials and group
membership; derived normals are regenerated. It does not infer wall/window
semantics, change topology, or establish a refinement claim. Read the current
snapshot first and verify unchanged source geometry before applying a prepared
edit. Use native hosted/parametric edits for semantic architectural entities.

## Procedural session freshness and retries

`procedural_session.create` captures a transient document identity and monotonic
model revision in `snapshot.base_model_revision`, plus a `commit_id`. Include
that identity in `procedural_session.commit`. Older clients may omit it; omission
uses the prepared identity and does not request another execution.

An accepted edit, undo, redo, new document, or project load invalidates an
uncommitted proposal. Commit refuses before mutation when its captured revision
is stale. Create a new session from the current model and reevaluate the steps.
Changing a document back to its previous shape does not make an old proposal
current again.

Each session has one successful execution. Retrying with the same options
returns the original receipt, including its `commit_id`, without replay—even
after undo or document replacement. The receipt describes the original execution,
not the current existence of its output entities. Different commit options or
another identity are refused after success; further evaluation is also refused.
For a deliberate rerun, create a new session. Sessions and receipts are transient
and are not restored by loading a project or restarting the app.

### Procedural commit transaction and validation

The live session executor applies supported commands provisionally, checks real
postconditions and current whole-document validators/obligations, then accepts
one history action. A late dispatch error, unmet postcondition, policy refusal,
or failed inline export rolls back admitted commands and preserves the prior
undo/redo history, save point, and pending user work. A successful compound
commit has one undo/redo unit. Rollback advances the revision fence; rebuild a
failed proposal from the current document before retrying it.

The audited set is `create_box`, `create_entity`, `definition.create`,
`definition.instantiate`, `occurrence.place`, `set_property`, `model_summary`,
and `run_validation_v2`. `ProjectRoot` edits apply to new content; `set_property`
cannot modify a pre-existing element through that scope. Refinement scopes,
organization-library writes, nested session instructions, and `parametric.create`
currently return `unsupported_transaction` before any step executes. Their
native APIs remain available; they need an audited command transaction before
being admitted to a procedural commit. Pending user commands also refuse commit
so they cannot be swept into a procedure's rollback or undo group.

Dry-run responses explicitly say `structural_projection_only`. Their stub IDs,
findings, and bindings are descriptor projections, not a live preview or proof
of geometric feasibility. The commit receipt identifies `live_model` validation,
its registered constraint IDs and whole-document scope. No findings does not
mean unregistered constraints passed or that rendered geometry was reviewed.

`require_clean` rejects live findings and unresolved/deferred obligations.
`accept_with_waivers` requires exactly identified current findings and non-empty
rationales; it cannot silently waive outstanding obligations. `accept_partial`
reports current findings and carries unresolved obligations explicitly. `options.postconditions` supplies additional commit promises and is included
in the retry identity and accepted script export. Relation
postconditions inspect real endpoints; claim grounding must match on an
unambiguous target; obligation satisfaction must refer to the actual output
entity of the specified step. Missing or ambiguous evidence fails closed.

## Design explanation

`explain_design { element_id }` returns the same read projection shown in the
property inspector's **Design explanation** section: recorded creation and
assembly context, Definition controls and hosting, direct dependencies, grounding
and source references, unresolved decisions/obligations, and last-sweep validator
coverage. It includes the document/model revision and explicit omission counts.

The projection is transient. Missing direct provenance is reported as missing,
not inferred to be Freeform; assembly membership is context, not inherited proof.
A source reference can be present, missing, or unresolved by this projection.
Presence does not establish applicability. Validation is a recorded sweep, not a
fresh run or an assertion that absent findings prove completeness. Dependencies
include recorded inactive alternatives and do not replace an exact edit plan.

Limits are 24 rows per section, 512 characters per text field, 2 KiB per structured
row detail, and a 48 KiB retained-row budget (complete responses below 64 KiB).
Use the named detail tools for omitted content. `definition.explain`,
`occurrence.resolve`, and `lookup_source_passage` are available in inspection
alongside `explain_design`; these are read-only operations.

The inspector refreshes on selection, revision, Definition registry, relevant
source-component or dependency-graph changes. It clears while a transform is
active and offers an explicit Refresh action for a new validation observation.
No dependency scan is performed per frame for an unchanged selection.

`get_authoring_provenance` likewise returns `mode: "Unrecorded"` when no direct
component exists. Explicit Freeform records remain Freeform. Elevation-intent
declaration and binding are editing operations and are not in inspection.

### Explicit project reload

`load_project` always reads the requested file and replaces document state, even
when it is the current path and the dirty flag is false. This discards unsaved
state and invalidates old document revision tokens and transient parametric
instances. A missing or invalid file reports an error before replacing the scene.
Use `get_instance_info` to inspect the current document without reloading it.

### Native drag evidence

`ux_drag { start, end, steps, trace: true }` opts into a bounded input trace.
`ux_observe.drag_trace` retains the most recent traced gesture (at most 128
rows; a drag admits at most 123 input steps). Each row identifies the captured
native candidate, affected IDs, model revision, and preview refusal if any.
The release row independently compares the committed authored snapshots with
the exact candidate retained before release and requires one revision advance.
Untraced gestures incur no snapshot-comparison or trace-row work.

Timing begins when the harness injects an input edge into Bevy, excluding MCP
transport/queue latency. `main_frame_ms` ends after the main schedule;
`release_to_commit_ms` is emitted only for an exact successful commit.
`present_ms` ends after Bevy submits the same extracted frame's primary-window
swapchain for presentation. It does not measure physical display scanout. A
missing acquired/presented surface leaves this field null; headless CPU work
must never be counted as a rendered-frame pass. The hook uses Bevy's render
schedule directly, without a renderer fork or synchronous GPU readback.

Input injection precedes cursor projection, snapping, and modal preview/confirm
consumers. Use rendered inspection alongside traces: a submitted frame proves
neither geometric correctness nor that an intended grip was selected. A release
without a captured candidate has no commit verdict and cannot pass the gesture
gate. Exported viewport images intentionally suppress manipulator overlays;
`include_ui: true` requires a capturable native window.

### Captured semantic edits

`list_edit_requests` advertises `core.semantic` alongside `core.transform`.
Its live schema accepts `AssignConcept`, `RemoveConcept`, `PublishAnchors` and
`Bind` intents (externally tagged JSON variants). An anchor target contains
`publisher`, `kind` and `role`. Preview with `preview_edit_plan`, inspect the
captured `semantic_changes` and `semantic_refusals`, and apply only the returned
`plan_id`. Refusals include the proposition, observed host, repair, contrasts
and evidence; compatible current anchors are named when available.

The existing AuthoredEditPlan captures before/after views of ConceptAssignment,
PublishedAnchors and SemanticBindings. Apply, undo and redo restore those exact
components. Both revision checks and semantic before-state checks fence stale
proposals, including changes made by legacy metadata writers. Existing native
semantic sidecars persist this state; no additional durable graph is introduced.
The compatibility tools `assign_concept` and `publish_anchors` use this same
captured command/history path.

`resolve_domain_term` returns publication contracts and current required-anchor
candidates. `get_entity_details` and the shared design explanation expose the
recorded meaning and bindings. Publication roles must satisfy the installed
contract. Reidentifying a concept validates retained bindings and publications;
explicit removal records a downgrade and clears its own semantic components.
Bare-entity relations remain the responsibility of their relation planners.

Declared anchor identity does **not** prove resolved geometry, automatic
placement/reseating, engineering validity or parametric driver persistence.
Geometry edits still use the appropriate shared edit request or curated
materializer. This bounded semantic request has no dedicated native drag UI;
the wider interactive anchor-binding and regeneration acceptance remains open.
