# OpenRouter provider — spec

> STV artifact (zbrain rules/STV.md), same shape as [`docs/grok/spec.md`].
> All upstream facts below are LIVE-PROBED (2026-08-21) or quoted from OpenRouter docs;
> nothing here is inferred from the grok/codex precedent.

## Goal

Add **OpenRouter as a fourth backend group** (`openrouter`) alongside `claude`, `codex`,
`grok` — so a Claude Code client pointed at llmux can pick a free OpenRouter model with
`/model or-ox-alpha` and have it served from an OpenRouter account.

Two user requirements (verbatim, 2026-08-21):
1. "인증하고 로그인 추가할수 있고" → `llmux login --openrouter` (OAuth PKCE browser flow,
   plus a paste-the-key fallback).
2. "model에서 or-모델이름 으로 선택해서 or 모델 그룹으로 추가" → the `or-` model prefix
   selects an OpenRouter model, and those models form the `openrouter` backend group.

Trigger model: **Ox Alpha** (`stealth/ox-alpha`), free, 1M context
(https://www.threads.com/@choi.openai/post/DcTPAuQjTMK).

## Why this is NOT a codex/grok-shaped translator (the load-bearing finding)

Codex and grok are Messages↔Responses **translators** with a live SSE transform
(`src/proxy/forward.rs:1076` `is_translate`). OpenRouter is not: it exposes a native
**Anthropic Messages** endpoint.

Live probe, 2026-08-21 (no key):

```
$ curl -s -X POST https://openrouter.ai/api/v1/messages -H 'content-type: application/json' \
    -d '{"model":"stealth/ox-alpha","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}'
{"type":"error","error":{"type":"authentication_error","message":"No cookie auth credentials found","error_type":"authentication"},"request_id":null}
```

That is the **Anthropic** error envelope (`type/error.type/request_id`), not OpenAI's
(`{"error":{"message",...,"code"}}` — which is what `/api/v1/chat/completions` returns on the
same probe). Confirmed by the official reference:
https://openrouter.ai/docs/api/api-reference/anthropic-messages/create-a-message
("Create a message … Anthropic Messages API format … text, images, PDFs, tools, extended thinking").

**Design consequence:** OpenRouter rides the **passthrough** path (`AnthropicPassthrough`
shape), not the translate path. No SSE converter, no format translation, no
`request_meta`/`converter` plumbing. The only body mutation is the `model` field rewrite.

## Verified upstream facts (openrouter wire contract)

| Fact | Value | Evidence |
|---|---|---|
| Messages endpoint | `POST https://openrouter.ai/api/v1/messages` | live probe 2026-08-21 (Anthropic error envelope); docs api-reference/anthropic-messages |
| `GET /api/v1/messages` | 404 — POST only | live probe 2026-08-21 |
| Auth header | BOTH `Authorization: Bearer sk-or-v1-…` and `x-api-key: sk-or-v1-…` accepted | live probe 2026-08-21: identical `authentication_error / "User not found."` for both |
| OAuth authorize URL | `https://openrouter.ai/auth?callback_url=<url>&code_challenge=<c>&code_challenge_method=S256` | docs use-cases/oauth-pkce |
| Headless authorize URL | same minus `callback_url`, plus `key_label=<app>` — code shown on screen to paste | docs use-cases/oauth-pkce |
| Code challenge | base64**url** of SHA-256(code_verifier) | docs use-cases/oauth-pkce |
| Token exchange | `POST https://openrouter.ai/api/v1/auth/keys`, JSON `{code, code_verifier, code_challenge_method}` → `{"key": "sk-or-v1-…"}` | docs; live probe 2026-08-21 returns `{"error":{"message":"Invalid code","code":400}}` for a bogus code |
| Code lifetime | single-use, 10 minutes | docs use-cases/oauth-pkce |
| Callback port | "any port" on localhost is accepted | docs use-cases/oauth-pkce |
| Key introspection | `GET /api/v1/key` (Bearer) → key label/usage/limit | live probe 2026-08-21 → 401 `User not found.` for a bogus key (endpoint exists) |
| Credits | `GET /api/v1/credits` (Bearer) | live probe 2026-08-21 → 401 (endpoint exists) |
| Model list | `GET /api/v1/models` — 420 models, 21 with `pricing.prompt == "0"` | live 2026-08-21 |
| Token refresh | **none** — the PKCE exchange yields a long-lived API key, not an expiring access token | docs use-cases/oauth-pkce (no refresh_token in the response shape) |

### The free set as of 2026-08-21 (live `GET /api/v1/models`, `pricing.prompt == "0"`)

| id | name | ctx | notes |
|---|---|---|---|
| `stealth/ox-alpha` | Ox Alpha | 1,048,576 | max_out 131,072; reasoning **mandatory**, efforts `low/high/max`, default `max`; text+image+video→text |
| `openrouter/free` | Free Models Router | 200,000 | picks a random free model per request |
| `z-ai/glm-5.2:free` | Z.ai GLM 5.2 | 256,000 | |
| `nvidia/nemotron-3-ultra-550b-a55b:free` | NVIDIA Nemotron 3 Ultra | 1,000,000 | |
| `nvidia/nemotron-3.5-lightning:free` | NVIDIA Nemotron 3.5 Lightning | 1,000,000 | |
| `dots-studio/dots-3-note-preview:free` | Dots3-Note Preview | 512,000 | |
| `poolside/laguna-s-2.1:free` | Poolside Laguna S 2.1 | 262,144 | |
| `cohere/north-mini-code:free` | Cohere North Mini Code | 256,000 | |
| `google/gemma-4-31b-it:free` | Google Gemma 4 31B | 262,144 | |
| `openai/gpt-oss-20b:free` | OpenAI gpt-oss-20b | 131,072 | |

Curated rows are a **convenience layer, not a gate**: any `or-<slug>` passes through.

## Requirements → design

### R1. `openrouter` backend group

- `BackendGroup` gains `OpenRouter` (`src/routing.rs`), ordered LAST
  (`Claude < Codex < Grok < OpenRouter`) so existing `Ord`-dependent behavior
  (representative group = Claude, `on_empty_group` fallback scan order) is unchanged.
- `from_kind("openrouter") → OpenRouter`; `as_str`/`from_label` → `"openrouter"`.
- `AccountCredential::OpenRouter { api_key, label, created_ms }`, `kind() == "openrouter"`.
  No refresh path (the key does not expire) — `needs_refresh()` is always false.
- Account name convention: `or:<label>` (label from `GET /api/v1/key`, fallback `or:key-N`).

### R2. Routing rules — `or-` prefix

Builtin openrouter rules: `Prefix("or-")`, `Exact("or")`, `Prefix("openrouter/")`.
Config override `routing.openrouter_models` (additive `#[serde(default)]`).

Collision check against existing builtins: codex owns `gpt-`/`o1`/`o3`/`o4`/`~codex`,
claude owns `claude|opus|sonnet|haiku|fable`, grok owns `grok`. No prefix of any of those
matches `or-`/`or`. (`o1`/`o3`/`o4` are prefixes, not `o*`.)

### R3. Model resolution — `or-<name>` → OpenRouter slug

`src/provider/openrouter.rs::resolve_model`:

1. Trim, lowercase. Strip a leading `or-` (or accept a bare `or`).
2. Bare `or` → the live pin (`config.openrouter.default_model`, default `stealth/ox-alpha`).
3. If the remainder contains `/` → use it VERBATIM (`or-openai/gpt-oss-20b:free`
   → `openai/gpt-oss-20b:free`). This is the escape hatch for the other 400 models.
4. Else look up the curated alias table (`or-ox-alpha` → `stealth/ox-alpha`,
   `or-glm-5.2` → `z-ai/glm-5.2:free`, …).
5. No match → pass the remainder through verbatim and let OpenRouter 404 it with its own
   message (never silently substitute a different model).

The rewrite happens in `request_in` on the JSON body's `model` field only; every other byte
of the Messages payload is untouched.

### R4. Provider — passthrough + endpoint + auth

`OpenRouterProvider` implements `Provider`:
- `name()` = `"openrouter"`, `endpoint()` = `config.openrouter.upstream`
  (default **`https://openrouter.ai/api`** — the host root BEFORE `/v1`, because
  `forward.rs::send_upstream` appends the client's verbatim path `/v1/messages`,
  composing `https://openrouter.ai/api/v1/messages`. Setting `…/api/v1` here
  yields `…/api/v1/v1/messages`, which live-probes 404 against the correct
  URL's 401 — pinned by a unit test).
- `auth()` = remove client `x-api-key`/`authorization`, inject
  `Authorization: Bearer <api_key>`. A non-openrouter credential reaching here is an
  `Auth` error (mirrors `provider::anthropic::inject_credential`'s cross-provider guards).
- `request_out`/`response_in`/`response_out` = identity (same as `AnthropicPassthrough`).
- `request_in` = model rewrite (R3) + header hygiene: drop Claude-Code-specific
  `anthropic-beta` values and `anthropic-dangerous-direct-browser-access`; keep
  `anthropic-version`.
- `forward.rs`: the passthrough branch selects the provider by group — `OpenRouter` →
  `state.openrouter`, everything else → `state.provider`. `is_translate` stays
  codex/grok-only.
- `/v1/messages/count_tokens`: OpenRouter has no equivalent; answered locally with the
  same naive estimate the translate path already uses.

### R5. Login — `llmux login --openrouter`

Reuses the existing local-callback PKCE machinery (`src/auth/oauth.rs`):
1. Generate `code_verifier` (43–128 chars) + `code_challenge = base64url(sha256(verifier))`.
2. Bind a localhost listener, open
   `https://openrouter.ai/auth?callback_url=http://localhost:<port>/callback&code_challenge=…&code_challenge_method=S256`.
3. On callback, read `code`; `POST /api/v1/auth/keys` → `{key}`.
4. `GET /api/v1/key` for the label; upsert `or:<label>`.

`llmux login --openrouter --paste` (and an automatic fallback when no browser can be
opened) prompts for a key instead — the same shape as `login --api`.

**Never logged**: the key goes through `proxy::logging::mask_credentials` like every other
credential (AGENTS.md architecture rule).

### R6. Catalog + pricing

- `catalog.rs` gains the curated openrouter rows with `group: "openrouter"`, `aliases`
  carrying the `or-*` slug, and the live `max_context` values from the table above.
- `pricing.rs`: openrouter rows are **$0/M in and out** for the curated free set. A
  non-curated `or-` model has unknown price → `None` (no invented number).

### R7. Surfaces

CLI `llmux status` / `accounts`, TUI group column, dashboard, islands contract all key off
`BackendGroup`; each `match` gains the new arm. Islands (Swift) `from_label` degrades
unknown labels to claude — the Swift side gets its openrouter case in the same PR so the
group is not mislabeled (2026-07-14 grok lesson: group 오표기 was one of three live
regressions found only in the user's real daemon).

## Non-goals (v1)

- OpenAI-format `/api/v1/chat/completions` (unnecessary — the Anthropic endpoint exists).
- Paid-model spend tracking / credit-budget scheduling (free models only, `$0`).
- Usage-based scheduler ranking from `GET /api/v1/key` (the group is expected to hold one
  account; the field is captured but not yet fed to `scheduler/usage.rs`).
- `openrouter/free` router-specific handling (it is just another slug).
