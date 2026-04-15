# Engine Fork Workflow

## Purpose

This document defines the only supported way to work with Talos3D when local
forks of Bevy and egui are involved.

The goal is simple:

- keep Talos3D understandable
- keep local builds predictable
- reduce surprise when upstream changes land
- make sure humans and AI agents follow the same structure

If a task touches Bevy, egui, `bevy_egui`, or editor UI behavior that may be
affected by those dependencies, read this document first.

## Repositories

Talos3D currently depends on three layers:

1. Talos3D app and platform code
2. Bevy engine
3. egui UI library

Current local checkouts:

- Talos3D: `/Users/torstenek/dev/talos3D/talos3d-core`
- Bevy fork: `/Users/torstenek/dev/bevy`
- egui fork: `/Users/torstenek/dev/egui`

Current important constraint:

- Talos3D uses vendored `bevy_egui`
- that vendored `bevy_egui` is on the Bevy `0.18` / egui `0.33` line
- local egui `main` is newer and already contains fixes that Talos3D cannot
  use directly until the bridge is updated

## Plain-English Model

Treat the three repositories as having different jobs:

- Talos3D is where product behavior lives
- Bevy is the engine fork we may track more closely over time
- egui is the UI library fork
- `bevy_egui` is the bridge between Bevy and egui

The bridge matters because Talos3D does not talk directly to egui. It talks to
egui through `bevy_egui`.

That means:

- a fix in egui does not automatically fix Talos3D
- a fix in Bevy does not automatically fix Talos3D
- sometimes the real work is in the bridge

## Branch Structure

Use these branch roles consistently.

### Talos3D

- `main`
  - normal Talos3D development branch
  - should stay buildable without depending on in-progress engine migrations
- `talos/bevy-main`
  - Talos3D integration branch for tracking newer Bevy work
  - this is where path dependencies to local Bevy belong

### Bevy fork

- `main`
  - mirror of upstream Bevy `main`
  - do not put Talos-specific experiments directly here
- `talos/<topic>`
  - only for Bevy changes that are real engine changes and could be proposed
    upstream

### egui fork

- `main`
  - mirror of upstream egui `main`
- `talos/egui-0.33-panel-fix`
  - backport branch for fixes needed by the old egui line that Talos3D still
    reaches through vendored `bevy_egui`
- `talos/<topic>`
  - only for egui changes that belong upstream

## Hard Rules

These rules are here to lower cognitive load.

1. Do not develop Talos-specific engine work directly on Bevy fork `main`.
2. Do not develop Talos-specific UI library work directly on egui fork `main`.
3. Keep fork `main` branches as mirrors of upstream `main`.
4. Put local experimental or upstreamable changes on named topic branches.
5. Treat `bevy_egui` as a separate compatibility layer, not as “just Bevy” or
   “just egui”.

## One-Time Setup

Each fork should have both `origin` and `upstream`.

### Bevy

```bash
cd /Users/torstenek/dev/bevy
git remote add upstream https://github.com/bevyengine/bevy.git
git fetch upstream
```

### egui

```bash
cd /Users/torstenek/dev/egui
git remote add upstream https://github.com/emilk/egui.git
git fetch upstream
```

You only need to add `upstream` once.

## How To Refresh Forks

This is the normal refresh flow.

### Refresh Bevy fork `main`

```bash
cd /Users/torstenek/dev/bevy
git fetch upstream
git checkout main
git rebase upstream/main
```

### Refresh egui fork `main`

```bash
cd /Users/torstenek/dev/egui
git fetch upstream
git checkout main
git rebase upstream/main
```

If you want your GitHub fork updated too:

```bash
git push origin main
```

## Current Recommended Working Modes

There are two supported modes.

### Mode A: Immediate Product Fixes

Use this when:

- Talos3D needs a fix now
- the fix already exists in a newer egui or Bevy line
- the bridge is still pinned to older versions

Current example:

- egui `main` already contains the panel-state clamp fix
- Talos3D still reaches egui through vendored `bevy_egui` on the older egui
  `0.33` line
- therefore the quickest safe path is a backport branch, not a full bridge
  upgrade

Recommended branch:

```bash
cd /Users/torstenek/dev/egui
git fetch upstream --tags
git checkout -b talos/egui-0.33-panel-fix tags/0.33.3
```

Then backport the needed fix onto that branch.

### Mode B: Engine Tracking

Use this when:

- Talos3D needs new Bevy features from `main`
- you want to migrate continuously instead of waiting for a release drop

Recommended branch:

```bash
cd /Users/torstenek/dev/talos3D/talos3d-core
git checkout -b talos/bevy-main
```

This is the only Talos3D branch where local Bevy path dependencies should be
introduced.

## How To Build Talos3D Against A Local egui Backport

This is the lowest-risk way to consume a fix that exists in newer egui but is
still needed through the old bridge.

Keep the existing vendored `bevy_egui` patch in Talos3D:

```toml
[patch.crates-io]
bevy_egui = { path = "vendor/bevy_egui" }
```

Then temporarily add local egui overrides in Talos3D root `Cargo.toml`:

```toml
[patch.crates-io]
bevy_egui = { path = "vendor/bevy_egui" }
egui = { path = "/Users/torstenek/dev/egui/crates/egui" }
epaint = { path = "/Users/torstenek/dev/egui/crates/epaint" }
emath = { path = "/Users/torstenek/dev/egui/crates/emath" }
ecolor = { path = "/Users/torstenek/dev/egui/crates/ecolor" }
```

Important:

- the local egui checkout used for this must be on a compatible `0.33.x`
  branch
- do not point Talos3D at egui `main` unless the `bevy_egui` bridge has been
  migrated too

Then build normally:

```bash
cd /Users/torstenek/dev/talos3D/talos3d-core
cargo build
cargo test
cargo run
```

## How To Build Talos3D Against Local Bevy `main`

Do this only on `talos/bevy-main`.

In the Talos3D manifests that currently depend on `bevy = "0.18"`, replace the
versioned dependency with a path dependency to the local Bevy checkout.

The main files are:

- `/Users/torstenek/dev/talos3D/talos3d-core/Cargo.toml`
- `/Users/torstenek/dev/talos3D/talos3d-core/crates/talos3d-core/Cargo.toml`
- `/Users/torstenek/dev/talos3D/talos3d-core/crates/talos3d-terrain/Cargo.toml`
- `/Users/torstenek/dev/talos3D/talos3d-core/crates/talos3d-architectural/Cargo.toml`

Example shape:

```toml
bevy = { path = "/Users/torstenek/dev/bevy", default-features = false, features = [
    "bevy_asset",
    "bevy_core_pipeline",
    "bevy_gizmos",
    "bevy_gizmos_render",
    "mesh_picking",
    "bevy_pbr",
    "bevy_picking",
    "bevy_post_process",
    "bevy_render",
    "bevy_scene",
    "bevy_state",
    "bevy_winit",
    "bevy_window",
    "bevy_text",
    "bevy_ui",
    "tonemapping_luts",
    "webgpu",
    "default_font",
    "png",
    "jpeg",
] }
```

Then build Talos3D from the Talos3D repository, not from Bevy:

```bash
cd /Users/torstenek/dev/talos3D/talos3d-core
cargo build
cargo test
cargo run
```

Cargo will automatically use the local Bevy workspace.

## When Not To Point Talos3D At egui `main`

Do not point Talos3D directly at egui `main` when all of these are true:

- Talos3D still uses vendored `bevy_egui`
- vendored `bevy_egui` still depends on `egui 0.33`
- vendored `bevy_egui` still uses APIs removed in egui `0.34`

Current example:

- vendored `bevy_egui` still uses `ctx.run(...)`
- egui `0.34` moved to `Context::run_ui`

That means a direct dependency bump is not enough. The bridge source must be
migrated.

## What To Do When A Fix Exists Upstream But Talos3D Still Misses It

Use this sequence:

1. Check whether the fix is in egui, Bevy, or `bevy_egui`.
2. Check whether Talos3D can actually reach that fix through its current
   dependency graph.
3. If not, choose the smallest safe move:
   - backport fix onto compatible old branch
   - or migrate bridge
   - or move Talos3D integration branch forward

Current panel-growth case:

1. Fix exists in egui `main`
2. Talos3D cannot reach it because vendored `bevy_egui` pins egui `0.33`
3. therefore:
   - immediate fix path: backport onto egui `0.33.x`
   - longer-term path: migrate `bevy_egui` forward

## Minimal Weekly Maintenance Routine

If Talos3D is actively tracking newer engine work, do this once per week:

1. Refresh Bevy fork `main`
2. Refresh egui fork `main`
3. Rebuild Talos3D integration branch
4. Fix breakage in the smallest valid layer
5. Push upstreamable changes to topic branches, not to fork `main`

In commands:

```bash
cd /Users/torstenek/dev/bevy
git fetch upstream
git checkout main
git rebase upstream/main

cd /Users/torstenek/dev/egui
git fetch upstream
git checkout main
git rebase upstream/main

cd /Users/torstenek/dev/talos3D/talos3d-core
cargo build
cargo test
```

## Checklist For Humans

Before touching engine dependencies:

- read this document
- decide whether this is an immediate fix or an engine-tracking task
- decide which repository actually owns the problem
- create a topic branch if the change is not a pure fork refresh
- avoid editing fork `main` directly

## Checklist For AI Agents

If a task mentions Bevy, egui, `bevy_egui`, panels, viewport sizing, UI
regressions, or engine upgrades:

1. Read this document first.
2. Do not assume Talos3D can consume fixes from egui `main` directly.
3. Check the bridge version before proposing dependency bumps.
4. Prefer the smallest safe move:
   - local Talos fix
   - compatible backport
   - bridge migration
   - full engine tracking
5. Keep fork `main` branches clean mirrors of upstream.

## Current Decision Summary

For the current assistant sidebar panel-growth problem:

- egui `main` already contains the correct panel-state clamp
- Talos3D cannot reach that fix directly today
- the immediate path is a compatible egui backport branch
- the longer-term path is a `bevy_egui` migration plus a Bevy tracking branch

This is the structure future work should follow unless this document is updated.
