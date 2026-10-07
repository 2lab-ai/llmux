# Codex frontend
Status: in-progress
Date: 2026-10-07

## Problem and contract

`llmux run` currently launches Claude Code against Anthropic Messages. Add
`llmux run --codex` and an OpenAI Responses ingress, preserving the existing
Claude launch as the default. The original scope is in [SSOT](codex-frontend/ssot.md).

## Acceptance scenarios

1. Run `llmux run --codex -- exec ...`: daemon readiness/version rules, remote
   resolution, passthrough arguments, cwd and child exit status match `run`.
   The actual child uses a session-scoped llmux provider, no persistent Codex config edits.
2. Select a Codex model: POST `/v1/responses` authenticates at the existing data-plane
   gate, leases/refreshes the selected pool account, and preserves native Responses
   input, reasoning and tool wire data through the subscription gateway.
3. Select a Claude model: the same routing/scheduler invokes Claude Agent SDK,
   returns Responses text/tool events, accepts tool outputs on the following turn,
   and never lets the SDK execute the client's tools itself or recurse into llmux.
4. Stream/non-stream, request validation, provider failures and client cancellation
   terminate honestly. Usage records the account, backend model, tenant and endpoint.
5. `/model` exposes llmux catalog/defaults with `--no-model-picker` parity; user
   model/effort overrides take precedence. Remote keys are supplied only to the
   chosen llmux endpoint, not inherited OpenAI credentials.
6. OpenAI activity rows have dark gray backgrounds on local/attached dashboards;
   Anthropic rows retain the existing background, irrespective of backend.
7. `just check`, independent review, CI, published preview, and actual installed
   `llmux run --codex` text/tool turns using both backend families are evidenced.

## Architecture / scope

Reuse `proxy::forward` scheduler/retry/refresh/lease and observability. A Responses
boundary validates the OpenAI envelope and provides normalized Messages metadata;
Codex upstream stays native, avoiding lossy reasoning/tool conversion. Claude SDK
is a subprocess transport using the selected credential and isolated settings.
A Responses output adapter wraps the Claude SDK Messages stream. Model discovery
is shared; only client-specific catalog serialization and provider flags differ.
No public vendor pay-as-you-go account system or WebSocket transport is implied.
Other catalog groups retain their existing supported Messages translation behavior.

See [vertical traces](codex-frontend/trace.md) and [loop](codex-frontend/loop.md).
