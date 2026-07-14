# Grok provider — spec

> STV artifact (zbrain rules/STV.md). The companion `trace.md` is the source of truth for
> execution paths; this file fixes scope, design decisions, and the File Map.
> Reference implementation analyzed: router-for-me/CLIProxyAPI (Go) — its xAI executor,
> auth, and model registry. All CLIProxyAPI file:line references are to that repo @ HEAD
> 2026-07-14 (shallow clone).

## Goal

Add **Grok (xAI) as a third backend group** to llmux — alongside `claude` and `codex` —
serving Anthropic Messages API clients (Claude Code) from a Grok subscription account via
xAI's Responses API, with registration, stats, and live model/effort switching exposed in
both the CLI and llmux-islands. Refactor so codex and grok share one Responses-API core
with thin per-provider adapters.

## Why this is safe to build on the codex skeleton (evidence)

- xAI's chat surface **is** the OpenAI Responses API: CLIProxyAPI's xAI executor posts to
  `{base}/responses` (`internal/runtime/executor/xai_executor.go:151,616`), and its xAI
  "thinking" applier literally embeds the codex applier
  (`internal/thinking/provider/xai/apply.go:12-16`).
- llmux's codex provider is already a Messages↔Responses translator ported from
  CLIProxyAPI (`src/provider/codex.rs:1-9`), with per-request model pass-through
  (`resolve_upstream_model`, codex.rs:~280) and per-request effort from the Claude Agent
  SDK's `output_config.effort` (codex.rs:319-349).

## Verified upstream facts (grok wire contract)

| Fact | Value | Evidence |
|---|---|---|
| OAuth | OIDC discovery + RFC 8628 device-code flow | CLIProxyAPI `internal/auth/xai/xai.go:66-176` |
| Discovery URL | `https://auth.x.ai/.well-known/openid-configuration` | `internal/auth/xai/types.go:17` |
| Client ID | `b1a00492-073a-47ea-816f-4c329264a828` (public Grok CLI client) | types.go:19 |
| Scope | `openid profile email offline_access grok-cli:access api:access` | types.go:21 |
| Refresh | `refresh_token` grant at discovered token endpoint, 5-min lead | xai.go:331-368, types.go:32 |
| Chat base URL (subscription/OAuth) | `https://cli-chat-proxy.grok.com/v1` | types.go:13 |
| Chat base URL (API key — **non-goal v1**) | `https://api.x.ai/v1` | types.go:10 |
| Chat endpoint | `POST {base}/responses`, SSE | xai_executor.go:151,616 |
| Identity headers (cli-chat-proxy only) | `X-XAI-Token-Auth: xai-grok-cli`, `x-grok-client-version: 0.2.93`, `User-Agent: xai-grok-workspace/0.2.93` | xai_executor.go:66-69,1104-1111 |
| Conversation header | `x-grok-conv-id: <session id>` | xai_executor.go:1085 |
| Effort | `reasoning: {effort}` only for models with thinking levels; stripped otherwise | xai_executor.go:1206-1211 |
| grok-4.5 | ctx 500K, max_out 65536, thinking `low/medium/high`, zero **not** allowed | registry models.json:2425-2443 |
| grok-build-0.1 | fast coding model, ctx 256K, no thinking levels | models.json:2413-2423 |
| Quota exhaustion | HTTP 429 body `code`/`error` contains `free-usage-exhausted` → 24h rolling window | xai_executor.go:2521-2545 |
| No usage headers/endpoint | grok exposes no `x-codex-*`-style quota headers and no `/api/oauth/usage` equivalent (none found in CLIProxyAPI) | xai_executor.go (absence) |
| Pricing (API list price, for cost display) | in $2.00/M, out $6.00/M, cached-in $0.50/M, no cache-write charge | docs.x.ai grok-4.5 (web, 2026-07-14) |
| Effort default upstream | `high` when unspecified | docs.x.ai reasoning (web, 2026-07-14) |

## Requirements → design

### R1. Grok provider + `grok` backend group

- `BackendGroup` gains a third variant `Grok` (`src/routing.rs:22-26`); `from_kind`:
  credential kind `"grok"` → `Grok` (routing.rs:32-37); `as_str`/`from_label` extended.
- Builtin routing rule for the grok group: model prefix `grok` (mirrors
  builtin claude/codex rules, routing.rs:105-125). Config `routing.grok_models`
  (additive, `#[serde(default)]`) — empty keeps builtins, non-empty replaces
  (Classifier::from_config, routing.rs:151-177).
- New credential variant `AccountCredential::Grok { access_token, refresh_token,
  expires_at_ms, token_endpoint, last_refresh_ms }`. Account name convention
  `grok:{email}` (email from id_token JWT `email` claim; fallback `sub`, then epoch-ms —
  mirrors CLIProxyAPI `internal/auth/xai/token.go:71-81`).
- `GrokProvider` (new `src/provider/grok.rs`): thin adapter over the shared Responses
  core (R5). `GrokShape { model: "grok-4.5" (default), client_model: Option, effort:
  Option }` — **no `fast`** (xAI has no service tier). Live-mutable behind `RwLock`
  exactly like `CodexShape` (codex.rs:96-125).
- Request shape differences vs codex (adapter knobs, not forks):
  - endpoint = `config.grok.upstream` (default `https://cli-chat-proxy.grok.com/v1`) +
    `/responses`;
  - identity headers `X-XAI-Token-Auth` / `x-grok-client-version` / `User-Agent`
    attached only when upstream is the official cli-chat-proxy host;
  - session header `x-grok-conv-id: {session_id}` (codex sends `session_id` +
    `prompt_cache_key` body field — grok keeps `prompt_cache_key` too, harmless);
  - `service_tier` never sent; `include: ["reasoning.encrypted_content"]` **not** sent
    (OpenAI-specific; CLIProxyAPI does not send it for xAI);
  - effort values valid for grok: `low|medium|high`. Per-request
    `output_config.effort` resolution mirrors codex (codex.rs:319-349) with grok clamp:
    `none|minimal` → `low`, `xhigh|max|ultra` → `high` (grok-4.5 cannot disable
    reasoning — models.json:2436 `zero_allowed: false`).
- Error handling: HTTP 429 whose body matches `free-usage-exhausted` (or "included free
  usage") → account parked with explicit `retry_after = 24h` (forward.rs retry_after
  path, forward.rs:1347); generic 429 → existing backoff.

### R2. Registration in CLI and islands

- CLI: `llmux login --grok` — device-code flow: discovery → device code → print
  `verification_uri_complete` + `user_code`, attempt `open` of the URL, poll token
  endpoint (interval/slow_down per RFC 8628, mirrors xai.go:196-328) → upsert account
  `grok:{email}`. No localhost callback port needed (unlike codex PKCE).
- Islands: daemon-run login (`POST /llmux/login/start {"provider":"grok"}`) —
  `LoginProvider` gains `Grok` (aliases `grok|xai|x-ai`, proxy/login.rs:22-47).
  Device flow has no browser callback; the daemon must surface the verification URL to
  the GUI: `GET /llmux/login/status` response gains optional `verification_uri` (the
  `_complete` variant) + `user_code` while pending. Islands opens the URL and keeps
  polling until Done/Error. (Additive fields — older clients ignore them.)

### R3. Stats & usage display (CLI + islands)

- Token/cost stats: the shared Responses SSE core already yields `StreamUsage`; the
  dashboard tracks per-(group, model). Add pricing entry `GROK_4_5 {input 2.0, output
  6.0, cache_read 0.5, cache_creation 0.0}` + `group == "grok"` unknown-model fallback
  (src/pricing.rs builtin table, pricing.rs:58-100).
- Quota windows: grok has **no** active usage endpoint and **no** passive quota headers
  → 5h/7d gauges stay empty ("—"), same rendering path as an accountless slot. A
  `free-usage-exhausted` park shows the existing cooldown/reset countdown UI.
- Surfaces: `llmux status` (client+server), TUI account table + activity log (model +
  effort recorded via grok `effective_request_meta` mirror), `/llmux/status` JSON
  (`group: "grok"`), islands: `UsageProvider.grok` + provider icon (new `grok` asset;
  falls back to text glyph when the asset is missing), closed-island in-flight counts
  (`[claude]{n} [codex]{m} [grok]{k}` — IslandUsageModel.swift:23-27,176-195), usage
  tiles, demo/snapshot fixtures.

### R4. Model switching from Claude Code (gpt-5.6 parity)

- `/model grok-4.5` in Claude Code → Messages body `model: "grok-4.5"` → Classifier
  routes to the Grok group (prefix rule) → scheduler picks a grok account → provider
  passes the **requested slug through verbatim** when it is grok-shaped
  (`grok-` prefix), else rewrites to `shape.model` — the exact
  `resolve_upstream_model` contract codex has on master (codex.rs:~280).
- Effort: Claude Code's effort setting arrives as `output_config.effort` and wins over
  the configured shape effort, clamped to grok's `low|medium|high` (R1). So
  `/model grok-4.5` + effort high ≙ how `gpt-5.6-sol` is used today (live config
  `codex.default_model = "gpt-5.6-sol"`, `~/.config/llmux.json`).
- Live shape control: `POST /llmux/grok {default_model?, reasoning_effort?}` — same
  contract as `POST /llmux/codex` (server.rs:1089-1140): partial update, apply live,
  persist via `config::update_path`. Islands settings pane gains a grok row (model +
  effort; no fast toggle).
- Config: new `config.grok` section `{upstream, default_model, reasoning_effort,
  client_model?, trace}` (defaults: cli-chat-proxy URL, `grok-4.5`, null, null, false).

### R5. Refactor: one Responses core, thin codex/grok adapters

- New `src/provider/responses.rs`: the provider-agnostic Messages↔Responses machinery
  moved **verbatim where possible** from codex.rs — `messages_to_input`,
  `build_instructions`, `tools_to_functions`, response/SSE conversion
  (Responses SSE → Anthropic SSE), usage extraction, and the request builder
  parameterized by a `ResponsesFlavor`:
  ```rust
  pub struct ResponsesFlavor {
      pub provider: &'static str,          // "codex" | "grok" (logs, errors)
      pub valid_efforts: &'static [&'static str],
      pub clamp_effort: fn(&str, &str) -> String,   // (effort, model) -> effort
      pub model_passthrough: fn(&str) -> bool,      // requested slug accepted?
      pub send_encrypted_reasoning_include: bool,   // codex true, grok false
      pub supports_service_tier: bool,              // codex true, grok false
  }
  ```
  (Exact shape may be simplified during implementation — a struct of data +
  fn pointers keeps it dyn-free and testable; the trace, not this sketch, is
  normative for behavior.)
- `codex.rs` shrinks to: `CodexShape`, OAuth/refresh glue, header assembly, flavor
  definition, `effective_request_meta`. `grok.rs` is the same ~small file for grok.
- `forward.rs` binary `is_codex` dispatch (forward.rs:726-770) becomes a three-way
  group match; `on_empty_group: "fallback"` (currently flips Claude↔Codex,
  forward.rs:1153-1154) becomes: fallback tries the remaining groups in fixed order
  `Claude → Codex → Grok`, first group with ≥1 configured account wins.
- Existing codex unit tests move with the code they test; codex behavior is
  **bit-identical** (gate: full existing test suite green untouched except imports).

## Non-goals (v1)

- API-key grok accounts (`api.x.ai` `using_api` path) — subscription OAuth only.
- x_search auto-injection (CLIProxyAPI injects `{"type":"x_search"}` always,
  xai_executor.go:77-78) — llmux does not inject tools the client didn't send.
- Grok media (image/video), websockets executor, `/responses/compact`, composer models.
- xAI reasoning replay cache (CLIProxyAPI `internal/cache/xai_reasoning_replay_cache.go`)
  — Claude Code resends full conversation; llmux's codex path already works multi-turn
  without a replay cache. Revisit only if live receipt shows reasoning-continuity loss.
- Importing grok-cli's own credential store.
- Active quota polling for grok (no known endpoint).

## Risks / open items

1. **cli-chat-proxy client-version pinning**: header value `0.2.93` may age; kept in one
   const with a comment, config-overridable via `grok.upstream` remaining functional
   even if identity headers change requirements. (CLIProxyAPI pins the same way.)
2. **Wire-shape unknowns** (does cli-chat-proxy accept `store:false`,
   `parallel_tool_calls`, `prompt_cache_key`?): CLIProxyAPI sends translator-normalized
   Responses bodies without stripping these; live receipt (first real request) is the
   gate. If rejected, adapter strips per flavor flag.
3. **429 body shape drift**: matching is substring-based on `code`/`error` fields,
   mirroring CLIProxyAPI exactly.

## File Map (complete modification surface)

Rust (`llmux`):
- `src/routing.rs` — `BackendGroup::Grok`, builtin grok rules, classifier, label parse
- `src/config/schema.rs` — `GrokConfig`, `RoutingConfig.grok_models`
- `src/config/mod.rs` — `AccountCredential::Grok`, persistence, demo alias for `grok:`
- `src/provider/responses.rs` — NEW shared core (moved from codex.rs)
- `src/provider/codex.rs` — reduced to adapter
- `src/provider/grok.rs` — NEW adapter
- `src/provider/mod.rs` — wiring
- `src/auth/grok.rs` — NEW: discovery, device flow, poll, refresh, JWT identity
- `src/auth/mod.rs` — export
- `src/cli/login.rs`, `src/cli/mod.rs` — `--grok`
- `src/proxy/login.rs` — `LoginProvider::Grok`, device-flow phase w/ verification URI
- `src/proxy/server.rs` — `AppState.grok`, `POST /llmux/grok`, login status fields,
  status JSON
- `src/proxy/forward.rs` — 3-way dispatch, `AccountCredential::Grok` refresh arm,
  free-usage-exhausted retry_after, on_empty_group order
- `src/scheduler/mod.rs` / `select.rs` — group plumbing (mostly type-driven)
- `src/pricing.rs` — grok prices + group fallback
- `src/tui/*` — group label/color where "codex" is special-cased
- `tests/` — contract tests (see trace.md)

Swift (`llmux-islands/LlmuxIslands`):
- `Llmux/LlmuxStatus.swift` — type/group `grok`
- `Llmux/IslandUsageModel.swift` — grok in-flight, provider(of:)
- `Llmux/LlmuxClient.swift` — grok config setter, login provider string
- `Llmux/LlmuxDashboard.swift` — grok settings row
- `Dashboard/UsageModelTypes.swift`, `UsageTiles.swift`, `DashboardAnalytics.swift`
- `UI/Components/UsageProviderIcon.swift` — grok icon + fallback
- `UI/Views/NotchClosedLabelView.swift`, `IslandUsageView.swift` — third group + login
- `Core/SnapshotMode.swift`, `Core/DemoMode.swift` — fixtures
