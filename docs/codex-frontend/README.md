# Codex frontend

Run the installed Codex CLI through llmux:

```sh
llmux run --codex
llmux run --codex -- exec -m sol 'Explain this repository'
llmux run --codex -- exec -m haiku 'Run the tests and explain failures'
llmux --remote server:3456 run --codex
```

The default `llmux run` still launches Claude Code. Both launchers share daemon
readiness/version handling, `--force`, remote configuration, account scheduling,
credential refresh, quota failover, tenant attribution and usage history. Arguments
after `--` go to the selected client; its working directory and exit code are retained.
Install Codex CLI on the client machine. For Claude models through Codex, install
Node.js 18+ and npm on the **daemon host**. On the first such request llmux installs
the pinned official Claude Agent SDK from its embedded lockfile into an isolated
cache. A failed installation returns an actionable error and may be retried.

The launcher supplies a session-only Codex provider pointing to `/v1/responses`.
It does not edit `~/.codex/config.toml` or replace Codex login credentials. The
provider uses the configured llmux proxy/remote key through a dedicated environment
variable. A remote daemon needs a configured `remote.api_key` or client key.

## Models and tools

The `/model` picker uses llmux's catalog, effort menus and context windows. Native
Codex coding instructions are reused from the local Codex model cache when present;
a self-contained coding prompt is used otherwise. Claude models use direct client
tools. Catalog aliases such as `opus`, `haiku`, `sol` and `astra` also work with `-m`.
`--no-model-picker` skips the catalog fetch/injection; an explicit
`-c model_catalog_json=...` keeps the user's catalog. User model/config options
follow the injected defaults and win.

Codex models retain their native Responses history, encrypted reasoning, custom
and namespaced tools. Claude models run through the actual Claude Agent SDK with
the selected llmux account. Caller tools are exposed to the model but **execute
in Codex's client sandbox**. Their IDs, arguments and results survive subsequent
turns. The SDK uses a private cwd/HOME with project settings, plugins, skills,
hooks and unrelated inherited credentials disabled. SDK-owned web search runs
inside the SDK when the caller enables server web search. SDK identity and
continuation reminders are additional to the caller's conversation.

Claude's SDK cannot enforce `parallel_tool_calls:false`, non-auto/non-none
`tool_choice`, `temperature`, `top_p` or an explicit non-default `service_tier`; these are explicit HTTP 400 errors.
`exec --output-schema` maps `text.format` JSON Schema to the SDK structured-output
API; the validated result returns as JSON text. A supplied output cap applies per
SDK model call, so internal schema/search rounds report `max_output_tokens_semantics`
rather than claiming a total Responses budget. Claude effort `none` disables
thinking; `minimal` maps to `low` and `ultra` to `max`, with a compatibility warning
(and strict-policy rejection) for the latter two mappings.
Foreign encrypted reasoning is omitted when switching to Claude and reported in
compatibility headers. `text.verbosity` has no SDK equivalent and is reported as
an omission. For Codex subscription accounts `max_output_tokens` is omitted and
reported, as with `max_tokens` on the Claude Code frontend; it is not a token cap.
`X-Llmux-Compatibility: strict` rejects these reported losses before refresh or
upstream traffic. See [provider compatibility](../provider-compatibility.md).

## HTTP and activity

- `POST /v1/responses` (also `/responses`): text, images, tools, streaming or JSON.
- `GET /v1/models`: OpenAI-shaped model list. `/llmux/models` retains llmux metadata.
- Authentication: `Authorization: Bearer <llmux-key>` or `x-api-key`. Conflicting,
  malformed and unknown explicit OpenAI credentials are rejected, including locally.
- Send the full conversation with `store:false`; stored responses,
  `previous_response_id`, `conversation` and background jobs are not implemented.
- Stream errors end in `response.failed`; nonstream failures return HTTP 502.
  Disconnecting releases the account lease and terminates SDK processes.

OpenAI endpoint activity uses a dark gray row background, even when Claude served
the request. Anthropic endpoint requests retain their normal background, even
when Codex served them. This origin persists through history and attached/native
clients; see [activity rendering](activity.md).

Raw I/O records retain the original Responses input and returned Responses output.
The upstream leg on the Claude route is explicitly labeled `claude-agent-sdk` and
records the Messages transport passed to/from the SDK, not a claim about the SDK's
private HTTP request. Captures remain opt-in and use the existing size/redaction rules.

Sources: `src/cli/run.rs`, `src/proxy/responses.rs`, `src/proxy/forward.rs`,
`src/provider/claude_sdk.rs` and [bridge contracts](../../bridge/README.md),
verified 2026-10-07. Native model schema and provider configuration were checked
against Codex CLI 0.160.1; SDK is pinned to 0.3.292.

Codex treats the custom `llmux` provider as lacking remote compaction support,
so context compaction uses ordinary Responses summarization requests. No separate
`/responses/compact` endpoint or remote-compaction override is configured.
