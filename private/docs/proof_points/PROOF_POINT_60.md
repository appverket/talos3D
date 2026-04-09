# Proof Point 60: Embedded Assistant Chat Lane With MCP Bridge

**Status**: Implemented  
**Date**: 2026-04-09

## Goal

Talos3D should provide an in-app assistant surface that can manipulate the
model through MCP while remaining compatible with local and hosted deployment
models.

## Delivered

- a right-side Assistant egui window opens by default
- the assistant uses the existing MCP surface through generic tool bridge calls
  rather than private editor hooks
- provider modes include:
  - managed relay (preferred)
  - direct OpenAI Responses fallback
  - direct Anthropic Messages fallback
- provider credentials stay in runtime state and are not persisted into project
  files
- the property panel shifts left when the assistant lane is visible so the
  default layout remains workable
- unit tests cover OpenAI function-call and response-text parsing helpers

## Why It Matters

This proof point turns the editor itself into a first-class AI client without
breaking the platform boundary between UI and public automation contracts.

## Verification

- `cargo test -p talos3d-core assistant_chat --all-features -- --nocapture`
- `cargo test -p talos3d-core --all-features --no-run`
- `cargo run --all-features -- --instance-id assistant-verify --model-api-port 24852`
- runtime integration verified that the app starts cleanly with the assistant
  lane and local MCP endpoint wired in by default
- live provider round-trip was not possible in this environment because
  `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and `TALOS3D_ASSISTANT_RELAY_URL` were
  all unset
