# Engine And UI Dependency Workflow

## Purpose

This document defines the supported way to change Bevy, `bevy_egui`, or egui
dependencies used by Talos3D. The goals are reproducible builds, a legible
dependency graph, and fixes that land in the layer that owns them.

## Current Policy

Talos3D uses registry-resolved `bevy_egui` and egui versions compatible with
its Bevy version. The historical vendored egui bridge/backport is retired.

Do not:

- add `vendor/bevy_egui` or `vendor/egui-*` trees;
- add local egui or `bevy_egui` entries under `[patch.crates-io]`;
- pin unrelated app launchers to an older Bevy/egui compatibility line;
- fix a Talos3D behavior bug by modifying a dependency when the defect belongs
  in Talos3D.

The normal dependency shape is:

```text
Talos3D app
  -> talos3d-core
  -> compatible released Bevy + bevy_egui + egui graph
```

All app compositions should use the same Bevy compatibility line as
`crates/talos3d-core/Cargo.toml` unless a deliberate migration branch records
why they differ.

## Decide Which Layer Owns The Problem

- Talos3D interaction, layout, command, or product semantics belong in
  Talos3D.
- Bevy scheduling, rendering, ECS, windowing, or engine defects belong in
  Bevy.
- egui widget/layout defects belong in egui.
- Bevy-to-egui integration defects belong in `bevy_egui`.

When the defect belongs upstream, treat the dependency as repairable
infrastructure: prepare the smallest regression-covered fix and propose it
upstream. Do not preserve a permanent Talos3D-only vendor fork.

## Supported Working Modes

### Released dependencies

This is the default. Update the compatible registry versions and lockfile,
then verify every Talos3D app composition that consumes them.

```bash
cd /path/to/talos3d-core
cargo update
cargo check -p talos3d-core
cargo test -p talos3d-core
```

Run launcher checks from their own manifests because `app/` and `app-core/`
are separate Cargo workspaces.

### Temporary upstream source pin

Use a source revision only when a required upstream fix is merged but no
compatible release exists yet. Record:

- upstream repository and commit;
- issue or pull-request link;
- license/provenance;
- affected app compositions;
- removal condition.

Prefer a Git revision pin in the relevant manifest. Keep it on a topic branch,
verify it in CI, and remove it as soon as a compatible release contains the
fix. A source pin is not permission to restore an egui vendor tree.

### Local Bevy tracking

Use a dedicated `talos/bevy-main` Talos3D integration branch when validating
Bevy `main` or preparing an upstream engine fix. Local Bevy path dependencies
belong only on that integration branch.

Recommended branches:

- Talos3D `main`: normal development, released dependencies;
- Talos3D `talos/bevy-main`: engine-integration work;
- Bevy fork `main`: clean upstream mirror;
- Bevy fork `talos/<topic>`: upstreamable engine change.

Example temporary shape:

```toml
bevy = { path = "/path/to/bevy", default-features = false, features = [
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
    "default_font",
    "png",
    "jpeg",
] }
```

Build Talos3D from the Talos3D repository; Cargo will resolve the local Bevy
workspace through that path.

## Verification

For any dependency-line change:

1. Inspect `cargo tree` for duplicate incompatible Bevy, egui, or
   `bevy_egui` versions.
2. Regenerate each affected lockfile.
3. Compile `talos3d-core` with and without optional features touched by the
   change.
4. Compile the exact app compositions users run.
5. Run focused UI/input tests and visually inspect the affected behavior.
6. Confirm no `vendor/bevy_egui`, `vendor/egui-*`, or local egui patch was
   introduced.
7. Record upstream provenance and the removal condition for any temporary pin.

Useful checks:

```bash
rg 'vendor/(bevy_egui|egui)|patch.crates-io' .
cargo tree -d
cargo check -p talos3d-core
cargo check -p talos3d-core --features model-api
cargo check --manifest-path app-core/Cargo.toml --features model-api
```

## Maintenance

Normal maintenance follows released versions, not continuously refreshed local
egui forks. When actively tracking Bevy `main`:

1. refresh the clean Bevy fork mirror;
2. rebase the Bevy topic and Talos3D integration branches;
3. rebuild affected Talos3D app compositions;
4. keep fixes in the smallest owning layer;
5. upstream dependency fixes and return Talos3D to released dependencies.

## Agent Checklist

When a task mentions Bevy, egui, `bevy_egui`, panels, viewport sizing, input,
or engine upgrades:

1. Read this document.
2. Inspect the actual dependency graph and lockfiles.
3. Do not reintroduce egui vendoring or local egui patches.
4. Identify the owning layer with evidence.
5. Use the smallest safe released update or a temporary reviewed source pin.
6. Verify both the shared crate and the exact product launcher.
7. Preserve the shared human/agent semantic edit path for interaction changes.

## Current Decision Summary

The older sidebar workaround that required a vendored egui bridge is resolved
by the current dependency line. Talos3D now uses registry dependencies and no
egui vendor tree. Future integration defects should be fixed upstream or
carried briefly as reviewed source pins, never as a revived permanent vendor
fork.

## Bevy 0.19.1 Audit

The 2026-08-13 patch release is the first released dependency baseline after
Talos3D's 0.19 release-candidate work. Direct Bevy requirements use `0.19.1`,
and Bevy/egui compositions use released `bevy_egui` `0.40.1`. Do not restore an
RC requirement in a downstream launcher: it can silently leave one lockfile on
the release candidate while another resolves the stable engine.

The upstream fixes change which engine workarounds Talos3D should avoid:

- Keep SSAO available on WebGPU. Bevy 0.19.1 fixes both Chrome's invalid depth
  `textureGather` call and an SSAO normalization error; do not add a
  browser-specific SSAO disable for those defects.
- Keep light and preview-camera isolation expressed with Bevy `RenderLayers`.
  The patch repairs extracted light/shadow layers and camera-despawn shadow
  handling; do not duplicate render extraction or defer camera cleanup locally.
- Continue updating reusable `Assets<Mesh>` handles for regenerated model and
  terrain meshes. The patch repairs mesh reallocation bookkeeping; do not churn
  handles as a defensive workaround.
- Transparent WebGPU batching, material binding-array checks, text layout, UI
  camera propagation, and macOS cursor-grab fallback are upstream-owned fixes.
  Talos3D should consume them through the released engine rather than carry
  platform branches for the fixed behavior.

Bevy's 0.19 `TransformGizmoPlugin` is deliberately not substituted for
Talos3D's `HandlesPlugin`. The upstream gizmo mutates one focused entity's
`Transform` during drag. Talos3D transforms authored snapshots and aggregates,
runs semantic-plan modifiers, and commits through command/history. Replacing
that path would create a second movement authority. Reconsider the upstream
gizmo only if its interaction deltas can feed the shared Talos3D edit plan (or
Bevy gains an equivalent interception contract) with presentation-path tests.

Likewise, BSN scenes, Bevy app settings, and runtime asset saving are useful
engine facilities but do not replace Talos3D authored models, Definition
libraries, command history, or persistence. They may be adopted for
presentation-only composition where they do not introduce a second semantic
lifecycle.
