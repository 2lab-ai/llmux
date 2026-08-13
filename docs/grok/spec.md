# Grok provider — spec

> STV artifact (zbrain rules/STV.md). The companion `trace.md` is the source of truth for
> execution paths; this file fixes scope, design decisions, and the File Map.
> Reference implementation analyzed: router-for-me/CLIProxyAPI (Go) — its xAI executor,
> auth, and model registry. All CLIProxyAPI file:line references are to that repo @ HEAD
> 2026-07-14 (shallow clone).

## Goal

Add **Grok (xAI) as a third backend group** to llmux — alongside `claude` and `codex` —
serving Anthropic Messages API clients (Claude Code) from a Grok subscription account via
xAI's Responses API. Registration and stats are exposed in both the CLI and
llmux-islands; the core model-switch requirement (R4) is Claude Code `/model grok-4.5`
routing. A live daemon-side model/effort override is exposed via the **daemon HTTP API**
(`POST /llmux/grok`); interactive callers for it (the TUI `f/m/e` control, an islands
settings row) are v1-descoped (§R4). Refactor so codex and grok share one Responses-API
core with thin per-provider adapters.

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
| grok-4.6 | ctx 500K, max_out (not stated by /v1/models), thinking `low/medium/high/xhigh`, zero **not** allowed | live cli-chat-proxy /v1/models 2026-08-13 |
| grok-4.5 | ctx 500K, max_out 65536, thinking `low/medium/high`, zero **not** allowed | registry models.json:2425-2443 |
| grok-build-0.1 | fast coding model, ctx 256K, no thinking levels | models.json:2413-2423 |
| Quota exhaustion | HTTP 429 body `code`/`error` contains `free-usage-exhausted` → 24h rolling window | xai_executor.go:2521-2545 |
| No usage endpoint; passive headers DO exist | no `/api/oauth/usage` equivalent (probed live: /v1/{usage,rate_limits,quota,me} 404; grok.com/rest/rate-limits 403 for OAuth2 tokens; absent from CLIProxyAPI and official grok CLI 0.2.101 binary). **Correction 2026-07-14**: 200 responses DO carry kind-first `x-ratelimit-{limit,remaining}-{requests,tokens}` headers (live capture: 900 req / 15M tok, no reset header) — the original "no passive quota headers" claim was wrong | live probes 2026-07-14 (llmux-evidence/2026-07-14-grok-group-usage) |
| Pricing (API list price, for cost display) | in $2.00/M, out $6.00/M, cached-in $0.50/M, no cache-write charge | docs.x.ai grok-4.5 (web, 2026-07-14) |
| Pricing — grok-4.6 (API list price) | in $2.00/M, out $6.00/M, cached-in carried from grok-4.5 ($0.50/M; not listed) | docs.x.ai grok-4.6 (web, 2026-08-13) |
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
- **OAuth token contract** (normative; C5/C8): device-code poll form =
  `grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code=…&client_id=…`
  (client_id REQUIRED — xai.go:258-262); refresh form =
  `grant_type=refresh_token&client_id=…&refresh_token=…` (xai.go:361-368). Rotation:
  a refresh response's `refresh_token`, when present, REPLACES the stored one; when
  omitted, the stored one is KEPT (llmux codex parity, forward.rs:1494-1499).
  `access_token`/`expires_at_ms`/`last_refresh_ms` swap atomically as one credential
  value (pool swap + config persist, existing `refresh_credential` shape). 401 or
  `invalid_grant` on refresh → `RefreshOutcome::Permanent` (re-login required, account
  excluded from retry ticks — existing semantics, server.rs:694,755). OIDC endpoint
  validation: https + hostname exactly `x.ai` or `*.x.ai` label-boundary suffix
  (mirror ValidateOAuthEndpoint, xai.go:47-64).
- `GrokProvider` (new `src/provider/grok.rs`): thin adapter over the shared Responses
  core (R5). `GrokShape { model: "grok-4.6" (default), client_model: Option, effort:
  Option }` — **no `fast`** (xAI has no service tier). Live-mutable behind `RwLock`
  exactly like `CodexShape` (codex.rs:96-125).
- Request shape differences vs codex (adapter knobs, not forks):
  - endpoint = `config.grok.upstream` (default `https://cli-chat-proxy.grok.com/v1`) +
    `/responses`;
  - identity headers `X-XAI-Token-Auth` / `x-grok-client-version` / `User-Agent`
    attached only when upstream is the official cli-chat-proxy host;
  - **NO `x-grok-conv-id` header** (consensus round 3): CLIProxyAPI sends it only when
    an execution-session id exists or the model is a `grok-composer-*` requiring
    isolated conversations (xai_executor.go:1116-1148); for standard grok-4.5 chat it
    is absent. Omitting it removes any cross-session state-mixing risk from a shared
    per-process id. Body `prompt_cache_key` = per-process uuid stays (pure cache hint,
    codex parity — not conversation state);
  - `service_tier` never sent; `include: ["reasoning.encrypted_content"]` **not** sent
    (OpenAI-specific; CLIProxyAPI does not send it for xAI);
  - effort is **per-model capability, not provider-global**: a static thinking-levels
    table (source: CLIProxyAPI registry models.json:2411-2520) —
    `grok-4.6 → {low,medium,high,xhigh}`, `grok-4.5 → {low,medium,high}`,
    `grok-4.3 → {none,low,medium,high}`,
    `grok-3-mini → {low,medium,high}`; models NOT in the table (e.g. `grok-build-0.1`,
    unknown slugs) get **no `reasoning` field at all** (omission, mirroring CLIProxyAPI's
    strip at xai_executor.go:1206-1211, debug-logged). For models in the table, the
    requested/configured effort clamps INTO the model's level set: `none|minimal` → `low`
    when zero not allowed (else `none`), `xhigh|max|ultra` → `high`. When the clamped
    result is `none` (only reachable on models whose level set contains it, e.g.
    grok-4.3), the `reasoning` field is OMITTED rather than sent as `"none"` —
    omission is the only universally-accepted wire form; explicit `"none"` is an
    untested upstream shape. **The per-model table is the single source of effort
    truth**: the `POST /llmux/grok` config endpoint accepts the SUPERSET
    `none|low|medium|high` (plus empty/`unset` to clear) and the per-request clamp
    against the effective model happens at request time — a configured `none` on a
    model without `none` degrades to `low` at request time, by the same clamp.
- Error handling (429): `Retry-After` header, when present, wins (existing generic
  path). Else a body whose `code`/`error` contains `free-usage-exhausted` / "included
  free usage" → park with **estimated** probe-not-before `now+24h` (xAI advertises a
  rolling 24h window — the real reset time is unknowable from the 429; surfaces render
  it as an estimate, not a hard reset). Generic 429 → existing backoff untouched.

### R2. Registration in CLI and islands

- CLI: `llmux login --grok` — device-code flow: discovery → device code → print
  `verification_uri_complete` + `user_code`, attempt `open` of the URL, poll token
  endpoint (interval/slow_down per RFC 8628, mirrors xai.go:196-328) → upsert account
  `grok:{email}`. No localhost callback port needed (unlike codex PKCE).
- Islands: daemon-run login (`POST /llmux/login/start {"provider":"grok"}`) —
  `LoginProvider` gains `Grok` (aliases `grok|xai|x-ai`, proxy/login.rs:22-47).
  Device flow has no browser callback; the daemon must surface the verification URL to
  the GUI: `GET /llmux/login/status` response gains optional `verification_uri` (the
  `_complete` variant) + `user_code` while pending. The DAEMON best-effort opens the URL
  host-side (parity with the PKCE flows); islands renders the clickable link + user code
  (covers remote daemons without double-tabbing local ones) and keeps polling until
  Done/Error. (Additive fields — older clients ignore them.)

### R3. Stats & usage display (CLI + islands)

- Token/cost stats: the shared Responses SSE core already yields `StreamUsage`; the
  dashboard tracks per-(group, model). Add pricing entry `GROK_4_5 {input 2.0, output
  6.0, cache_read 0.5, cache_creation 0.0}` + `group == "grok"` unknown-model fallback
  (src/pricing.rs builtin table, pricing.rs:58-100).
- Quota windows (**revised 2026-07-14**, post-ship): grok has no active usage endpoint,
  but 200 responses carry `x-ratelimit-{limit,remaining}-{requests,tokens}` burst headers
  (RPM/TPM-shaped, no reset). These feed the 5h slot via the standard-bucket path with an
  estimated 60s reset horizon (`headers::STANDARD_RESET_FALLBACK`), grok accounts only.
  The 7d gauge stays empty. A `free-usage-exhausted` park shows the existing
  cooldown/reset countdown UI.
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
  request/persist contract as `POST /llmux/codex` (server.rs:1089-1140): partial
  update, apply live, persist via `config::update_path`. This is the **daemon HTTP
  API** control (callable directly / by scripts). **Interactive callers are v1-descoped
  and accurately scoped** (consensus round 4):
  - The **TUI `f`/`m`/`e` grok control** (mirroring codex's `perform_remote_codex` at
    tui/mod.rs:1160 + `CodexSettingsDoc`) is NOT in this PR — it needs a parallel
    `GrokSettingsDoc` in the dashboard document and the keybind wiring, a bounded
    follow-up. codex keeps its TUI control; grok's is the endpoint only in v1.
  - The **islands settings row** is likewise descoped (islands has no codex row
    either; `POST /llmux/codex` is TUI/HTTP-only).
  The stated R4 requirement — Claude Code `/model grok-4.5` switching "like gpt-5.6" —
  is delivered by the routing path above (verified in the live receipt), independent
  of these interactive control surfaces.
- Config: new `config.grok` section `{upstream, default_model, reasoning_effort,
  client_model?, trace}` (defaults: cli-chat-proxy URL, `grok-4.6`, null, null, false).

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
  group match.
- **`on_empty_group` semantics, made precise** (was ambiguous; consensus review): the
  empty-group hook fires ONLY when the matched group has **zero configured accounts**
  (`has_account` = any account whose kind maps to the group, forward.rs:1140 — parked/
  limited accounts still count as configured). `"fallback"` then tries the remaining
  groups in fixed order `Claude → Codex → Grok`, first group with ≥1 configured account
  wins; after fallback the SERVING group's provider applies its own model rewrite (a
  grok-named request served by claude keeps Anthropic semantics — observable, logged).
  A group whose accounts are all parked/limited is NOT empty: the request stays
  in-group and gets the existing all-limited behavior (identical to the claude group
  today). Contract-tested (C2b).
- Existing codex unit tests move with the code they test; codex behavior is
  **bit-identical** (gate: full existing test suite green untouched except imports).
- **Commit discipline**: the behavior-preserving core extraction lands as its own
  commit (C12 green at that commit), grok lands on top — refactor failures isolate
  from provider failures.

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

## Compatibility & rollback (consensus review MUST-FIX)

- `AccountCredential` is an internally-tagged serde enum (`#[serde(tag = "type")]`,
  schema.rs:614-616): an OLD llmux binary **fails to parse** a config containing
  `"type": "grok"` (whole-file parse error, accounts is a plain Vec). This is the same
  contract the `Codex` variant shipped with — precedent, not new risk class — but it is
  now DOCUMENTED: **downgrade procedure** = remove `grok:*` account entries (and
  optionally the additive `grok`/`routing.grok_models` sections, which old binaries
  ignore harmlessly via `#[serde(default)]`) from `~/.config/llmux.json`, then run the
  old binary. `llmux accounts remove <name>` on the NEW binary is the supported path.
- Contract test (C17): a pre-grok config (no grok fields) parses and round-trips
  VALUE-stable under the new binary (the new default `grok` section is serialized;
  old binaries ignore it) — additive-only guarantee, semantic not byte-level.
- Islands↔daemon version skew: additive JSON only; old islands renders grok accounts
  as claude-group (existing `== "codex"` check) during upgrade windows — cosmetic,
  accepted.
- Cost figures for grok (and codex) subscription accounts are **API-list-price
  equivalent estimates**, not billed amounts — documented here; UI copy unchanged in
  v1 (existing cost tiles already carry this meaning for codex).

## Gates (merge-blocking)

1. Behavior-preserving extraction commit: full existing suite green (C12).
2. All contract tests C1–C17 green.
3. **Live wire receipt** before merge: at least one captured real
   `POST …/responses` request + SSE stream against a real grok account through the
   local daemon (raw_io trace on), INCLUDING a ≥2-turn tool-call round trip
   (reasoning-continuity check) — this closes spec Risks #2 (wire-shape unknowns).
4. External reviewer agent pass (zbrain DEV.md §2).

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
- `Llmux/LlmuxClient.swift` — login status verification fields (grok config setter:
  descoped with the settings row, §R4)
- `Dashboard/UsageModelTypes.swift`, `UsageTiles.swift`, `DashboardAnalytics.swift`
- `UI/Components/UsageProviderIcon.swift` — grok icon + fallback
- `UI/Views/NotchClosedLabelView.swift`, `IslandUsageView.swift` — third group + login
- `Core/SnapshotMode.swift`, `Core/DemoMode.swift` — fixtures
