# Proof Point 66: Assistant Sidebar Profiles And Streaming Transcript

**Status**: Complete  
**Date**: 2026-04-11

## Goal

The in-app assistant should behave like a proper editor sidebar, not a floating
one-off prompt box. Users need a stable transcript, a prompt composer, and a
configuration surface for cloud and local model providers that persists with
editor state.

## Target Outcome

- the assistant uses a real right sidebar lane with a restrained default width
  and user resizing
- the sidebar exposes a transcript above the prompt composer
- assistant output appears incrementally while work is running
- provider/model configurations are saved and switchable
- OpenAI, Anthropic, Gemini, and local OpenAI-compatible runtimes such as
  LM Studio and Ollama are represented in the config surface
- the sidebar model is ready for more sidebar tabs later

## Landed

- The assistant lane now has explicit `Chat` and `Configs` modes instead of a
  hardcoded inline connection form.
- Saved assistant profiles now persist with editor state and can be created,
  duplicated, deleted, and selected from the sidebar.
- Profiles now support protocol-aware routing for:
  - OpenAI Responses
  - OpenAI Chat Completions
  - Anthropic Messages
  - Gemini Generate Content
- Local OpenAI-compatible profiles now ship as first-class templates for
  LM Studio and Ollama.
- Assistant execution now streams tool activity and the final answer back into
  the transcript while the request is still pending.
- The sidebar width default and resize range were normalized to a more
  practical right-lane footprint.

## Verification

- `cargo test -p talos3d-core right_sidebar_state_round_trips_through_json --all-features`
- `cargo test -p talos3d-core assistant_preferences_round_trip_and_normalize --all-features`
- `cargo test -p talos3d-core gemini_endpoint_appends_model_path_once --all-features`
- `cargo test -p talos3d-core --all-features --no-run`
- live verification in a fresh `model-api` app instance:
  - launched Talos3D with a new `codex-assistant` instance id
  - confirmed the app booted successfully with the assistant refactor in place
  - captured a desktop screenshot while validating the live sidebar build path
