# Proof Point 71: MaterialSpec On The Curation Substrate

**Status**: Planned  
**Date**: 2026-04-20

## Goal

Land `MaterialSpec` as the first curated construction-material kind on the
curation substrate, with an initial typed API surface and pack/dependency
integration.

## Scope

- add `MaterialSpecBody`, `MaterialSpec`, and `MaterialSpecRegistry`
- integrate the registry into `CurationPlugin`
- expose an initial typed API surface for inspection and draft lifecycle
- model the soft `default_rendering_hint` without turning it into a hard
  dependency

## Why It Matters

This is the architectural seam that keeps render appearance and governed
construction-material semantics separate while allowing them to cooperate.

## Expected Verification

- registry round-trip/unit tests
- publication-floor tests against `CurationMeta`
- API tests for list/get/save/publish behavior
