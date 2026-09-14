# Provider compatibility

One harness, four backend groups — **not** four identical APIs. Claude Code sends the same
Anthropic Messages request every time; what survives the trip depends on which group serves
it. This page is the readable difference matrix: what llmux forwards, what it drops (and
reports), and what it refuses outright, with the code line or dated receipt behind each row.

Read it before you trust a request field on a non-Claude model. Deeper detail lives in the
[operational reference](operational-reference.md#codex--grok-compatibility-contract) (the
user-facing contract) and the [responses compatibility spec](responses-compatibility/spec.md)
(the translation rules and their receipts).

## Wire paths

| Group | Upstream | Path |
| --- | --- | --- |
| Claude (`fable`, `opus`, `sonnet`, `claude-*`) | `https://api.anthropic.com` (config `upstream`), subscription OAuth or API key | **native Messages, no translation** — the request keeps its path and shape; llmux swaps the credential header and normalizes the body only: model-alias resolution, `[1m]` suffix strip, unsigned-`thinking` strip (`src/provider/anthropic.rs:28-93`, relay branch `src/proxy/forward.rs:1472-1480`) |
| OpenRouter (`or-*`) | `https://openrouter.ai/api/v1/messages` — a native Anthropic Messages endpoint (`src/config/schema.rs:1191-1205`) | **native Messages, no translation**, but the body IS rewritten: `model` → wire slug, and the SAME unsigned-`thinking` strip as the Claude path (`src/provider/openrouter.rs:143-167`, which calls `anthropic::strip_foreign_thinking` at `:147`); the `anthropic-beta` / `anthropic-dangerous-direct-browser-access` headers are dropped (`:300-314`) |
| Codex (`gpt-*`, `sol`, `terra`, `luna`) | `https://chatgpt.com/backend-api/codex/responses` — the **ChatGPT subscription gateway**, not the public OpenAI API | **translation** Messages → Responses (`src/provider/responses_request.rs`) |
| Grok (`grok-*`) | `https://cli-chat-proxy.grok.com/v1/responses` — the **Grok subscription gateway**, not api.x.ai | **translation** Messages → Responses |

The two gateways share Responses *syntax* with the vendors' public APIs; that does not make
their *capabilities* the same. Public API docs are a hypothesis about them, not a receipt.

## Difference matrix

`forwarded` = sent upstream as-is · `dropped` = not sent, named in a response header ·
`400` = refused locally by llmux before any upstream call or credential refresh ·
`untested` = nobody has measured it here (not a claim that it fails).

| What you send | Claude | OpenRouter | Codex | Grok |
| --- | --- | --- | --- | --- |
| `max_tokens` (output cap) | forwarded | forwarded (`src/provider/openrouter.rs:563-577`) | **not sent at all** — the one cap field measured there is refused | forwarded as `max_output_tokens`, **meaning unproven** |
| `temperature` / `top_p` / `top_k` | forwarded | forwarded | **400** | **400** |
| non-empty `stop_sequences` | forwarded | forwarded | **400** | **400** |
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

Prior assistant `thinking` blocks are dropped, and neither gateway's own encrypted reasoning
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
