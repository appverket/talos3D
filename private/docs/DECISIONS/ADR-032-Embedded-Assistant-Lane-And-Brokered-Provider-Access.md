# ADR-032: Embedded Assistant Lane And Brokered Provider Access

**Status**: Accepted  
**Date**: 2026-04-09

## Context

Talos3D already had an external MCP surface, but not an embedded in-app
assistant. The user requirement is not just “add chat.” The assistant must:

- behave like a first-class editor surface
- manipulate the model through the same public MCP contract as external agents
- support future browser/SaaS deployment where provider access should be
  brokered by a backend
- still allow practical local fallback through direct provider API keys

This makes provider access and tool access separate concerns.

## Decision

### 1. The in-app assistant is a UI client of MCP

The assistant does not receive private editor mutation hooks. It uses a generic
MCP bridge (`mcp_list_tools` and `mcp_call_tool`) so internal and external
agents remain aligned.

### 2. Managed relay is the preferred deployment path

For browser/SaaS deployment, the preferred model is a backend-managed relay
that brokers provider access and policy. The current client expects an
OpenAI-Responses-compatible endpoint for this relay path.

### 3. Direct provider keys are local fallback, not the primary hosted model

OpenAI and Anthropic direct API-key modes are supported for local/native use
and development, but they are explicitly secondary to the managed relay model
for hosted deployments.

### 4. The assistant is presented as a right-lane work surface

The assistant opens on the right side by default, but remains an egui window so
it can still be moved, resized, or hidden without introducing a second docking
system.

## Consequences

### Positive

- the assistant remains honest to the AI-first platform contract
- hosted deployments can keep provider credentials and account policy on the
  backend
- local developers can still use direct API keys without extra infrastructure

### Negative

- managed relay verification depends on external backend availability
- provider behavior differs slightly between OpenAI Responses and Anthropic
  Messages, so the client needs provider-specific tool-call loops

## Relationship To Existing Decisions

- **ADR-008 (AI-Native Design Interface)**: the assistant is another AI client
  of the authored model
- **ADR-017 (UI Chrome Architecture)**: the assistant is part of chrome, not a
  private debug tool
- **ADR-031 (Scene Lighting As Authored, AI-Visible State)**: the assistant can
  manipulate newly-authored lighting through the same MCP surface
