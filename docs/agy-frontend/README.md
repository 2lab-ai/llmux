# Antigravity CLI frontend (`llmux run --agy`)

Run Google's Antigravity CLI (`agy`) through llmux:

```sh
llmux run --agy
llmux run --agy -- -p 'Explain this repository' --model claude-opus-5-5
llmux run --agy -- --model gpt-6.2-sol --effort high
eval "$(llmux env --agy)"   # manual wiring, then run agy yourself
```

**In short.** agy ≥ 1.5.4 has an official "LLM gateway" mode, switched on by
environment variables alone. In that mode it needs no Google sign-in, speaks the
**public Gemini API** (the `genai` wire) to the gateway, and reads its model list from
the gateway. llmux serves that API at `/v1beta/models/…` and routes each request by
model to any backend group — claude, codex, grok, openrouter or gemini. The gemini
group gets the Gemini body almost unchanged; the others get it translated to Messages.
agy is the third client frontend, next to Claude Code (`llmux run`) and Codex CLI
(`llmux run --codex`), on the same account pool, scheduler and usage history.

Research date 2026-11-21, agy 1.5.13 (Homebrew cask `antigravity-cli`). Sources are
tagged: **[live]** a capture of agy against a recording gateway, **[binary]** the
agy binary read as data, **[docs]** Google's documentation, **[llmux]** this repo.

## Map

```
agy (gateway mode, no Google login)
  GET  /v1beta/models?pageSize=1000                       ─► llmux catalog (local)
  POST /v1beta/models/{model}:streamGenerateContent?alt=sse ─► shared forward path
  POST /v1beta/models/{model}:generateContent             ─►   (aggregated)
  POST /v1beta/models/{model}:countTokens                 ─► local estimate
                       │
                       ▼ route by model
   claude ── Messages ──► Claude Agent SDK bridge       ─┐
   codex  ── Messages ──► Responses translation         │ Messages SSE ─► genai SSE
   grok   ── Messages ──► Responses translation         │
   openrouter ─ Messages ► /v1/messages                 ─┘
   gemini ── genai body in the Cloud Code envelope ──► v1internal ─► frames unwrapped
```

| agy concern | llmux answer |
|---|---|
| Base URL | `AGY_LLM_GATEWAY_URL=http://localhost:<port>` (plain http to loopback only) |
| Wire | `AGY_LLM_GATEWAY_WIRE_PROTOCOL=genai` |
| Auth | `Authorization: Bearer <AGY_LLM_GATEWAY_API_KEY>`, or none (keyless loopback) |
| Models | `GET /v1beta/models` from the llmux catalog |
| Subagent tiers | `AGY_LLM_GATEWAY_TIER_PRO` / `_FLASH` / `_FLASH_LITE` = the model the session starts on |
| Activity | rows carry the Google endpoint origin (own tint) |

## Endpoints

Seen live in headless mode **[live]**: `GET /v1beta/models?pageSize=1000` at every
start, then one `POST /v1beta/models/{id}:streamGenerateContent?alt=sse` per model
turn. Nothing else: no `generateContent`, no `countTokens`, no `POST /v1beta:*`.
llmux also serves `:generateContent` (interactive side calls) and `:countTokens`
(a local estimate, about four bytes of JSON per token), and
`GET /v1beta/models/{id}` (one entry). Any other `/v1beta` path is a Google-shaped 404.
A doubled `/v1beta/v1beta/…` prefix is tolerated; agy itself drops a trailing
`/v1beta` from the gateway URL **[binary]**.

The model comes from the URL path (percent-decoded, a `models/` prefix stripped). agy
sends a discovery id verbatim **[live]**, except one case found while testing llmux:
for its own Gemini families it strips a level suffix and sends the level in the body
(next section).

## Redirect knobs

All environment, all session-scoped; `run --agy` edits no file **[binary][docs]**.

| Variable | `run --agy` |
|---|---|
| `AGY_LLM_GATEWAY_URL` | set to the endpoint base URL |
| `AGY_LLM_GATEWAY_WIRE_PROTOCOL` | set to `genai` |
| `AGY_LLM_GATEWAY_API_KEY` | set only when the endpoint has a key, removed otherwise |
| `AGY_LLM_GATEWAY_TIER_PRO` / `_FLASH` / `_FLASH_LITE` | set to the model the session starts on: the agy arguments' `--model`, else the start model |
| `AGY_LLM_GATEWAY_MODELS` | removed — it would replace discovery and its metadata |
| other `AGY_LLM_GATEWAY_*` (`_HEADERS`, …) | removed, except `_CA_CERT`, `_CLIENT_CERT`, `_CLIENT_KEY`, `_PROXY_URL` (how to reach llmux itself) |
| `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `GOOGLE_GEMINI_BASE_URL`, `CLOUD_CODE_URL`, `AGY_ADC_AUTH` | removed — they select other agy backends |

The admin (MDM) settings file wins over the environment **[docs]**:
`/Library/Application Support/Antigravity/admin_settings.json` on macOS,
`/etc/antigravity/admin_settings.json` on Linux. `run --agy` warns when one exists.

`--model <start model>` is passed unless the agy arguments name `--model` or start
with a subcommand (`agy models`). The start model is `agy.default_model`, which
defaults to `codex.default_model` — the model `llmux run --codex` starts on
([configuration](../configuration.md)). An alias resolves to its catalog row's id. The
subagent tiers follow the model the session starts on: a user `--model x` (or
`--model=x`), resolved the same way, else the start model. `llmux env --agy` has no
agy arguments, so its tiers are the start model.

## Auth

- agy sends `Authorization: Bearer <AGY_LLM_GATEWAY_API_KEY>`; with the variable
  unset it sends no auth header at all **[live]**.
- The Gemini API paths read the client key the way the OpenAI paths do: `Bearer`,
  `x-api-key` or `x-goog-api-key`, all naming the same key; a presented key must be
  valid even on loopback. A keyless loopback client is tenant `local`. Refusals use the
  Google envelope (`401 UNAUTHENTICATED`), which agy shows as
  `UNAUTHENTICATED (code 401): Invalid proxy API key` **[live]**.
- `run --agy` passes the endpoint's key: the local daemon's `proxy.api_key` (llmux
  initializes one), or `remote.api_key` with `--remote`.
- agy refuses plain `http://` to a non-loopback host **[binary][docs]**, so
  `run --agy` / `env --agy` against a remote daemon fail early with the way out: an
  ssh tunnel to the daemon port, then `llmux --remote 127.0.0.1:<port> run --agy`.
- No Google login, OAuth client or token handling on the agy side. The Claude Code
  `claude-cli/` refusal of the gemini group does not apply: agy's user agent is
  `antigravity/cli/<version> (…; auth_method=gateway)` **[live]**.

## Discovery and limits

`GET /v1beta/models` lists the llmux catalog as `vertexModelEntry` rows:
`name` (`models/<id>`), `displayName`, `description`, `inputTokenLimit` (the catalog
context window), `outputTokenLimit`, `supportedGenerationMethods`, `thinking`.
Paginated with `pageSize` (default 50, at most 1000) and `nextPageToken`.

What agy does with it **[live]**: `outputTokenLimit` becomes every request's
`maxOutputTokens`; `--effort` is refused unless `thinking` is true; with no `--model`
agy takes the **first** entry; ids it does not know take one of 32 placeholder slots
and the rest are dropped **[binary]** (log: `custom gateway model(s) exceeded the
maximum placeholder limit (32)`), while ids it knows — every Gemini level id,
`gemini-4-neon-xhigh`, `gemini-3.8-flash-low` — take no slot and show with agy's own
labels **[live, 2026-11-21]**; `claude-opus-5-5` counts as custom. Hence:

1. **One entry per model, no brackets.** A `[1m]` twin and its base row are one
   entry under the base id; the base row's window wins. A `[1m]`-only row
   (`claude-fable-5-2[1m]`, `claude-opus-4-8[1m]`) is listed under its base id and
   **served as the `[1m]` row**, so its window holds.
2. **Every Gemini level id, and at most 32 of the other entries.** Over that cap: the
   start model, then every model a catalog alias points at (each family's current
   pick), then the rest; within each tier the groups take turns, so no group crowds
   another out. The catalog after the Gemini 4 reshape (2026-11-21) gives 46 entries,
   29 ids agy does not know and 17 Gemini level ids, so none is left out (before the
   known-id rule, 36 candidates of the same day lost four).
3. **The start model first**, the rest in catalog order.
4. **`thinking`** is true when the row has an effort menu, and for a gemini id that
   names its level — every gemini row (`gemini-3.8-flash-low`, `gemini-4-neon-xhigh`).
   agy strips that suffix from the URL (`…/models/gemini-4-neon:…`) and sends the
   level as `thinkingLevel` (`LOW` … `XHIGH`) only when the entry says
   `thinking: true`; with `false` it sends `thinkingBudget: 0` and the level is lost
   **[live, 2026-11-21]**. The ingress folds the level back into the same id. agy
   still refuses `--effort` on such an id.
5. **`outputTokenLimit` per group** (the catalog has no output caps): claude 64,000;
   codex 128,000; gemini 65,536; grok 32,768; openrouter 16,384.

## Live capture findings (agy 1.5.13)

Against a recording gateway on loopback, no Google sign-in, auto-update off
**[live]**:

- Headers: `User-Agent: antigravity/cli/1.5.13 (aidev_client; os_type=…; arch=…;
  cl=…; auth_method=gateway)`, `Authorization: Bearer …`,
  `X-Goog-Ext-525006001-Bin` (an opaque stream marker), `Accept-Encoding: gzip`.
- Body keys: `contents`, `systemInstruction` (a ~17 KB system prompt as a user-role
  Content), `tools`, `generationConfig`, `labels`. No `toolConfig`, no
  `safetySettings`, no `sessionId` (dropped in 1.5.4).
- `tools`: 14 entries, each `{"functionDeclarations":[one declaration]}` with an
  OpenAPI-subset `parameters` schema: UPPERCASE types (`"OBJECT"`, `"STRING"`), int64
  counts as strings (`"minItems": "1"`).
- `generationConfig.thinkingConfig`: `{"includeThoughts":true,"thinkingBudget":1024}`
  on a thinking entry by default; `{"includeThoughts":true,"thinkingLevel":"HIGH"}`
  with `--effort high`; `{"includeThoughts":false,"thinkingBudget":0}` on an entry
  without `thinking`.
- `labels`: `cascade_id`, `trajectory_id`, `request_id`, `client`, `model_enum`, …
  — client metadata; llmux never forwards it.
- **Tool round trip:** agy echoes the model's `functionCall` back **without its
  `id`** (its `thoughtSignature` kept) and answers with
  `functionResponse{name, response:{output:"…"}}`, also without an id.
- Responses agy accepts: `data: <GenerateContentResponse>\r\n\r\n` frames, no
  `response` wrapper; a `{"text", "thought": true}` part shows as thinking; usage is
  read from `usageMetadata` (`cachedContentTokenCount` as cache reads).

## llmux mapping

### Translated (claude, codex, grok, openrouter)

`src/proxy/genai/request.rs` builds an Anthropic Messages body; each group then takes
its existing arm — claude through the Claude Agent SDK bridge exactly as the
Responses ingress does, codex and grok through the Messages → Responses translation,
openrouter at `/v1/messages`.

| Gemini API | Messages |
|---|---|
| `systemInstruction.parts[].text` | `system` |
| role `user` / `model` | `user` / `assistant`, decided per part: a `functionResponse` is always a user `tool_result` (agy may put one under `model`), a `functionCall` an assistant `tool_use`; same-role turns merge; `tool_result` blocks lead their turn |
| `text` | text |
| `{text, thought:true}`, `thoughtSignature` | dropped (foreign reasoning never goes to another vendor) |
| `inlineData` image/* · application/pdf · other | image (base64) · document (base64) · text note |
| `fileData` | text note |
| `functionCall{name,args,id?}` | `tool_use`; id = its own, else `toolu_agy_<content>_<part>` |
| `functionResponse{name,response}` | `tool_result` paired with the preceding model turn's calls by id, then by name in order; `response.output` (string) or the JSON text; `response.error` → `is_error` |
| `functionDeclarations[]` | tools; `parameters` converted (UPPERCASE → lowercase, `nullable` → a `"null"` type member, string counts → numbers, `format:"enum"` / `propertyOrdering` dropped); `parametersJsonSchema` passes through |
| `googleSearch`, `codeExecution`, other non-function tools | dropped (debug log) |
| `toolConfig.functionCallingConfig` AUTO/ANY/NONE + `allowedFunctionNames` | `tool_choice` auto / any (one name → that tool) / none |
| `maxOutputTokens` | `max_tokens` |
| `temperature` / `topP` / `topK` / `stopSequences` | carried to openrouter; **dropped** on claude (the SDK refuses them), codex and grok (refused there) — agy users cannot set them |
| `thinkingLevel` MINIMAL/LOW/MEDIUM/HIGH/XHIGH/MAX (bare or `THINKING_LEVEL_…`) | `output_config.effort` (`UNSPECIFIED` / `MODEL_DEFAULT` → none) |
| `includeThoughts: true` (budget ≠ 0) | `thinking: {type: adaptive}` — thinking on and shown |
| `thinkingBudget: 0` | no thinking |
| other `thinkingBudget` values (agy's default 1024) | not carried; the model's default effort applies |
| `labels` | ignored |

The answer comes back event by event (`src/proxy/genai/response.rs`): text deltas →
`parts:[{text}]` frames, thinking deltas → `{text, thought:true}`, a `tool_use` →
one `functionCall{name,args,id}` part when its block stops; the final frame carries
`finishReason` (end_turn / tool_use / stop_sequence → `STOP`, max_tokens →
`MAX_TOKENS`, refusal → `SAFETY`) and `usageMetadata` (`promptTokenCount` = input +
cache reads + cache writes, `cachedContentTokenCount` = cache reads,
`candidatesTokenCount` = output; no `thoughtsTokenCount` split — Messages output
counts include thinking), `modelVersion`, `responseId`.

### Native (gemini)

The gemini group serves the Gemini API natively
(`GeminiProvider::build_native_request`): the body rides as the Cloud Code envelope's
`request` (`{project, model, request, userAgent, requestType, requestId}`) with its
`thoughtSignature`s, Google's call ids and the OpenAPI schemas untouched. What changes:

- The model resolves by the provider's rules — alias, `[1m]`, and the effort from
  `thinkingLevel` picking the level variant of the bare family agy sends through the
  provider's family table: `gemini-3.8-flash` + `LOW` → `gemini-3.8-flash-low`,
  `gemini-4-neon` + `XHIGH` → `gemini-4-neon-xhigh` (the row the user picked), capped
  at the family's top level (`gemini-4-argon` + `XHIGH` → `gemini-4-argon-high`); no
  level → `-high`. The
  level rides in the id, so `thinkingConfig` becomes `{"includeThoughts": true}` (the
  Messages path sends the same).
- `labels` are dropped; `sessionId` is derived from the first user text.
- agy's one `functionDeclarations` entry per tool is merged into **one** `tools` entry
  (Cloud Code and Vertex have refused several function-declaration entries; the
  Messages path's single merged entry is the proven shape); other tool kinds stay
  entries of their own.
- A model turn's first `functionCall` without a signature (a call another model made
  in a mixed session) gets the `skip_thought_signature_validator` placeholder.

The stream comes back with each `{"response":…}` frame unwrapped
(`NativeStream`): usage is read from the last `usageMetadata` (fresh input = prompt −
cached), a stream with no `finishReason` ends in an error frame, and the account
handling (ToS benching on a suspension 403, quota parks) is the gemini group's own.

### Errors and the rest

- Every non-2xx answer becomes `{"error":{"code","message","status"}}` with the same
  HTTP status (`INVALID_ARGUMENT`, `UNAUTHENTICATED`, `PERMISSION_DENIED`,
  `NOT_FOUND`, `RESOURCE_EXHAUSTED`, `UNAVAILABLE`, `INTERNAL`); `retry-after` is
  kept. A stream that fails mid-way ends in one such `{"error":…}` frame. The same
  envelope answers before the body is decoded: a method a Gemini route does not serve
  (405), a body over `proxy.max_request_bytes` (413), a body that cannot be read (400).
- `:generateContent` folds the stream into one `GenerateContentResponse` (adjacent
  text parts merged).
- Raw-io captures the exchange at the client boundary; activity rows carry the
  Google endpoint origin ([activity](../codex-frontend/activity.md)).

## Limits and risks

- **Remote over http:** refused by agy; use a tunnel or an https endpoint.
- **Effort menus are agy's.** agy refuses `--effort` levels its own family defaults
  do not list (`xhigh` on `gpt-6.2-sol` was refused **[live]**); discovery cannot
  widen them.
- **No thinking by default on Claude:** agy sends a 1024 budget, not a level, unless
  `--effort` is given; llmux turns thoughts on (adaptive) but sets no effort.
- **Lost in translation** (non-gemini groups): thought text and signatures, file
  references, non-image/PDF inline media, non-function tools, a forced tool choice on
  claude, sampling knobs on claude / codex / grok.
- **Gemini group:** `maxOutputTokens` is forwarded (the Messages path omits its cap).
- **`--effort` stops at `high` in agy 1.5.13:** in gateway mode it accepts `--effort`
  only `low|medium|high` and refuses `xhigh` / `max` client-side **[live,
  2026-11-21]**. Pick the level by model instead: `--model gemini-4-neon-xhigh`
  reaches llmux as `gemini-4-neon` + `thinkingLevel: XHIGH` and is served as
  `gemini-4-neon-xhigh` **[live]**. agy's enum also has `MAX` **[binary]**, which
  llmux maps should a later agy send it.
- **Version churn:** the gateway knobs changed between 1.3 and 1.5. Minimum agy
  1.5.4 (no `sessionId`); re-check each release.
- **agy's own state** (conversations, logs, caches) stays in its home
  (`~/.gemini/antigravity-cli/`), as `--codex` uses the real `~/.codex`.
- **Telemetry** to Google in gateway mode is unverified (the checks here ran with
  agy's outbound proxy blackholed).

## Verification (2026-11-21)

- Unit: `src/proxy/genai/{mod,request,response,discovery}.rs`, the native builder in
  `src/provider/gemini.rs`, `src/cli/run/agy.rs`, `src/cli/env.rs`.
- e2e (`tests/e2e.rs`, `agy_*`): discovery / countTokens / 404 local; Bearer and
  `x-goog-api-key` auth; Google-shaped 405 / 413 before decode; gemini native round
  trip (envelope, one merged declaration entry, unwrapped frames, signatures, skip
  validator), the level via `thinkingLevel` on Gemini 3.8 Flash and Gemini 4 families,
  `generateContent`, upstream 400; codex both ways; openrouter at `/v1/messages` with
  Messages headers (and its Responses-ingress twin `responses_openrouter_posts_messages`);
  claude through an SDK fixture.
- CLI (`tests/cli.rs`): `run --agy` environment, `--model`, and the subagent tiers
  following a user `--model`, with a stub `agy`; remote http refused with the tunnel
  hint; `env --agy`.
- Live: agy 1.5.13 → a dev daemon on an isolated config and port → mock upstreams
  (Cloud Code, Codex Responses, a Claude Agent SDK stand-in): a text turn and a tool
  turn (`run_command` executed, result sent back) on gemini, codex and claude, all
  `SUCCESS`, activity rows `google`. Real-backend proof is not part of this record.
- Live effort probe: against a recording gateway, `--effort low|medium|high` reached it
  as `thinkingLevel: LOW|MEDIUM|HIGH` on `gemini-4-neon`, `gemini-4-argon`,
  `claude-opus-5-5`, `gpt-6.2-sol` and `grok-4.7`; `xhigh`, `max` and `minimal` were
  refused by agy itself. Picking a level-named id instead (`gemini-4-neon-high` /
  `-xhigh` / `-medium` / `-low`, `gemini-4-hydrogen-xhigh`, `gemini-4-argon-medium`,
  `gemini-4-carbon-low`, all `thinking: true`) reached it as the bare family plus
  `thinkingLevel: HIGH|XHIGH|MEDIUM|LOW`; one served with `thinking: false` arrived as
  `thinkingBudget: 0`.
- Live placeholder probe: with 40 unknown ids, 14 Gemini 4 level ids, the 3 Flash level
  ids and `claude-opus-5-5` / `claude-sonnet-5-5` served, `agy models` listed 32 unknown
  ids plus all 17 Gemini level ids and logged 10 custom ids skipped.

Sources (fetched 2026-11-21): antigravity.google docs (`/docs/install.md`,
`/docs/cli/headless.md`, `/docs/enterprise.md`, `/docs/changelog.md`); Google Cloud
"connect an enterprise gateway" page; github.com/danistrebel/agy-litellm-gateway;
github.com/humanfia/humanize PR #210; github.com/harbor-framework/harbor PR #4180.
