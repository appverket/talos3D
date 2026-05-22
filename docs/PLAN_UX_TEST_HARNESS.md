# UX Test Harness Plan

Date: 2026-05-21

Status: Proposal (open feasibility spike, see §7)

## 1. Problem

We need a robust way to exercise Talos3D **as the user experiences it**, not as
MCP drives it. The two are materially different, and the current fallbacks
(screenshots + OS-level input, optionally via computer-use) are brittle:

- the app is often behind other windows / not focused, so OS input misfires;
- screenshot diffing couples every assertion to pixel layout, DPI, theme, and
  render timing;
- nothing reproducibly exercises the *input* path that users actually hit.

### Why MCP is not enough

MCP and the real UI **converge at the command / `Messages<T>` layer**. An MCP
`create_box` request ends up sending the *same* `CreateBoxCommand` message the
box tool sends:

- MCP path: HTTP/stdio → channel → `poll_model_api_requests` →
  `handle_create_box` → `send_event::<CreateBoxCommand>`
  (`plugins/model_api.rs`).
- Both paths then flow through the history/command pipeline
  (`plugins/commands.rs`) into ECS.

Everything **above** that convergence line is exercised by the UI but **not** by
MCP — and that stack *is* the user-facing UX:

- input-ownership arbitration, egui-chrome vs. 3D viewport
  (`plugins/input_ownership.rs`);
- cursor → world projection onto the drawing plane
  (`plugins/cursor.rs:352`, `update_cursor_world_pos`);
- viewport picking / raycast selection (`plugins/selection.rs:144`,
  `MeshRayCast`);
- tool state machines reading `just_pressed`/`pressed`/`just_released` across
  frames (`plugins/modeling/tools/box_tool.rs`);
- egui toolbar/palette hit-testing and keyboard shortcuts.

A harness that only drives commands re-tests what MCP already covers. The value
is in driving the layer above the convergence line.

## 2. Key enabling fact: a single faithful injection chokepoint

The vendored `bevy_egui` (0.39.1) reads its input from Bevy **messages** —
`MouseButtonInput`, `CursorMoved`, `KeyboardInput`, `MouseWheel`, `Ime`,
`TouchInput` (`vendor/bevy_egui/src/input.rs:383+`) — the *same* messages Bevy's
own `InputPlugin` consumes to populate `ButtonInput<MouseButton>` and
`ButtonInput<KeyCode>`.

The arbitration between egui and the viewport happens in
`absorb_bevy_input_system` (`vendor/bevy_egui/src/input.rs:1206`): when egui
wants the pointer it calls `reset_all()` on the `ButtonInput` resources and
`clear()`s the message queues, so the viewport never sees a click that landed on
a panel.

**Consequence:** writing synthetic `MouseButtonInput` / `CursorMoved` /
`KeyboardInput` messages into the world reproduces the *entire* real input path —
egui chrome, input ownership, viewport tools, picking — exactly as a real user's
winit events would, and **independent of OS window focus or z-order**. No
screenshots and no OS-level input simulation are required.

Two supporting facts make this practical:

- `MeshRayCast` (selection/picking) is **CPU-side** — viewport picking works
  with no GPU surface.
- AccessKit is available (both `egui` and `bevy_egui` ship the `accesskit`
  feature) but is **not currently enabled**. Turning it on yields a queryable
  widget tree (roles, labels, rects), so egui widgets can be addressed by label
  instead of by pixel coordinate.

## 3. Design overview

A shared **input-driver + ECS-observer** core, deployed in two tiers that reuse
the same primitives.

### Tier 1 — in-process headless integration tests

Build the real `App` with the production plugin set, do **not** call `.run()`,
and step `app.update()` manually. Driver helpers write synthetic input messages;
assertions are ECS queries (selection set, created entities, transforms, tool
state, history). Deterministic, fast, CI-friendly.

This is the gold standard for flow coverage and is what we do not have today:
existing tests (e.g. `app/tests/model_api_recipe_wall.rs`) call handlers on a
bare `World` — no `App`, no input messages, no frame stepping.

### Tier 2 — driving the live running app

Extend the existing model-api channel (`plugins/model_api.rs`) with:

- **input-injection** request variants: `MovePointer`, `Click`, `Drag`,
  `PressKey`, `ClickWidget`;
- **observation** request variants returning structured ECS state.

An agent then exercises the actual GUI the user sees — through the message bus,
immune to focus/z-order — and asserts on structured state instead of pixels.
This is the direct replacement for the current screenshot+MCP workflow.

Both tiers share the driver/observer core, so the hard part is built once.

## 4. Driver API and its correctness traps

- `move_to(world_or_screen_pos)` — must update **both** cursor representations:
  set the `Window`'s stored cursor position (the viewport reads
  `window.cursor_position()` via `plugins/cursor.rs:182`) **and** emit a
  `CursorMoved` message (egui reads that). Keeping only one in sync is the
  easiest mistake to make.
- `click()` / `key_press()` — emit `MouseButtonInput` / `KeyboardInput` **and**
  step the correct number of frames. Tools key off `just_pressed`, which is true
  for exactly one frame, so a click is a press-frame followed by a release-frame.
- `drag(from, to)` — press, a sequence of `move_to` + frame steps, then release.
- `click_widget("Box")` — resolve the target rect from the AccessKit tree, then
  `move_to` + `click`. Layout-independent; survives panel reflow.
- Observation — typed ECS queries, never pixels (e.g. selection set, last
  created element ids, transforms, `History` depth, active tool/state).

## 5. What this buys us

- **Focus/z-order independence** — injection is on the message bus; the OS
  window server is out of the loop.
- **No screenshot brittleness** — assertions are structured ECS state.
- **Real fidelity** — exercises input ownership, cursor projection, picking,
  tool state machines, and egui hit-testing, none of which MCP touches.
- **Determinism** — explicit frame stepping removes timing flake.

## 6. Non-goals / what still needs other coverage

This harness deliberately does **not** validate the OS window server, real GPU
rendering correctness, winit event translation, or platform quirks (e.g. the
macOS first-frame scissor-rect crash the repo vendors a `bevy_egui` patch for).
Those still warrant:

- **Tier 2.5 (optional):** headless render to an offscreen target + image
  snapshot for visual regressions.
- **Tier 3 (rare):** a real-window smoke test to catch platform/windowing
  issues, which can keep using the existing screenshot tooling.

## 7. Open feasibility risk (spike first)

Unconfirmed from static reading: whether the full plugin stack
(`bevy_render` + `webgpu` + `bevy_egui`) will **initialize windowless** for
Tier 1. egui's layout + hit-test + AccessKit run CPU-side in `Update`/`PostUpdate`
regardless of GPU, but `bevy_egui`'s plugin may hard-require a `RenderApp`.
Likely resolutions: offscreen render target + `ScheduleRunnerPlugin` (no winit),
or a headless wgpu backend.

Note that **Tier 2 sidesteps this entirely** — it drives a real window, just not
through the OS — so even if Tier 1 needs more work, Tier 2 delivers value first.

## 8. Build sequence

1. **Refactor** the plugin composition out of `app/src/main.rs:74` into a
   reusable `build_app(opts)` (precondition for clean Tier-1 tests).
2. **Spike** the windowless run: prove the stack initializes and `app.update()`
   advances. ~1–2 hrs of risk burn-down.
3. **Build the driver/observer core** — input-injection + ECS-query helpers,
   with the dual-cursor and frame-stepping nuances handled in one place.
4. **Enable `accesskit`** on `bevy_egui`/`egui`; expose a widget-rect lookup.
5. **Write 2–3 golden-path flow tests** (e.g. activate Box tool → click two
   points → assert one box entity + one history entry; click a toolbar button →
   assert tool activated).
6. **Wire Tier 2** by adding input/observation variants to the model-api
   channel, reusing the same core.

## 9. Coordination with Codex (parallel work)

To avoid collisions while Codex works on related areas:

- **Shared seam = the driver/observer core.** Whoever lands step 3 first should
  publish the function signatures (input-injection helpers + ECS-query helpers)
  so the other builds against a stable interface.
- **`build_app(opts)` (step 1) is a shared dependency** for Tier 1 and likely
  for Codex's work too — land it early and small, separately from the harness
  logic, to minimize merge surface in `main.rs`.
- **Tier 1 vs. Tier 2 are separable** and can be owned independently once the
  core exists; Tier 2 only adds variants to `model_api.rs`, Tier 1 only adds
  files under `app/tests/`.
- **AccessKit enablement (step 4)** is a feature-flag/dependency change that
  touches `Cargo.toml`s — coordinate the timing so it doesn't land mid-rebase.
- Record the branch/worktree in `BRANCH_AND_WORKTREE_LEDGER.md` per project
  convention before starting off-main work.

## 10. Open questions

- Does `bevy_egui` require a `RenderApp` to update egui contexts, or can it run
  with rendering stubbed? (Resolved by the §7 spike.)
- Snapshot/serialization format for Tier-2 observation responses — reuse the
  existing model-api JSON result shapes, or a dedicated view-state schema?
- Should Tier-2 input injection live behind the existing `model-api` feature, or
  a separate `ux-automation` feature so it can be excluded from shipping builds?
