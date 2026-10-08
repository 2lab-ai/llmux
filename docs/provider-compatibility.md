# Provider compatibility

Two client frontends share four backend groups. What survives depends on both the
incoming protocol and the selected backend. The detailed legacy matrix below
describes **incoming Anthropic Messages**; Responses has its own transport and controls. This page is the readable difference matrix: what llmux forwards, what it drops (and
reports), and what it refuses outright, with the code line or dated receipt behind each row.

Read it before you trust a request field on a non-Claude model. Deeper detail lives in the
[operational reference](operational-reference.md#codex--grok-compatibility-contract) (the
user-facing contract) and the [responses compatibility spec](responses-compatibility/spec.md)
(the translation rules and their receipts).

## Frontend transport matrix

| Backend | Incoming Anthropic Messages | Incoming OpenAI Responses |
| --- | --- | --- |
| Claude | Native Messages with auth/model/thinking normalization | Official Claude Agent SDK; Responses converted to full Messages transcript and results converted back |
| Codex | Messages → Responses → Messages | Native Responses history/tools/encrypted reasoning preserved |
| Grok | Messages → Grok Responses → Messages | Responses → Messages → existing Grok adapter → Responses |
| OpenRouter | Native Messages with model/thinking normalization | Responses → Messages → OpenRouter Messages → Responses |

Transport source checked 2026-10-07: [`src/proxy/forward.rs`](../src/proxy/forward.rs)
(`run_taxonomy_loop`) and [`src/proxy/responses.rs`](../src/proxy/responses.rs)
(`messages_request`). This table describes implementation paths, not live parity
proof for every backend; installed release smokes cover native Codex and Claude SDK.

The [Codex frontend guide](codex-frontend/README.md) owns Responses controls,
structured output, tool replay and refusal/omission rules. In particular, Claude
SDK requests preserve `[1m]`, execute caller tools in the Codex client and reject
unsupported sampling/forced-tool controls. Native Codex Responses retains reasoning
continuity; the Messages translation losses below do not apply universally.

## Collaboration is not wire parity

When Claude Code delegates to an agent whose `model:` is `gpt-6-astra[1m]` or
`grok-4.7` ([multi-model agents](multi-model-agents.md)), that agent's requests
take the **incoming Anthropic Messages → Codex/Grok** rows of the matrices below,
with the same drops and refusals: `max_tokens` is not sent to Codex at all;
non-null `temperature`/`top_p`/`top_k` and non-empty `stop_sequences` are refused
with a local 400; prior `thinking` blocks are dropped and there is no reasoning
continuity across turns. The parent (Claude) keeps the native Messages path.

- Supported here means measured on the dated receipt in the row — not every agent
  prompt shape has been probed.
- Open the agent's row in the raw viewer ([ai-debugger](ai-debugger.md)) to see
  exactly which fields reached the provider.
- Nothing in this page changes the Codex frontend's SDK boundary described above.

## Haiku 5.5 catalog update

Checked 2026-10-08: `haiku` / `haiku-5-5` resolve to `claude-haiku-5-5[1m]`.
Native Messages strips `[1m]` on the wire; Responses keeps it for the SDK.
The [official model page](https://platform.claude.com/docs/en/models/haiku-5-5/overview)
lists 1M context, 128K output, adaptive thinking, text/image input and tools.
Native Messages still forwards output caps, thinking, tools/images and sampling
parameters under the matrix below; Haiku rejects non-default sampling values.
The Responses SDK path keeps its existing control validation and caller-tool
execution. Streaming/error handling, usage observation and upstream counting are
unchanged. Evidence: `provider::anthropic::tests::normalize_body_resolves_haiku_5_5_and_preserves_4_5`,
`catalog::tests::claude_entries_carry_curated_efforts_and_context` and the
frontend matrix's implementation paths. These code tests do not establish
Haiku-specific subscription availability, tool/streaming behavior or its full
context ceiling; those remain untested by this catalog change. Public API prices
are references, with [request-tier/TTL limitations](models.md#current-claude-reference-prices).

## Wire paths (incoming Messages)

| Group | Upstream | Path |
| --- | --- | --- |
| Claude (`fable`, `opus`, `sonnet`, `claude-*`) | `https://api.anthropic.com` (config `upstream`), subscription OAuth or API key | **native Messages, no translation** — the request keeps its path and shape; llmux swaps the credential header and normalizes the body only: model-alias resolution, `[1m]` suffix strip, unsigned-`thinking` strip (`src/provider/anthropic.rs:28-93`, relay branch `src/proxy/forward.rs:1472-1480`) |
| OpenRouter (`or-*`) | `https://openrouter.ai/api/v1/messages` — a native Anthropic Messages endpoint (`src/config/schema.rs:1191-1205`) | **native Messages, no translation**, but the body IS rewritten: `model` → wire slug, and the SAME unsigned-`thinking` strip as the Claude path (`src/provider/openrouter.rs:143-167`, which calls `anthropic::strip_foreign_thinking` at `:147`); the `anthropic-beta` / `anthropic-dangerous-direct-browser-access` headers are dropped (`:300-314`) |
| Codex (`gpt-*`, `sol`, `terra`, `luna`) | `https://chatgpt.com/backend-api/codex/responses` — the **ChatGPT subscription gateway**, not the public OpenAI API | **translation** Messages → Responses (`src/provider/responses_request.rs`) |
| Grok (`grok-*`) | `https://cli-chat-proxy.grok.com/v1/responses` — the **Grok subscription gateway**, not api.x.ai | **translation** Messages → Responses |

The two gateways share Responses *syntax* with the vendors' public APIs; that does not make
their *capabilities* the same. Public API docs are a hypothesis about them, not a receipt.

## Difference matrix (incoming Messages)

`forwarded` = sent upstream as-is · `dropped` = not sent, named in a response header ·
`400` = refused locally by llmux before any upstream call or credential refresh ·
`untested` = nobody has measured it here (not a claim that it fails).

| What you send | Claude | OpenRouter | Codex | Grok |
| --- | --- | --- | --- | --- |
| `max_tokens` (output cap) | forwarded | forwarded (`src/provider/openrouter.rs:563-577`) | **not sent at all** — the one cap field measured there is refused | forwarded as `max_output_tokens`, **meaning unproven** |
| `temperature` / `top_p` / `top_k` | forwarded | forwarded | **400**, except recognized session-title default temperature [below](#claude-code-internal-tasks) | **400** |
| non-empty `stop_sequences` | forwarded | forwarded | **400**, except [scoped monitor adapter](#claude-code-auto-mode-monitors) | **400** |
| assistant `thinking` history | forwarded when **signed**; a block with a missing/empty `signature` is stripped¹ | same¹ | **dropped** | **dropped** |
| assistant `redacted_thinking` | forwarded untouched (it carries no `signature` field by design) | same | **dropped** | **dropped** |
| `model` field | rewritten: alias resolved, `[1m]` client suffix stripped | rewritten to the OpenRouter wire slug (`or-ox-alpha` → `stealth/ox-alpha`) | replaced by the served upstream model (`src/provider/responses_request.rs:134`) | same |
| top-level `thinking` config (`budget_tokens`, `disabled`) | forwarded | forwarded | **dropped** (shape validated first) | **dropped** |
| base64 PNG/JPEG image on a user message | forwarded | forwarded | converted to `input_image` | converted |
| image by URL, other media types, unknown blocks | forwarded | forwarded | **400** (never fetched) | **400** |
| tools / `tool_choice` / `disable_parallel_tool_use` | forwarded | forwarded | converted; unnamed or undeclared tool → **400** | same |
| `/v1/messages/count_tokens` | upstream count | **local chars/4 estimate** (upstream 404s) | local estimate; images → **400** | local estimate; images → **400** |
| `prompt_cache_key` | n/a | n/a | sent | not sent |

¹ A `thinking` block with a missing or empty `signature` — what the Codex/Grok translator
synthesizes, since it has nothing to sign with — is refused by the real Anthropic API with
`Invalid signature in thinking block`, and OpenRouter's Messages schema requires the signature
too. So on **both** native-Messages groups those blocks are removed before relay, signed
blocks and `redacted_thinking` pass untouched, and a message left with an EMPTY content array
by the strip is dropped whole — an unsigned thinking-only turn has nothing valid to replay
(`src/provider/anthropic.rs:81-128`; OpenRouter reuses it at `src/provider/openrouter.rs:147`).
This is what makes a mid-session `/model` switch back to a native-Messages group survive.

Sources: matrix rows and their refusal reasons are enumerated in
[responses-compatibility/spec.md §2](responses-compatibility/spec.md#2-compatibility-matrix);
the local-estimate behavior is `src/proxy/forward.rs:2595-2611`.

## Codex: no output-limit guarantee — `max_tokens` is not sent at all

This is the difference most likely to bite, so it gets its own section.

**Live receipt (2026-09-14, `gpt-6-astra` over the ChatGPT gateway):** the smallest possible
cap, `max_output_tokens: 1`, returned

```text
HTTP 400 {"detail":"Unsupported parameter: max_output_tokens"}
```

Because the gateway refuses that field, llmux **omits the cap entirely** and names it in
`X-Llmux-Omitted-Fields` (`src/provider/responses_request.rs:255-290`). It does not substitute
another field, clamp, truncate locally, or synthesize a `max_tokens` stop — a faked cap is a
lie about a budget.

**Practical consequence: on `gpt-*` there is no output-limit guarantee at all.** Your
`max_tokens` is not enforced weakly — it is not transmitted. Model choice, effort and prompt
are the only levers left, and they are *guidance*, not enforcement: nothing bounds the
response length.

**What the supporting sources do and do not establish** (read 2026-09-14):

- The official Codex client's Responses request struct — the complete serialized field list —
  has no cap field:
  [`codex-rs/codex-api/src/common.rs#L259-L285`](https://github.com/openai/codex/blob/3abbf9fe2c6b6910e9de61f6a0c5bb468f74b5c8/codex-rs/codex-api/src/common.rs#L259-L285)
  @ `3abbf9fe2c6b6910e9de61f6a0c5bb468f74b5c8`.
- The same body goes to both hosts; only the base URL differs between the ChatGPT
  subscription backend and the public API
  ([`model-provider-info/src/lib.rs#L370-L388`](https://github.com/openai/codex/blob/3abbf9fe2c6b6910e9de61f6a0c5bb468f74b5c8/codex-rs/model-provider-info/src/lib.rs#L370-L388)).
- [openai/codex#36180](https://github.com/openai/codex/issues/36180) is an **open feature
  request** to make that client send `max_output_tokens`. It describes the client's own
  request body — it is not an official statement about what the gateway accepts.
- Public `api.openai.com` — a **different endpoint**, stated here only as contrast — does
  document `max_output_tokens` as an upper bound including reasoning tokens
  ([openai-openapi `openapi.yaml#L34177-L34182`](https://github.com/openai/openai-openapi/blob/498c71ddf6f1c45b983f972ccabca795da211a3e/openapi.yaml#L34177-L34182)).
  Public-API support says nothing about the subscription gateway.

**Qualified conclusion:** no supported alternative output-cap field was found in the current
official Codex client or its docs, and `max_output_tokens` is live-rejected. That is *not*
proof that the gateway accepts no cap at all — a client struct is not a server allowlist, and
other field names are **untested** here. If one is ever shown to work, this page and the
translator change together.

## Grok: the cap is accepted, but it is not your cap

**Live receipt (2026-09-14, `grok-4.6` over the cli-chat-proxy gateway):**
`max_output_tokens: 1` returned HTTP 200 with `status: "incomplete"`,
`incomplete_details.reason: "max_output_tokens"`, visible text `Hello`, and usage
`output_tokens: 168` of which `reasoning_tokens: 167`.

Stated exactly: on that single request, a cap of 1 was accepted, the response terminated as
`incomplete` for that reason, one visible token came back, and 168 output tokens were
reported. It does **not** establish that the cap generally bounds visible tokens, nor that it
bounds any total llmux can predict, nor anything about what a subscription is charged.
llmux therefore forwards your value verbatim and attaches the `max_tokens_semantics` warning
instead of claiming budget equivalence.

llmux maps `incomplete_details.reason: "max_output_tokens"` to Anthropic `stop_reason:
"max_tokens"`, keeping the partial text and the true upstream usage — the reported
`output_tokens` is never reduced to the number you asked for (`src/provider/responses.rs:725-743`).

## No reasoning continuity on Codex/Grok

This section applies to **incoming Messages**. Native Codex Responses preserves
caller-supplied encrypted reasoning and history. For Messages translation, prior
assistant `thinking` blocks are dropped, and neither gateway's own encrypted reasoning
is stored or replayed by llmux. Multi-turn text and tool transcripts are unaffected, but a
`gpt-*` or `grok-*` turn does not resume the previous turn's private reasoning. A top-level
`thinking` config is validated and then dropped: `budget_tokens` bounds nothing upstream and
`{"type":"disabled"}` does not stop these models from reasoning. Reasoning effort on these
groups comes from llmux's own resolution (`config.codex` / `config.grok`, `/effort`), not
from the Messages body — see [models](models.md) for the per-model effort menus.

## How to check any request yourself

When a translated request loses something, the response you receive names it
(`src/proxy/forward.rs:695-710`, `:851-882`). A faithful request carries none of these
headers at all, so their **presence** is the signal:

| Header | Meaning |
| --- | --- |
| `X-Llmux-Omitted-Fields` | fields that were **not** sent upstream (e.g. `max_tokens` on Codex, `thinking`, `thinking_config`) |
| `X-Llmux-Compatibility-Warnings` | the omissions plus semantic caveats (e.g. `max_tokens_semantics` on Grok) |
| `X-Llmux-Token-Count: estimate` | this count came from a local heuristic, not a tokenizer |

Two ways to act on them:

- **Inspect** — open the raw request/response viewer in the dashboard
  ([AI debugger](ai-debugger.md)) and read the headers and the actual upstream body.
- **Refuse** — send `X-Llmux-Compatibility: strict` and any of the losses above becomes an
  HTTP 400 *before* upstream traffic. Since normal clients always send `max_tokens`, strict
  mode rejects Codex requests rather than pretending to enforce a budget.

The headers are stamped on both terminal legs of a served request — streamed SSE and
aggregated JSON — so the same request reports the same losses either way. They ride a
response llmux actually produced; do not expect them on an upstream failure. Headers and WARN
logs are diagnostics, **not** a promise that Claude Code shows you a warning — it does not.

## Known unknowns

Stated so nobody reads a gap as a guarantee:

- Whether any non-`max_output_tokens` output cap works on the Codex gateway — **untested**.
  The official client sends no cap field and `max_output_tokens` is refused; neither fact
  enumerates the gateway's server-side allowlist.
- What Grok's cap bounds in general — **unmeasured**. One fixture showed a cap of 1 returning
  one visible token alongside 167 reasoning tokens; that is a single observation, not a rule.
- Whether the gateways accept `temperature` / `top_p` / `top_k` / `stop_sequences` at all —
  **untested**; llmux refuses them locally rather than forwarding on a public-API assumption.
- Billing: none of the probes above establish what a subscription is charged.
- Every probe is a single fixture on a single model on a dated snapshot of a gateway that can
  change without notice.

## Adding or changing a provider

Any provider/model integration or semantics mapping change must audit these axes and update
this page **in the same PR** — the rule and its checklist are in
[`rules/documents.md`](../rules/documents.md).


## OpenAI Responses frontend (2026-10-07)

The matrix above describes **Anthropic Messages ingress**. With `llmux run --codex`,
Codex sends Responses and the following additional contracts apply:

| Axis | Codex subscription backend | Claude Agent SDK backend |
|---|---|---|
| Endpoint/auth | Native gateway Responses, leased Codex token; ingress Bearer is a llmux key | Official SDK 0.3.292, isolated leased Claude OAuth/API key; no direct HTTP fallback |
| Output cap | `max_output_tokens` omitted + headers; strict policy rejects | Mapped to SDK `CLAUDE_CODE_MAX_OUTPUT_TOKENS` |
| Reasoning | Native items/encrypted content preserved; existing model effort policy applies | SDK effort mapped; foreign reasoning history omitted + headers; private signatures are not fabricated |
| Structured output | Native `text.format` reaches the gateway | SDK outputFormat validates the supplied JSON Schema; object and scalar/array/null results return as JSON text |
| Tools/images | Native function/custom/namespace/additional-tools payloads preserved | Structured transcript and MCP definitions; caller executes tools in Codex; string custom inputs and namespaces restored; user/tool-result images retained |
| Controls | Native fields reach gateway except reported cap omission | `temperature`, `top_p`, forced tool choice and `parallel_tool_calls:false` rejected400; verbosity omitted+reported |
| Streaming/errors | Native SSE; nonstream aggregates completed output items, including gateway terminals with empty output | SDK Messages SSE converts to Responses; malformed tools/truncated streams fail, never become successful executable calls |
| Usage/counting | Gateway input/output/cache counters; output includes reasoning | Real SDK usage, including internally completed web-search rounds; no token-count endpoint or synthetic tokenizer claim |

Evidence: `tests/e2e.rs::responses_*`, `src/proxy/responses.rs::tests`,
`src/proxy/sse.rs::tests::sdk_internal_round_totals_replace_initial_usage`, and
`bridge/*.test.mjs` (actual pinned SDK against localhost mock), 2026-10-07.
[Codex frontend guide](codex-frontend/README.md) explains installation, client
configuration and unsupported stored/background response features. Claude SDK
adds its own identity/reminders: role/content preservation is not byte-identity.

## Claude Code auto-mode monitors

**2026-10-07 compatibility exception:** a recognized Claude Code security monitor
for a recently observed GPT main session uses `claude_code.auto_classifier_model` (default alias `luna`) with medium
effort. It does not inherit global main-model effort or fast mode. Tenant plus
normalized device/session identity isolates concurrent sessions. Main CLI turns
(including tool-result continuations) update context; subagents and harness control
requests do not. The state has a six-hour TTL, 4096-entry cap, and no disk recovery.
Unknown sessions retain normal routing; an unavailable Codex group does not fall
back to Claude for a mapped monitor. This is a transport/model change, not an
authorization policy replacement or an assurance that two models judge identically.

| Axis | Monitor behavior and evidence (2026-10-07) |
| --- | --- |
| Prompt and output | Policy/transcript forwarded through existing Responses text conversion. Both severity stages supported: Stage 1 closing tag, Stage 2 optional closed `<thinking>`, severity 0–100 and optional category. Historical block yes/no supported when named by system contract. `src/proxy/auto_classifier.rs`; unit tests `severity_stage_one_stop_and_stage_two_thinking_are_preserved`, `historical_block_contract_and_unknown_shapes` |
| Stop and cap | Only a recognized single `</severity>`/`</block>` stop may be removed from provider input and applied to the completed, validated output. Actual closing delimiter is excluded and `stop_reason/stop_sequence` are set truthfully. `classifier_local_stop` warning is returned. `max_tokens` is still omitted/reported: no output-token/billing-cap promise. Generic stop refusal and strict original-control validation remain. E2E `auto_classifier_strict_policy_and_generic_stop_refusal_remain` |
| Reasoning | Request-owned medium effort; original top-level `thinking` remains omitted/reported. No prior-reasoning continuity added. Textual Stage 2 `<thinking>` is validated and preserved; provider reasoning summaries are excluded from the delivered verdict. E2E `auto_classifier_gpt_session_routes_luna_and_preserves_severity_stop` sets global ultra to prove override isolation |
| Tools/images | Monitor requests must have no tools. Existing image/content refusal and conversion policy is unchanged. No classifier result is turned into a tool call or fabricated native safeguard evaluation |
| Streaming/errors | Both client modes buffer and validate the complete actual verdict. Successful upstream stream buffer cap 1 MiB, total request deadline 60 seconds; malformed, failed, incomplete, truncated or oversized output produces an error, never partial approval. E2E `auto_classifier_stage_two_streaming_and_nonstreaming_preserve_real_verdict`, `auto_classifier_failed_incomplete_and_oversize_streams_never_expose_verdict`, `auto_classifier_silent_stream_errors_without_success_verdict` |
| Usage/counting | Original upstream usage retained, including reasoning. Count endpoint never learns main context or uses this adapter. Raw request remains original; translated upstream and activity identify the configured effective model/medium. E2E `auto_classifier_streaming_stage_one_and_raw_provenance`, `auto_classifier_unknown_session_other_endpoint_and_disabled_routing_are_unchanged` |
| Auth/endpoint | Existing Codex subscription credentials and `/responses` endpoint; no public OpenAI API assumption. E2E first regression proves success with no Claude account; `auto_classifier_codex_unavailable_never_falls_back_to_claude` covers configured fallback policy too |

These are mock-based routing/contract receipts, not live safety-quality equivalence
claims. The real CLI 2.1.292 Stage 1 wire shape was observed on 2026-10-07:
Sonnet request, max_tokens 64, disabled thinking, no tools, stop `</severity>`,
nonstream response `<severity>5` with a stop-sequence reason. Stage 2's no-stop
8192-token request and thinking/severity/category parser were inspected in the
installed 2.1.292 client. Test prompts are sanitized synthetic fixtures; no full
user transcript or credential is committed.

### Claude Agent SDK account errors

The pinned SDK's typed error is distinct from an arbitrary HTTP 403. Before any
client stream starts, `oauth_org_not_allowed`, `account_on_hold` and
`verification_required` (403), and `billing_error` (402), reject the leased
credential and retry the same request with another eligible account. The existing
fingerprint guard prevents a stale response from benching a replacement credential.
The same organization-rejected credential was also observed returning native
Messages HTTP 403 (`oauth_not_allowed_for_organization`), so this is not inferred
to be a local SDK-only setting restriction. These restrictions do not rotate
OAuth refresh tokens. `authentication_failed`
(401) retains the single forced refresh followed by normal authentication failover.

Request/model errors (400/404) leave account health unchanged. Rate limits retain
429 scheduling. Overload, cloud-credential setup errors and server/unknown errors
remain transient; cloud setup failures do not refresh a Claude OAuth token.
The SDK's synthetic `max_output_tokens` warning is followed by its real
`max_tokens` stop and becomes an incomplete Responses result, not an account error.
Once output has started, a later failure terminates that stream and is not replayed
as a fresh request on another account.

Evidence: `bridge/*.test.mjs` audits the SDK enum and actual SDK error sequence;
`tests/e2e.rs::responses_sdk_*` drives the Rust subprocess and HTTP lifecycle.

## Claude Code internal tasks

**2026-10-07:** recognized implicit requests in a GPT main session use the
configurable Opus→`sol`, Sonnet→`terra`, Haiku→`luna` alias mapping. The same tenant
and normalized session isolation used by monitors applies. Main execution requests
remain authoritative even if their text resembles a title/compaction prompt;
subagents cannot overwrite that choice. Unknown sessions are unchanged, apart from
an exact initial `quota`/`max_tokens:1` probe or captured no-tools session-title
request carrying fresh `llmux run` launch context. A title can arrive before the
first main turn; E2E `internal_routing_title_before_first_main_uses_fresh_hint_without_seeding_session`
covers that order, configured aliases, stale hints and observed Claude priority.
Mapped requests never fall back to Claude on Codex exhaustion.

| Axis | Internal-task behavior and evidence (2026-10-07) |
| --- | --- |
| Output/cap | Native title `{title:string}` JSON schema maps to Responses `text.format` with `name:session_title`, `strict:true`. Other title schemas fail locally. `max_tokens` remains omitted/reported, including the quota probe's `1`: this is an availability probe, **not** a one-token billing guarantee. E2E `internal_routing_title_schema_temperature_and_effective_model_both_legs`; strict schema accepted by real Codex Luna gateway probe on 2026-10-07 |
| Generation controls | Only recognized no-tools session titles may omit the captured default `temperature:1`; omission is reported in compatibility headers. Other temperatures and generic sampling/stop controls retain local 400. Strict mode validates original controls and refuses loss. Same title E2E |
| Reasoning | Titles/quota use request-owned low effort; workers preserve valid client effort independently of global main settings. Prior thinking/continuity policy unchanged; security remains medium. E2E `internal_routing_configured_tiers_and_security_override_do_not_hijack_main_switch` |
| Tools/images | Helpers/subagents use existing bounded client-tool and image conversion/refusal policy. Title/quota signatures require no tools; no new tool or image support is implied. `src/proxy/internal_requests.rs` |
| Streaming/errors | Titles/helpers use normal streaming or JSON aggregation, with actual effective model identity. Monitor-only verdict buffering, 60-second deadline and 1 MiB cap do not apply to workers. Provider protocol failures retain existing error behavior; no title/verdict is fabricated. Same title E2E tests both modes and global client-model override |
| Usage/counting | Genuine `x-codex-*` quota headers are preserved; `x-llmux-quota-source:codex` and `x-llmux-quota-mode:availability` identify the probe. No Anthropic utilization/reset/status headers are invented. Claude Code's native quota consumer cannot display Codex percentages from these headers; HTTP 200 only establishes availability. Count endpoints never consume launch hints or change context. E2Es `internal_routing_initial_quota_uses_validated_hint_and_real_codex_headers`, `internal_routing_unknown_session_pinned_count_and_disabled_are_unchanged` |
| Auth/endpoint | Existing Codex subscription credentials/gateway are used; no new credentials or public OpenAI API assumptions. Reserved launch header is removed before forwarding, other custom headers preserved. Original ingress bytes remain in raw logs; effective upstream model is logged. E2E configured tiers verifies Claude switchback and no hint leakage |

Captured native title fingerprint: `You are naming a coding session so the user can
pick it out of a long list of sessions.` with `<session>…</session>` input, empty
tools, temperature 1 and `{title:string}` schema (Claude Code 2.1.292). This also
fixes the activity label from `user` to `title`. No new prompt text is injected.

Startup context requires `x-llmux-claude-launch-time` (Unix milliseconds) within
120 seconds and not in the future; both reserved headers are stripped upstream.
Expired, missing or invalid timestamps leave unknown sessions unchanged. Remote
clock skew can suppress bootstrap. Session state remains ephemeral: a daemon
restart inside that short window can reuse launch context until a main turn is observed.
