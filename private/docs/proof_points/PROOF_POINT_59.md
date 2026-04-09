# Proof Point 59: Authored Scene Lighting And MCP Light Management

**Status**: Implemented  
**Date**: 2026-04-09

## Goal

Talos3D should treat scene lighting as authored, persistent, AI-visible state
instead of hard-coded startup fixtures.

## Delivered

- directional, point, and spot lights are now authored entities with stable
  element ids and property-edit support
- ambient lighting is explicit scene state and persists with the project
- the old hard-coded startup lights were replaced by a default daylight rig
  seeded through the authored-light path
- a Lights window under View provides lightweight creation, deletion, ambient
  tuning, and selection handoff to Properties
- MCP now exposes:
  `get_lighting_scene`, `list_lights`, `create_light`, `update_light`,
  `delete_light`, `set_ambient_light`, and `restore_default_light_rig`
- delete-dependency traversal no longer panics when unrelated feature
  component types have never been instantiated in the current world

## Why It Matters

This proof point makes visual staging part of the same public authored model as
geometry, materials, and definitions. That is necessary for AI-first editing,
repeatable screenshots, browser-safe persistence, and future hosted workflows.

## Verification

- `cargo test -p talos3d-core --all-features --no-run`
- `cargo test -p talos3d-core lighting_round_trip_and_restore_default_rig --all-features`
- `cargo run --all-features -- --instance-id lighting-verify --model-api-port 24851`
- live MCP verification created a sphere, applied the bundled MAIBEC material,
  restored the daylight rig, added spot/point lights, saved
  `private/screenshots/lighting-mcp-verification.png`, and saved
  `private/models/lighting-mcp-verification.talos3d`
