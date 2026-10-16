# Gemini provider — spec

> STV artifact (zbrain rules/STV.md). The companion [`trace.md`](trace.md) is the source of
> truth for execution paths; this file fixes scope, design decisions and the File Map.
> Reference implementation analyzed: router-for-me/CLIProxyAPI @ `5fc580b1` (2026-10-15,
> shallow clone) — its Antigravity auth (`internal/auth/antigravity`), executor
> (`internal/runtime/executor/antigravity_executor*.go`) and Claude translator
> (`internal/translator/antigravity/claude`).

## Goal

Add **Gemini as a fifth backend group** (`gemini`) next to `claude`, `codex`, `grok` and
`openrouter`. Claude Code (`llmux run`) and Codex (`llmux run --codex`) select Gemini models
from the same catalog-driven `/model` picker and short aliases they use today:

| Typed | Resolves to (catalog row) | Upstream model |
|---|---|---|
| `gemini` | `gemini-3.8-flash-high[1m]` | `gemini-3.8-flash-high` |
| `argon` | `gemini-4-argon[1m]` | `gemini-4-argon` |
| `gemini-3.8-flash-high` / `-medium` / `-low` (± `[1m]`) | same id | id without `[1m]` |
| `gemini-4-argon` (± `[1m]`) | same id | `gemini-4-argon` |

> **Superseded 2026-10-15 for Claude Code.** Only Codex selects Gemini models. Claude Code
> gets no gemini rows in its picker and a local 400 for a gemini request; see
> [Claude Code (2026-10-15)](#claude-code-2026-10-15).

Out of scope: an Antigravity (`agy`) **frontend** (`llmux run --agy`). The Antigravity
*backend* is the only subscription path that serves these ids (facts below).

## Claude Code (2026-10-15)

- **Observation (2026-10-15).** The Antigravity backend answered Claude Code requests with
  429 `RESOURCE_EXHAUSTED` while the same account served Codex requests with 200 in the same
  minute.
- **Policy (owner decision, 2026-10-15).** llmux does not forward Claude Code traffic to the
  gemini group:
  - `llmux run` leaves gemini rows out of the Claude Code `/model` picker
    (`model_picker_settings` in `src/cli/run.rs`). The Codex catalog injected by
    `llmux run --codex` keeps them.
  - A request whose `User-Agent` starts with `claude-cli/` (Claude Code and its subagents)
    and that routes to the gemini group gets a local `400 invalid_request_error` from the
    gate in `src/proxy/forward.rs`, whatever its method and path: `/v1/messages`, its
    `/v1/messages/count_tokens` calls and the `/v1/responses` ingress alike. The gate keys on
    the client identity, never on the endpoint or the prompt. No request reaches Google. The
    message is the requested model, then `: `, then:
    `gemini models are not available from Claude Code: Google's Antigravity backend refuses Claude Code requests, so llmux does not forward them. Use gemini from Codex: llmux run --codex -- -m gemini`
    On `/v1/responses` the same text comes in the OpenAI error shape
    (`{"error":{"type":"invalid_request_error","message":…}}`); every other path gets the
    Anthropic error shape.
  - Other Anthropic-Messages clients are not affected by this gate. Codex enters on
    `/v1/responses` with its own `User-Agent` and is not affected either.
- **Unchanged.** Gemini usage stays visible (TUI `gemini N acc` row, `llmux status` quota
  rows). Argon still needs a Google AI Ultra or Enterprise login (404 on Google AI Plus).
- **Codex receipt (2026-10-15).**
  `llmux run --codex -- exec -m gemini 'Reply with exactly the word: pong'` → `pong`;
  activity `/v1/responses group=gemini model=gemini-3.8-flash-high status=200`.

The Goal paragraph above, trace scenarios S1 and S2, and Gate 3's Claude Code round trip
predate this decision and are kept as the design record.

## Verified upstream facts

| Fact | Value | Evidence |
|---|---|---|
| Subscription backend | `POST {base}/v1internal:streamGenerateContent?alt=sse`, `base` = `https://daily-cloudcode-pa.googleapis.com` | agy CLI log 2026-10-15 (`~/.gemini/antigravity-cli/log/cli-20261015_103929.log` names `daily-cloudcode-pa.googleapis.com/v1internal:{loadCodeAssist,fetchAvailableModels}`); CLIProxyAPI `antigravity_executor.go:35-40` |
| OAuth client | Antigravity's Google installed-app OAuth client (a client id and its client secret); NO PKCE; `access_type=offline`, `prompt=consent`. llmux does not ship this client: the operator configures it as `gemini.client_id` / `gemini.client_secret` (or `LLMUX_GEMINI_CLIENT_ID` / `LLMUX_GEMINI_CLIENT_SECRET`) | agy's own id_token `aud`/`azp` name the same client (decoded locally 2026-10-15); CLIProxyAPI `internal/auth/antigravity/constants.go:6-8`, `auth.go:141-170` |
| Scopes | `cloud-platform userinfo.email userinfo.profile cclog experimentsandconfigs` | `constants.go:12-18` |
| Redirect | `http://localhost:51121/oauth-callback` | `auth.go:144`, `sdk/auth/antigravity_headless_test.go:25` |
| Token / userinfo | `https://oauth2.googleapis.com/token`, `https://www.googleapis.com/oauth2/v2/userinfo?alt=json` | `constants.go:21-24` |
| Project | `POST {base}/v1internal:loadCodeAssist {"metadata":{"ideType":"ANTIGRAVITY"}}` → `cloudaicompanionProject` (string or `{id}`); absent → `onboardUser {tier_id, metadata}` poll | live 2026-10-15: 200, project present, `currentTier.id = free-tier`, `paidTier.id = g1-plus-tier` ("Google AI Plus"); `auth.go:248-330` |
| Envelope | `{model, project, userAgent:"antigravity", requestType:"agent", requestId:"agent-<uuid>", request:{contents, systemInstruction?, tools?, toolConfig?, generationConfig, sessionId}}` | `antigravity_executor_request.go:511-553`; live 200 2026-10-15 |
| Identity | `User-Agent: antigravity/hub/<version> darwin/arm64`; version from the public hub manifest, `2.23.1` on 2026-10-15; Cloud Code rejects newer models below `2.23.0` | `internal/misc/antigravity_version.go:18-24,116`; `GET …/manifest/latest-arm64-mac.yml` → `version: 2.23.1` |
| Stream event | `data: {"response":{"candidates":[{"content":{"role":"model","parts":[…]},"finishReason":…}],"usageMetadata":{promptTokenCount,candidatesTokenCount,thoughtsTokenCount,totalTokenCount,cachedContentTokenCount?},"modelVersion","responseId"},"traceId","metadata"}` | live capture 2026-10-15 (`gemini-3.8-flash-high` → `pong`, `modelVersion gemini-3.8-flash-n`) |
| Tool call part | `{"thoughtSignature":"<584 chars>","functionCall":{"name","args","id":"call_90071"}}` followed by `{"text":""}` | live capture 2026-10-15 (`gemini-3.8-flash-low`) |
| Thought signature rule | turn 2 with the real signature → 200; with `"skip_thought_signature_validator"` → 200; with none → **400** `Function call is missing a thought_signature in functionCall parts…` | live 3-variant probe 2026-10-15 |
| Tool schema | `functionDeclarations[].parametersJsonSchema` accepted a JSON Schema with `additionalProperties:false` | live probe 2026-10-15 |
| Model catalog + quota | `POST {base}/v1internal:fetchAvailableModels {"project"}` → `models{id:{displayName,maxTokens,maxOutputTokens,quotaInfo{remainingFraction,resetTime?},…}}`, `defaultAgentModelId` | live 2026-10-15: `gemini-3.8-flash-{high,medium,low}` ctx 1,048,576 / out 65,536, `quotaInfo {remainingFraction:1, resetTime:2026-10-22T01:50:24Z}`; `defaultAgentModelId = gemini-3.8-flash-high` |
| Argon availability | Ultra / Enterprise only; this account lists `gemini-4-argon-disabled` ("Unavailable", `quotaInfo:null`); `gemini-4-argon` → **404 NOT_FOUND** | [Antigravity models page](https://antigravity.google/docs/models/) (fetched 2026-10-15); live probe 2026-10-15 |
| Context / output | `gemini-3.8-flash`: 1,048,576 in / 65,536 out; `gemini-4-argon`: 1,048,576 unified, 65,536 out by default | [3.8 Flash page](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash) (updated 2026-09-02); [Argon page](https://ai.google.dev/gemini-api/docs/models/gemini-4-argon) (updated 2026-10-14) |
| Pricing (API list, for cost display only) | 3.8 Flash $0.75 in / $3.75 out / $0.075 cached (through 2026-12-31, then doubles); Argon $2 / $10 / $0.10 (through 2026-11-30, then doubles) | [pricing page](https://ai.google.dev/gemini-api/docs/pricing) (updated 2026-10-14) |
| 429 shapes | `error.status = RESOURCE_EXHAUSTED`; `error.details[]` carries `google.rpc.RetryInfo.retryDelay` and `google.rpc.ErrorInfo.reason` (`QUOTA_EXHAUSTED` = window spent, `RATE_LIMIT_EXCEEDED` = burst) | `antigravity_executor_credits.go:226-282` |
| `maxOutputTokens` | removed for non-Claude Antigravity models | `antigravity_executor_request.go:90-101` |
| ToS risk | Google states that using Antigravity resources through third-party tools/proxies violates the Antigravity ToS and suspends accounts (repeat = permanent) | [gemini-cli discussion #20632](https://github.com/google-gemini/gemini-cli/discussions/20632); forum reports 2026-02…09 |

## Requirements → design

### R1. `gemini` backend group + routing

- `BackendGroup::Gemini` is appended **after** `OpenRouter` (the derived `Ord` fixes the
  fallback scan order and the status representative — neither moves). `from_kind("gemini")`,
  `as_str`/`from_label` `"gemini"`, `ALL` extended. `needs_body_translation = true`,
  `serves_messages_only = true` (no count_tokens sibling upstream).
- Builtin rules: `Prefix("gemini")`, `Exact("argon")`. Config `routing.gemini_models`
  (additive, `#[serde(default)]`): empty keeps builtins, non-empty replaces them.
- Disjointness with every other builtin family is unit-tested (no other family's rule
  matches `gemini…`/`argon`, and the gemini rules match none of theirs).

### R2. Credential, login, import

- `AccountCredential::Gemini { email, access_token, refresh_token, expires_at_ms,
  project_id, last_refresh_ms }`, serde tag `"gemini"`, kind `"gemini"`, dedup key = email,
  account name `gemini:{email}`.
- OAuth client: not shipped. `GeminiConfig::oauth_client()` resolves it from
  `LLMUX_GEMINI_CLIENT_ID` / `LLMUX_GEMINI_CLIENT_SECRET` (non-empty wins) over
  `gemini.client_id` / `gemini.client_secret`; either missing is
  `AuthError::GeminiClientNotConfigured` ("gemini OAuth client not configured: …"). Every
  login surface resolves it first and stops there (no browser, no store-import fallback);
  the TUI attached to a remote daemon uses its own machine's config and environment.
- `llmux login --gemini`: bind `127.0.0.1:51121`, open the authorize URL (`state` checked),
  exchange the code with client id + secret, read `userinfo` email, resolve the project
  (`loadCodeAssist`, `onboardUser` fallback), upsert. When the interactive flow cannot run
  (port taken, headless), fall back to importing the Antigravity CLI store
  `~/.gemini/antigravity-cli/antigravity-oauth-token` (codex parity:
  `account_from_codex_import`). `llmux import` probes the same file.
- Import is offline: email from the stored `id_token` JWT, expiry from `token.expiry`
  (RFC 3339), `project_id` empty. An empty project is resolved lazily (`loadCodeAssist`,
  `onboardUser` fallback) and persisted, by whichever of these comes first: the first model
  request (after its token refresh, R5), a manual usage refresh (`POST /llmux/refresh-usage`)
  or a scheduled usage poll (R6). The usage paths never refresh a token, so they look the
  project up only while the stored token is usable (outside the 5-minute refresh-ahead
  margin). The startup usage pass never looks it up: the daemon reports ready only after
  that pass, so a lookup there would delay readiness. One implementation serves all three
  paths (`src/proxy/gemini_project.rs`).
- Refresh: `grant_type=refresh_token` + client id + secret. A returned `refresh_token`
  replaces the stored one; an absent one keeps it (grok contract). `invalid_grant` / 400 /
  401 → `RefreshOutcome::Permanent`. No OAuth client configured → no grant is sent:
  `RefreshOutcome::Misconfigured` (never a bench, never "re-login required"). The request
  is answered `502` with the message (at once when the token has expired, after the
  upstream 401 otherwise), and the background refresh skips gemini accounts so an expired
  gemini token cannot starve the other accounts' refresh.
- Islands/daemon login: `LoginProvider::Gemini` (aliases `gemini|google|antigravity`),
  same PKCE-style callback phase as codex.

### R3. Provider (Messages ↔ Antigravity)

`src/provider/gemini.rs`, `GeminiProvider { base_url, shape: RwLock<GeminiShape>,
signatures: SignatureCache }`, `GeminiShape { model (default "gemini-3.8-flash-high"),
client_model, user_agent }`.

- **Model resolution** (`resolve_model`): trim; strip one trailing `[1m]`
  (case-insensitive); alias `gemini` → `gemini-3.8-flash-high`, `argon` →
  `gemini-4-argon`; bare `gemini-3.8-flash` picks the variant from `output_config.effort`
  (`none|minimal|low` → `-low`, `medium` → `-medium`, otherwise `-high`); any other
  `gemini-*` id passes verbatim; non-gemini names (empty-group fallback) use `shape.model`.
  Client effort is NOT sent upstream: the level is baked into the variant id, and no
  `thinkingLevel` wire form is verified for Argon on this backend.
- **Request** (`translate_request`): system (string or text blocks) →
  `systemInstruction {role:"user", parts}`; `user`→`user`, `assistant`→`model`; text →
  `{text}`; base64 image (png/jpeg/webp/heic/heif) → `inlineData`; base64 PDF document →
  `inlineData application/pdf`; text document → `{text}`; URL sources, other media and
  unknown block types → local 400 (`ProviderError::InvalidRequest`, JSON path in the
  message, never the payload); `tool_use` → `functionCall {name, args, id}`; `tool_result`
  → `functionResponse {name (from the matching earlier tool_use), id, response:{result |
  error}}`, images in its content become `functionResponse.parts[].inlineData`; prior
  `thinking` / `redacted_thinking` blocks are dropped (no Anthropic-signature replay);
  adjacent same-role contents merge.
- **Thought signatures**: the converter gives every `functionCall` a freshly minted client
  tool id (`toolu_gemini_<uuid>`, R4) and stores `CachedCall {signature, upstream_id}` — the
  part's `thoughtSignature` and Google's own `functionCall.id`, either may be absent — in a
  bounded cache keyed by `(sessionId, client tool id)` (capacity 4096, FIFO by last insert).
  The key is never Google's id: live call ids are small counters such as `call_90071` that
  repeat, and conversations that open with the same user text share a `sessionId`, so a key
  on Google's id let one conversation's signature overwrite another's. On the next turn the
  request side looks up each replayed client id: a hit that holds Google's id sends it back
  on BOTH the `functionCall.id` and the matching `functionResponse.id` (a call Google sent
  without an id keeps the client id), and the cached signature is attached. The FIRST `functionCall` part of a model turn with no cached signature gets
  `skip_thought_signature_validator` (both measured 200; omission measured 400); a later
  parallel call without one carries none (Gemini signs only the first call of a parallel
  step). A miss keeps the client id as sent. The cache is in-memory: after a daemon restart
  the dummy keeps multi-turn tool use working.
- **Tools**: `{name, description, input_schema}` → one `{functionDeclarations:[…]}` entry
  with `parametersJsonSchema` = the schema minus the `$schema` / `$id` / `$comment`
  keywords, removed from the root and every subschema position (`properties` /
  `patternProperties` / `$defs` / `definitions` / `dependentSchemas` / `dependencies`
  values; `items`, `additionalProperties`, `not`, `if` / `then` / `else`, `contains`,
  `propertyNames` and the other single-subschema keywords; `allOf` / `anyOf` / `oneOf` /
  `prefixItems` members). Property names (a property named `$id` stays) and literal data
  (`const`, `enum`, `default`, `examples`, any other keyword's value) are copied verbatim.
  Tools without `input_schema` (server tools) are dropped and logged.
  `tool_choice`: `auto`→`AUTO`, `any`→`ANY`, `tool{name}`→`ANY` + `allowedFunctionNames`,
  `none`→`NONE`.
- **Generation config**: `thinkingConfig.includeThoughts = true`; `temperature`/`top_p`/
  `top_k`/`stop_sequences` → `temperature`/`topP`/`topK`/`stopSequences`; `max_tokens` is
  NOT sent (reference parity — the backend applies the model's own cap).
- **Envelope**: `model`, `project` (credential), `userAgent:"antigravity"`,
  `requestType:"agent"`, `requestId:"agent-<uuid>"`, `request.sessionId` = `-<u63 of
  sha256(first user text)>` (stable per conversation, reference parity), else a random
  per-request id.
- **Headers**: `Authorization: Bearer`, `Content-Type: application/json`,
  `Accept: text/event-stream`, `User-Agent` = `shape.user_agent`. No other headers.

### R4. Stream conversion (`GeminiSseConverter`)

Implements `SseTransform` plus the accessor set `relay_translate` uses
(`error_message`, `raw_usage`, `events_seen`, `into_message_json`,
`with_safeguards_requested`).

- **Safeguards are NOT a no-op.** When the client asked for `dangerous_tool_use` review,
  the terminal `message_delta` and the aggregate carry `safeguard_results` with every
  announced tool id per-call `unavailable` (reason `unsupported`) — the exact answer the
  Responses converter gives (`responses.rs` `unavailable_safeguard_results`). Silence
  latches the whole Claude Code session to local classification, Anthropic turns included.

- First event → `message_start` (`id = msg_<responseId>`, `model` = client-facing id).
- `thought:true` text → `thinking` block (`thinking_delta`, empty `signature_delta` at
  stop — unsigned, so the Anthropic path's existing `strip_foreign_thinking` removes it on a
  model switch). Non-thought text → `text` block (empty texts skipped). `functionCall` →
  `tool_use` block (`id` = a freshly minted `toolu_gemini_<uuid>` (32 hex digits) for every
  call, whether or not Google sent an id; Google's id goes to the signature cache, R3), one
  `input_json_delta` carrying the full `args` JSON, then stop.
- `finishReason`: `STOP` → `end_turn` (`tool_use` when any call was emitted),
  `MAX_TOKENS` → `max_tokens`, `SAFETY`/`RECITATION`/`BLOCKLIST`/`PROHIBITED_CONTENT`/
  `SPII` → `refusal`, `MALFORMED_FUNCTION_CALL` and unknown → Anthropic `error` event.
- Usage: `input_tokens = promptTokenCount − cachedContentTokenCount`,
  `cache_read_input_tokens = cachedContentTokenCount`, `output_tokens =
  candidatesTokenCount + thoughtsTokenCount`; emitted in the final `message_delta`.
- `data: {"error":…}` mid-stream → Anthropic `error` event, `error_message` set.

### R5. Relay integration

- `relay_translate` takes a `TranslateConverter` enum (`Responses | Gemini`) implementing
  `SseTransform` and the accessor set. Landed first as a behavior-preserving commit.
- Messages ingress: `is_translate` arm `BackendGroup::Gemini → state.gemini.build_request`.
- Responses ingress (Codex frontend): the existing `else` chain gains a Gemini arm —
  Responses → Messages (`responses::messages_request`) → Gemini adapter → Messages →
  Responses, exactly the grok/openrouter shape.
- Missing project at request time (a request that goes upstream: `/v1/messages` or the
  Responses ingress, never `count_tokens`) → resolve via `loadCodeAssist`, persist with
  `config::update`, continue; failure → 502 naming the step. The usage paths share the same
  lookup (R6).

### R6. Errors, quota, usage

- 429 with `RetryInfo.retryDelay` → retry-after = that delay. `ErrorInfo.reason =
  QUOTA_EXHAUSTED` (or no delay and the body names quota exhaustion) → park until the
  served model's `quotaInfo.resetTime` when known, else an estimated 1h probe-not-before.
- 403 whose body names the Terms of Service / suspension → account benched with the
  message "Google suspended Antigravity access for this account (ToS); appeal before
  re-enabling" — never silently rotated as a transient error.
- Active usage: `fetchAvailableModels` (`src/auth/gemini_usage.rs`). Window mapping: the
  default agent model's `quotaInfo` → the 7-day slot (`utilization = 1 −
  remainingFraction`, `resets_at = resetTime`). **Unknown is never zero**: missing
  `quotaInfo` or `remainingFraction` → no window. A row whose `remainingFraction` is present
  but unreadable (non-numeric, `null`, non-finite or negative) is skipped (logged at debug)
  and the other rows still count; only a catalog in which every row is unreadable fails the
  read as malformed. The usage poller and `POST /llmux/refresh-usage` read it with provider
  `"gemini"`; no status ever benches.
- Project before quota: `fetchAvailableModels` is addressed by the project, so an account
  imported without one (R2) has no usage source until it is resolved. A scheduled poll and a
  manual refresh resolve it first through the request path's lookup (guarded pool write +
  persist) when the stored token is usable, and the reading lands on the resolved
  credential. The startup pass (`prime`) never looks up and keeps the skip. A failed lookup on
  a scheduled poll is logged and skipped (no window, no backoff step) until the account's
  next poll; on a manual refresh it is that refresh's failure. A manual refresh whose token
  is not usable is refused before any IO as `unsupported` (422, kind `gemini: no usage
  source until its project is resolved (needs a fresh access token)`).
- Idle probe: gemini accounts are excluded (no quota headers on the stream).

### R7. Catalog, pricing, surfaces

- Catalog rows (group `gemini`, empty effort menu — the level is in the id):
  `gemini-3.8-flash-high[1m]` (aliases `gemini`, `gemini-3.8-flash`),
  `gemini-3.8-flash-high`, `gemini-3.8-flash-medium[1m]`, `gemini-3.8-flash-medium`,
  `gemini-3.8-flash-low[1m]`, `gemini-3.8-flash-low`, `gemini-4-argon[1m]` (alias
  `argon`), `gemini-4-argon`. Context 1,048,576. Aliases live on the `[1m]` rows
  (operator rule 2026-09-10).
- Pricing: `GEMINI_3_8_FLASH {0.75, 3.75, 0.075, 0}`, `GEMINI_4_ARGON {2.0, 10.0, 0.10,
  0}` + `group == "gemini"` fallback → flash. API-list-price-equivalent estimates.
- Surfaces: `/llmux/status` and `llmux status` group `gemini`, TUI group block + label,
  activity model/group, accounts list `(gemini)`, dashboard group bar. Islands render the
  `gemini` group label and in-flight count (Swift + Linux QML); provider icon falls back to
  the text glyph.

## Non-goals (v1)

- Antigravity frontend (`llmux run --agy`), agy config or settings sync.
- Gemini API keys (`generativelanguage.googleapis.com`), Vertex AI, Gemini CLI OAuth.
- Antigravity-hosted Claude / GPT-OSS models (served by their own groups in llmux).
- Thinking-level control for Argon (no verified wire form); Anthropic signature replay of
  prior thinking; web search grounding; image generation models; `count_tokens` upstream
  (local estimate, like grok).
- A live `POST /llmux/gemini` shape endpoint (config-file only in v1).

## Compatibility & rollback

- `AccountCredential` is internally tagged: a config with `"type":"gemini"` does not parse
  under older binaries. Downgrade = `llmux accounts remove gemini:<email>` first. The
  additive `gemini` / `routing.gemini_models` sections are ignored by older binaries.
- Contract test: a pre-gemini config parses and round-trips value-stable.
- Islands↔daemon skew: older islands render `gemini` accounts in the claude block —
  cosmetic.

## Gates (merge-blocking)

1. Behavior-preserving converter-enum commit: full existing suite green.
2. `just check` (fmt + clippy -D warnings + tests) green, contract tests in `trace.md`.
3. Live wire receipt through a scratch daemon on a real gemini account: text turn, a
   ≥2-turn tool round trip from Claude Code (superseded 2026-10-15: Claude Code is refused
   locally, see [Claude Code (2026-10-15)](#claude-code-2026-10-15)), a Codex-frontend
   turn, alias selection (`gemini`, `argon`) visible in the activity log. Argon *serving*
   needs an Ultra account; on Plus the receipt shows routing + the upstream 404 surfaced as
   an Anthropic error.
4. External review (trinity) before merge.

## File map

Rust: `src/routing.rs`, `src/config/schema.rs`, `src/config/mod.rs`,
`src/provider/{mod.rs,gemini.rs}`, `src/auth/{mod.rs,gemini.rs,gemini_usage.rs}`,
`src/proxy/{forward.rs,gemini_project.rs,server.rs,login.rs,usage_controls.rs,idle_probe.rs}`,
`src/scheduler/{usage.rs,idle_probe.rs,mod.rs}`, `src/cli/{mod.rs,login.rs,import.rs,
accounts.rs,api.rs}`, `src/catalog.rs`, `src/pricing.rs`, `src/dashboard.rs`,
`src/tui/{view.rs,triage.rs,ui.rs,mod.rs}`, `tests/`.
Islands: `llmux-islands/LlmuxIslands/{Llmux/LlmuxStatus.swift,Llmux/IslandUsageModel.swift,
UI/Views/NotchClosedLabelView.swift,UI/Views/IslandUsageView.swift,
UI/Components/UsageProviderIcon.swift,Dashboard/UsageTiles.swift}`,
`llmux-islands-linux/qml/Usage.qml`, `llmux-islands-core/src/contract.rs`.
Docs: `docs/models.md`, `docs/provider-compatibility.md`, `docs/configuration.md`,
`docs/codex-frontend/README.md`, `docs/multi-model-agents.md`, `AGENTS.md`.
