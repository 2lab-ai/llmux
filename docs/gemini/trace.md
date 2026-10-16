# Gemini provider — vertical trace

Source of truth for execution paths ([spec](spec.md) fixes scope). Each scenario runs
client → daemon → upstream → back. Contract tests (`G*`) are the executable form; they
start RED.

## S1. Claude Code `/model argon` → streamed text

> Superseded 2026-10-15: the Claude Code picker omits gemini rows and a Claude Code request
> that routes to the gemini group gets a local 400 at routing (step 3), so it never reaches
> step 4 ([spec](spec.md#claude-code-2026-10-15)). Kept as the design record; steps 1–7 still
> describe other Anthropic-Messages clients.

0. **Client surface** — `llmux run` injects the catalog picker (`src/cli/run.rs`
   `model_picker_settings`); the user types `/model argon` (or picks
   `gemini-4-argon[1m]`). Claude Code submits `model: "argon"` verbatim (not a native
   alias, so no `ANTHROPIC_DEFAULT_*` export applies).
1. **API entry** — `POST /v1/messages`, client key auth (`proxy/keys.rs`).
2. **Input** — Anthropic Messages body; `stream: true`.
3. **Routing** — `Classifier::classify("argon")` → builtin `Exact("argon")` →
   `BackendGroup::Gemini` (G1). Scheduler leases a `gemini`-kind account.
4. **Refresh** — `refresh_credential` `Gemini` arm when `expires_at_ms` is near
   (google token endpoint; G6). Empty `project_id` → `loadCodeAssist` → persist (G8).
5. **Translate** — `GeminiProvider::build_request`: `resolve_model("argon")` →
   upstream `gemini-4-argon`, client-facing `gemini-4-argon[1m]` (G2); envelope + headers
   (G3); `POST {base}/v1internal:streamGenerateContent?alt=sse`.
6. **Upstream** — Plus account: 404 NOT_FOUND → `relay_translate` non-2xx arm → Anthropic
   `{"type":"error","error":{"type":"api_error","message":"gemini upstream: …"}}` with
   status 404. Ultra account: SSE.
7. **Return** — `GeminiSseConverter` → `message_start` (model `gemini-4-argon[1m]`) …
   `message_stop` (G4); activity log `group=gemini model=gemini-4-argon` — the resolved
   upstream id for every alias form (`argon`, `gemini-4-argon[1m]`), codex parity
   (`request_meta_records_gpt_6_astra_for_every_alias_form`): pricing keys off this string.

## S2. Claude Code two-turn tool round trip (gemini-3.8-flash-high)

> Superseded 2026-10-15 for Claude Code, as S1
> ([spec](spec.md#claude-code-2026-10-15)).

1–5 as S1 with `model: "gemini"` → upstream `gemini-3.8-flash-high`, tools →
`functionDeclarations[].parametersJsonSchema` (G5).
6. Turn 1 SSE part `{thoughtSignature, functionCall{name,args,id: call_…}}` → converter
   emits `tool_use {id: toolu_gemini_<uuid>}` (minted for every call, never Google's id) +
   caches `(sessionId, toolu_gemini_…) → CachedCall {signature, upstream_id: call_…}` (G7),
   `stop_reason tool_use`.
7. Claude Code runs the tool, sends turn 2 with `assistant[tool_use]` +
   `user[tool_result]` carrying `toolu_gemini_…`.
8. Translate: a cache hit puts Google's `call_…` back on the `functionCall.id` AND the
   `functionResponse.id`, and the `functionCall` part gets the cached signature;
   `functionResponse{name,id,response.result}` (G7). Cache miss → the client id goes up as
   sent and the first call gets `skip_thought_signature_validator` (G7b). Upstream 200 →
   text answer.

## S3. Codex frontend (`llmux run --codex -- exec -m gemini …`)

1. `POST /v1/responses` (Responses ingress, `proxy/responses.rs`).
2. Classifier → Gemini; `forward.rs` Responses arm: `responses::messages_request(input)`
   → Anthropic body → `state.gemini.build_request` (G9) → same upstream path as S1.
3. `GeminiSseConverter` → Anthropic SSE → existing Messages→Responses stream wrapper →
   Codex.

A `claude-cli/` client on this ingress never reaches step 2: the Claude Code gate refuses
it after classification with a local 400 in the OpenAI error shape
([spec](spec.md#claude-code-2026-10-15)).

## S4. `llmux login --gemini`

1. CLI binds `127.0.0.1:51121`; prints + opens the authorize URL (client id, scopes,
   `access_type=offline`, `prompt=consent`, `state`).
2. Callback `GET /oauth-callback?code&state` → state check → token exchange (form:
   code, client_id, client_secret, redirect_uri, grant_type) (G10).
3. `userinfo` → email; `loadCodeAssist` → project (G8); upsert `gemini:{email}`.
4. Failure → fall back to the agy store import (S5).

## S5. `llmux import` / login fallback from the agy store

1. Read `~/.gemini/antigravity-cli/antigravity-oauth-token`: `token.{access_token,
   refresh_token,expiry}`, `id_token` → email claim (G11).
2. Upsert `gemini:{email}` with empty `project_id`: resolved by the first model request
   (S1 step 4), a manual usage refresh or a scheduled usage poll (S6), whichever comes
   first; the usage paths only while the stored token is usable, never in the startup pass.

## S6. Usage poll

1. Empty `project_id` + a usable stored token (outside the refresh-ahead margin) →
   `gemini_project::ensure_project` (`loadCodeAssist`, guarded pool write + persist), on a
   scheduled poll (`UsagePoller::tick`) or `POST /llmux/refresh-usage`; never in the startup
   `prime` pass. A failed lookup skips this poll (no window, no backoff step) or fails the
   manual refresh.
2. `UsagePoller::poll_account` gemini arm → `fetchAvailableModels {project}` (G12).
3. `quotaInfo` of `defaultAgentModelId` → `seven_day {utilization: 1 − remainingFraction,
   resets_at: resetTime}`; absent → no window; a row with an unreadable
   `remainingFraction` is skipped, and only an all-unreadable catalog fails the read (G12b).

## S7. 429 / 403

1. 429 `RESOURCE_EXHAUSTED` + `RetryInfo.retryDelay "3.2s"` → retry-after 3.2s (G13).
2. `ErrorInfo.reason QUOTA_EXHAUSTED` → park until known reset, else 1h estimate (G13b).
3. 403 naming Terms of Service → bench with the ToS message (G14).

## Contract tests

| ID | Asserts | Home |
|---|---|---|
| G1 | `gemini*` / `argon` → Gemini; disjoint from other families; `routing.gemini_models` override | `src/routing.rs` |
| G2 | `resolve_model`: aliases, `[1m]` strip, bare `gemini-3.8-flash` + effort → variant, verbatim `gemini-*`, fallback to shape model | `src/provider/gemini.rs` |
| G3 | envelope fields, headers, no `maxOutputTokens`, `includeThoughts`, sessionId stable for equal first user text | `src/provider/gemini.rs` |
| G4 | converter: text, thought → unsigned thinking, empty text skipped, usage mapping, finish mapping, mid-stream error, `into_message_json` aggregate | `src/provider/gemini.rs` |
| G4b | `with_safeguards_requested(true)`: `safeguard_results` on `message_delta` and aggregate, every announced tool id per-call `unavailable` | `src/provider/gemini.rs` |
| G5 | tools → `parametersJsonSchema`, `$schema` / `$id` / `$comment` stripped at schema positions only (property names and `const` / `enum` / `default` / `examples` data kept), tool_choice mapping, server tools dropped | `src/provider/gemini.rs` |
| G6 | refresh form, rotation keep/replace, `invalid_grant` → permanent | `src/auth/gemini.rs` |
| G7 / G7b | signature cache round trip under minted client ids (upstream id restored on call and response; conversations sharing a session keep their own signatures); dummy on first uncached call only | `src/provider/gemini.rs` |
| G8 | `loadCodeAssist` string / `{id}` project; onboard fallback | `src/auth/gemini.rs` |
| G9 | Responses ingress reaches the gemini adapter | `src/proxy/forward.rs` tests |
| G10 | authorize URL params; code exchange form | `src/auth/gemini.rs` |
| G11 | agy store parse (email from id_token, RFC 3339 expiry) | `src/auth/gemini.rs` |
| G12 / G12b | quota mapping; unknown never zero; an unreadable row is skipped | `src/auth/gemini_usage.rs` |
| G13 / G13b | 429 retry delay + quota park | `src/proxy/forward.rs` |
| G14 | 403 ToS bench message | `src/proxy/forward.rs` |
| G15 | credential serde round trip + pre-gemini config value-stable | `src/config/mod.rs` |
| G16 | catalog rows + alias ownership on `[1m]` rows; pricing rows | `src/catalog.rs`, `src/pricing.rs` |
