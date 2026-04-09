# Assistant Chat

## Purpose

Talos3D includes an embedded assistant chat lane that appears on the right side
of the editor by default. It is implemented as an egui window, but it behaves
like a persistent right-lane assistant surface rather than a transient popup.

The assistant is AI-first in the same way as the rest of Talos3D:

- it uses the public MCP surface for model work
- it does not get private editor-only mutation hooks
- it can inspect and manipulate the same authored model concepts that external
  agents use

## Provider Modes

The assistant supports three connection modes:

### 1. Managed Relay

Preferred for browser and SaaS deployments.

- configure a relay URL, bearer token, and model
- the current client implementation expects an OpenAI Responses-compatible
  endpoint
- this is the recommended path when a backend should broker provider access,
  fixed-plan account access, policy enforcement, logging, or billing

### 2. OpenAI

Direct API-key fallback for local/native use.

- uses the OpenAI Responses API
- suitable when a local operator wants direct provider access without a relay

### 3. Anthropic

Direct API-key fallback for local/native use.

- uses the Anthropic Messages API with tool use
- suitable when a local operator wants direct provider access without a relay

## MCP Bridge Model

The assistant talks to Talos3D through MCP, using two generic internal tools:

- `mcp_list_tools`
- `mcp_call_tool`

That means the assistant can reach the same modeling, lighting, material,
definition, view, persistence, and screenshot operations that external MCP
clients can reach, without hard-coding a parallel internal API surface.

If no MCP URL is configured manually, the assistant defaults to the local
Talos3D MCP endpoint when the app is running with `model-api` enabled.

## Security And Deployment

Secrets are not persisted to project files.

Deployment guidance:

- local desktop usage may use direct API keys
- managed or browser-hosted deployments should prefer the relay path
- browser/SaaS deployments should not rely on shipping direct vendor API keys
  to the browser client

## Environment Variables

Optional startup configuration:

- `TALOS3D_ASSISTANT_MCP_URL`
- `TALOS3D_ASSISTANT_RELAY_URL`
- `TALOS3D_ASSISTANT_RELAY_TOKEN`
- `TALOS3D_ASSISTANT_RELAY_MODEL`
- `OPENAI_API_KEY`
- `TALOS3D_ASSISTANT_OPENAI_MODEL`
- `ANTHROPIC_API_KEY`
- `TALOS3D_ASSISTANT_ANTHROPIC_MODEL`

## Current Limitations

- the managed relay contract is currently OpenAI Responses-compatible rather
  than a Talos3D-specific protocol
- live provider verification still depends on configured relay credentials or
  vendor API keys in the runtime environment
