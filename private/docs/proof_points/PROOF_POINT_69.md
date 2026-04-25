# Proof Point 69: TextureAsset And MaterialDef Boundary

**Status**: Planned  
**Date**: 2026-04-20

## Goal

Refactor the render-material layer so texture payloads become first-class shared
assets and `MaterialDef` can link to curated construction semantics without
changing its role as the canonical internal rendering-material model.

## Scope

- add `TextureAsset` / `TextureRegistry`
- add `MaterialDef.spec_ref`
- preserve backward-compatible material persistence
- keep existing materials UI and MCP tools working

## Why It Matters

This separates shared media infrastructure from render-material appearance and
creates the seam needed for later `MaterialSpec` work without replacing
`MaterialDef` with glTF JSON or another external schema.

## Expected Verification

- unit tests for `TextureRegistry` de-duplication and id stability
- persistence round-trip tests for legacy embedded/asset-path textures
- material-info serialization tests showing backward-compatible MCP payloads
