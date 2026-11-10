# Responses request compatibility — spec (codex + grok)

Ingress scope: the provider design and dated receipts below describe incoming
Anthropic Messages. For the later incoming OpenAI Responses path, see the
[frontend transport matrix](../provider-compatibility.md#frontend-transport-matrix).

What an Anthropic Messages request becomes on the two Responses backends llmux speaks to,
and what happens to the parts that do not fit. Implementation:
`src/provider/responses_request.rs`. Response-side (terminal events, counting) lives in
[`trace.md`](trace.md) and `src/provider/responses.rs`.

**The rule this document exists to state: llmux never silently drops request content.**
Before this unit, images, nested tool-result images, `tool_choice`, `max_tokens`, unknown
content blocks and nameless tools all disappeared into a `debug!`/`warn!` line the client
never sees — the model then answered a question it was never shown. Now each of those is
either converted, or refused with a typed error naming the JSON path, or (for the two
losses the wire makes unavoidable) reported in response headers.

## 1. Endpoints and evidence

| Backend | Upstream | Flavor |
| --- | --- | --- |
| codex | `chatgpt.com/backend-api/codex/responses` (ChatGPT subscription) | `ResponsesFlavor::Codex` |
| grok | `cli-chat-proxy.grok.com/v1/responses` (Grok subscription) | `ResponsesFlavor::Grok` |

Neither is the vendor's public API, so public API docs are a hypothesis, not a receipt.
Live probes against both endpoints (2026-09-11, synthetic fixtures — no user data):

| Probe | codex | grok |
| --- | --- | --- |
| base64 PNG `input_image` + flat named `tool_choice` | 200, `function_call report_color({"color":"red"})` | 200, same call |
| `tool_choice` `required` | 200, a function call | 200, a function call |
| image nested in `function_call_output.output` array | 200, answered from the image | 200, answered from the image |
| `tool_choice` `none` | 200, zero calls | 200, zero calls |
| `max_output_tokens: 16` | **400 `Unsupported parameter: max_output_tokens`** | 200 `status:incomplete`, `incomplete_details.reason: max_output_tokens`, usage reports `output_tokens: 302` of which `reasoning_tokens: 286` |

Re-probed 2026-09-14 with the smallest possible cap, `max_output_tokens: 1`: codex answered
`400 {"detail":"Unsupported parameter: max_output_tokens"}` again; grok answered 200
`status:incomplete` / `incomplete_details.reason: max_output_tokens` with visible output
and `output_tokens: 168` of which `reasoning_tokens: 167`.

The cap probe shows wire ACCEPTANCE and the visible-output effect on that one request. It
does **not** prove the cap bounds the same total the Anthropic client means by
`max_tokens` (the reported reasoning tokens are additional to the visible ones), and it
says nothing about billing.

What the codex 400 licenses is narrow: `max_output_tokens` is refused. Whether that backend
accepts some OTHER output-cap parameter is **unmeasured** here — no alternative field has
been probed, so its absence from this document is not evidence that none exists.

The per-flavor **image** format receipts (both gateways probed with all four media types
Anthropic accepts, 2026-09-17) live with the rule they justify, in §R1a.

## 2. Compatibility matrix

| Anthropic input | codex | grok | Report |
| --- | --- | --- | --- |
| text, tool_use, tool_result (text) | converted | converted | — |
| `image` base64 PNG/JPEG/WebP on a `user` message | `input_image` data URL, bytes verbatim | same | — |
| `image` base64 **GIF** on a `user` message | `input_image` data URL, bytes verbatim | decoded and re-encoded as a PNG data URL | — |
| `image` inside a `user` `tool_result` | `output` becomes a content-item array | same | — |
| `image` with `source.type: "url"` | **400** | **400** | — |
| `image` on assistant/system/developer | **400** | **400** | — |
| `image` media type outside PNG/JPEG/WebP/GIF, non-base64 data, >20 MiB encoded | **400** | **400** | — |
| **animated** GIF; GIF that needs >128 MiB to decode; GIF whose PNG would exceed 20 MiB; undecodable GIF | forwarded verbatim (no decode) | **400** | — |
| `document` / `audio` / `video` / any unknown block | **400** | **400** | — |
| `tool_choice` `auto` / `any` / `none` / `tool{name}` | `"auto"` / `"required"` / `"none"` / `{"type":"function","name":…}` | same | — |
| `tool_choice.disable_parallel_tool_use: true` | `parallel_tool_calls: false` | same | — |
| `tool_choice` naming an undeclared tool; `any`/`tool` with no tools | **400** | **400** | — |
| tool entry with no `name` (server-side tool) | **400** | **400** | — |
| `max_tokens` (positive integer) | omitted | `max_output_tokens` | `max_tokens` (codex) / `max_tokens_semantics` (grok) |
| `max_tokens` 0, negative, fractional, string, explicit `null` | **400** | **400** | — |
| `max_tokens` absent | no cap sent | no cap sent | — |
| assistant `thinking` signed `llmux-rs1:<this backend>:…` (the backend's own reasoning, R4) | a `reasoning` item at its position | same | — |
| any other assistant `thinking` / `redacted_thinking` (Anthropic-signed, the other backend's, unsigned) | omitted, order preserved | same | `thinking` / `redacted_thinking` |
| `thinking` on a non-assistant message | **400** | **400** | — |
| `role` outside user/assistant/developer/system | **400** | **400** | — |
| `system` (string or text blocks) | `instructions` (a leading `x-anthropic-billing-header:` line left out) | same | — |
| `role:"system"` message (text only) | `developer` input item at its position | same | — |
| `role:"system"` message with a non-text block | **400** | **400** | — |
| non-null `temperature` / `top_p` / `top_k` | **400** | **400** | — |
| non-empty `stop_sequences`; `stop_sequences` not an array | **400** | **400** | — |
| empty `stop_sequences`, or explicit `null` sampling control | accepted, asks nothing | same | — |
| top-level `thinking` config (`enabled`+`budget_tokens` / `adaptive` / `disabled`) | not forwarded | not forwarded | `thinking_config` |
| malformed top-level `thinking` | **400** | **400** | — |

**400** = `ProviderError::InvalidRequest` → HTTP 400 from llmux itself: no upstream call, no
credential refresh, no provider-failure score.

## 3. The rules

### R1 — content

- An image becomes `{"type":"input_image","image_url":"data:<media_type>;base64,<data>"}` —
  the flat string form both endpoints accept, never the Chat-Completions nesting. `detail`
  is not sent (the reference codex client strips it; its accepted values are model-gated).
- **Order is preserved.** Text and images interleave inside ONE message item's `content`
  array in transcript order; consecutive text blocks still join with `\n`, so a text-only
  message is byte-identical to what the previous translator produced.
- `tool_result` output stays a plain string when it is text only (the historical shape) and
  becomes the `[{"type":"input_text"…},{"type":"input_image"…}]` array only when it carries
  an image.
- `is_error: true` prefixes the output with `[llmux:tool-result-error]`. Responses has no
  error flag on `function_call_output`, and a failed tool result that reads as a successful
  one makes the model report success it never got.
- Validation is structural only. A `tool_use`'s `input` is the tool's OWN arbitrary JSON:
  it is serialized verbatim and never walked for content blocks, so a tool argument that
  happens to contain `{"type":"image"}` is payload, not structure. An ABSENT `input` keeps
  the previous translator's `{}` (a genuine no-arg call); an explicit `null` or a non-object
  is refused rather than stringified into `"null"`.
- No URL is ever fetched. Inlining a remote image would make llmux an SSRF proxy for its
  clients, and the reference codex client refuses remote image URLs outright.
- Error messages carry the JSON path (`messages[2].content[1].source.data`) and never the
  payload — a 20 MiB base64 blob must not reach a log. Over-cap images are refused from
  their ENCODED length, before any decode.

### R1a — Image formats

Anthropic's Messages API accepts exactly **jpeg, png, gif, webp**, so those four are every
image a client can legally send. What each gateway does with them was measured, not
assumed — live probes 2026-09-17 with synthetic fixtures, no user data:

| `input_image` media type | codex | grok |
| --- | --- | --- |
| `image/png` | 200, reply `OK` | 200, reply `OK` |
| `image/jpeg` | 200, reply `OK` | 200, reply `OK` |
| `image/webp` | 200, reply `OK` | 200, reply `OK` |
| `image/gif` | 200, reply `OK` | **400** `{"code":"invalid_image","error":"code: 'Client specified an invalid argument', message: \"Downloaded response does not contain a valid JPG, PNG, WebP, or ICO image."}` |

So the accepted set is per flavor, and llmux's policy follows it exactly:

| Flavor | Forwarded byte-for-byte | Re-encoded as PNG |
| --- | --- | --- |
| codex | png, jpeg, webp, gif | — |
| grok | png, jpeg, webp | gif |

**Passthrough is the default, and it is the point.** An accepted media type is not decoded
at all: the client's own base64 is reused, so the bytes upstream are the bytes the client
sent. A payload llmux never parses is one it cannot be broken by, and an image it never
re-encodes is one it cannot degrade. The "unsupported image" 400 users saw before this
unit came from llmux's own png/jpeg allowlist, not from either gateway.

llmux converts only where Anthropic accepts a format the gateway does not — today exactly
**GIF on grok**. Relabelling `image/gif` as `image/png` would not be a fix and is not what
happens: the backend parses the bytes, not the label. The GIF's composited RGBA frame is
really decoded and really re-encoded, transparent index included, so alpha survives.

Three refusals guard that conversion, each **before** the memory it protects is allocated.
They apply on grok only; on codex the same GIF is forwarded without being parsed:

| Case | Why refused | Error path |
| --- | --- | --- |
| animated GIF | one PNG cannot carry the frames; forwarding frame 0 would substitute a still the user never sent. `image` will not answer "how many frames" (`GifDecoder::read_image` composites frame 0 and returns `Ok`), so llmux walks the container's own block chain — a truncated chain counts as unknown, not as one frame | `….source.data` |
| declared `width × height × 4` > **128 MiB** | a 99-byte GIF can declare 65535×65535 in its logical screen descriptor (a 16 GiB frame) — the 20 MiB compressed cap sees none of that. This is the only size limit on the path (the `gif` backend's 50 MB per-frame default is not consulted when `image` hands it a caller-owned buffer), so the refusal is always llmux's typed 400 naming the dimensions, never a decoder-internal "not decodable" that blames the payload | `….source` |
| PNG would exceed **20 MiB** | enforced by the writer *during* encoding, so the oversized buffer is never finished | `….content[n]` |
| undecodable / truncated GIF | nothing to convert | `….source.data` |

A media type outside the flavor's accepted set is a 400 naming the field path, the flavor
and what that flavor would have taken:
`messages[0].content[0].source.media_type: unsupported image media type \`image/bmp\`
(codex accepts png, jpeg, webp, gif)`.

Grok also rejects images under 512 total pixels ("Image has 256 total pixels (16x16), which
is below the minimum of 512") and dimensions under 8 px. That is the gateway's own rule,
not llmux's: such an image is forwarded and the upstream 400 propagates, exactly as today.

**Cost.** `validate_request` (the pre-refresh gate) and `build_responses_body` (the
provider) each run the full conversion, so a grok-bound GIF is decoded and re-encoded
**twice per request** — bounded by the same per-image budgets, entirely local, and always
before any upstream call. Every other (flavor, format) pair pays nothing, because nothing
is decoded. Removing the second pass means threading the converted body from the gate into
the provider, which is a `src/proxy/forward.rs` change and not part of this unit.

### R2 — tools

`tools`, `tool_choice` and `parallel_tool_calls` travel as a trio: **with no tools (absent
or empty), all three are omitted on BOTH flavors.** Without tools `auto`/`none` are
vacuous, there is nothing to parallelize, and xAI is documented to reject a `tool_choice`
with no tools. Validation still runs first, so `{"type":"any"}` with no tools is an error,
not a vacuous omission.

A tool entry with no `name` is refused. Dropping it (the old behavior) leaves the model
believing in a capability that silently does not exist; llmux cannot forward or execute a
server-side tool, and will not invent one.

### R3 — `max_tokens`

Codex cannot take an output cap at all (the 400 above), so the field is omitted and named
in the report. Grok takes it as `max_output_tokens`, forwarded verbatim with the
`max_tokens_semantics` warning because budget equivalence is unproven (§1).

llmux never fakes the cap it could not forward: no local truncation, no clamping, no
synthesized `max_tokens` stop. An invalid value is refused rather than repaired — a client
that asked for `max_tokens: 0` has a bug worth seeing.

Absent and `null` are **not** the same thing. An absent `max_tokens` is a no-cap body (what
the codex idle probe and `count_tokens` send) and is legal. An explicit
`"max_tokens": null` is a limit the client did send and is not a positive integer, so it is
refused — treating it as absent would silently omit a field the client wrote, which is the
class of loss this module exists to stop.

### R4 — prior reasoning

**The backend's own reasoning is carried across turns (issue #239, 2026-11-10).** Every
codex response carries a `reasoning` output item with `encrypted_content` (codex asks for it
with `include: ["reasoning.encrypted_content"]`; grok returns it unasked). llmux hands it to
the client and takes it back:

1. **Response.** For a client that asked for thinking (`thinking.type` `enabled` or
   `adaptive`), each reasoning item that carries `encrypted_content` becomes one `thinking`
   block at the item's position: empty `thinking`, and `signature`
   `llmux-rs1:<codex|grok>:<encrypted_content>`, streamed as a `signature_delta` before the
   block closes and present in the non-stream aggregate. A client that did not ask for
   thinking gets no thinking block, as on the Anthropic API.
2. **Request.** An assistant `thinking` block whose signature carries THIS backend's
   ciphertext goes upstream as `{"type":"reasoning","summary":[],"encrypted_content":…}` at
   its position, before the call it reasoned about. Every other `thinking` /
   `redacted_thinking` block (an Anthropic signature, the other backend's ciphertext, an
   unsigned summary) is omitted and reported, as before.
3. **Native Messages.** A `/model` switch back to Claude or OpenRouter strips any
   `llmux-rs1:` thinking block with the unsigned ones (`anthropic::strip_foreign_thinking`):
   it is not an Anthropic signature.

Claude Code sends every thinking block back (`context_management.edits[0]` is
`clear_thinking_20251015` with `keep: "all"` on 347 of 347 sampled conversations,
2026-11-10), so the replayed items are a stable part of the prompt prefix.

Live receipts (2026-11-10, synthetic). Turn 1 asks the model to choose a secret codeword in
its reasoning, never show it, call a tool, then print `<time> <codeword>`; turn 2 sends the
tool result.

| Turn 2 on | codex `gpt-6-astra` | grok `grok-4.7` |
| --- | --- | --- |
| reasoning replayed (with or without the item `id`) | `quincunx` ×3, 0 new reasoning tokens | `narthex` ×2, 41 and 74 reasoning tokens |
| reasoning not replayed (before this change) | `oubliette`, then `orrery`: invented each time, 33 reasoning tokens | `catafalque`, then `apricity`; 631 and 1,494 reasoning tokens |
| replayed to another model (`gpt-6-sol`, `gpt-6.1-sol`) | used (`quincunx`) | not probed |
| replayed under another codex account | 200, silently ignored (same input tokens as no replay) | not probed |

A second codex probe removes word-choice bias: turn 1 picks a random 8-digit number in its
reasoning. Replayed, turn 2 answered `58392017` three times out of three with 0 new
reasoning tokens; not replayed, it answered `58391627`, `72941683`, `58392017`, reasoning
anew each time (26, 23, 32 tokens). Replayed items do not break the prompt cache: the
repeated replay requests read 1,536 of 1,628 input tokens from cache.

End to end through this code (`CodexProvider::build_request` → codex → the converter with
replay on → the next turn), a Claude Code-shaped tool loop: turn 1 came back as
`[thinking(signature llmux-rs1:codex:…), tool_use]`; turn 2 went upstream as
`[user, reasoning, function_call, function_call_output]`; all three turns sent one
`session-id` equal to `prompt_cache_key`.

Cost, stated plainly: a replayed item is real context. It counted +35 input tokens for 33
reasoning tokens, and still counted after a later user message (codex probe, same day) —
the same as the codex CLI, which replays every reasoning item, and as Claude itself under
`keep: "all"`. The ciphertext is ~8K characters per turn (median 7,928 over 91 codex
items); the local `count_tokens` estimate skips `signature` values so it is not inflated.

**`prompt_cache_key` and `session-id` are keyed by the shared prefix (issue #240,
2026-11-10).** codex sends one key as both — the codex CLI's source notes that "ChatGPT
derives cache affinity from the Responses session-id header". The key is
`responses::prefix_cache_key`: a UUID-shaped SHA-256 over the TRANSLATED `tools`, or over
`instructions` when no tools are sent — what is on the wire, so a client that spells the
same prefix differently (a string or block `system`, a tool with or without its default
schema, `cache_control`, the billing-header line) still gets one key. Every request that
can reuse one cached prefix — each turn of a conversation, a session's subagents, a new
conversation with the same tool set — shares the route that holds it; different tool sets
spread across routes.

Why the prefix and not the conversation. Before, llmux sent one id per daemon process. A
first fix (#275) sent one key per conversation, the codex CLI's per-thread policy; after it
shipped, the next-turn cache ratio on owner-mac fell from 0.901 to 0.821 on codex
(27 minutes against the 90 before), with every low-cache pair keeping its full prefix. A
controlled A/B on the live codex gateway (2026-11-10) found the cause:

| New request | under its own key | under the shared key |
| --- | --- | --- |
| same 17K-token instructions prefix, no tools | 0 / 17,127 cached (3/3) | 16,896 / 17,127 (3/3) |
| same 60 tools (~16.6K tokens), different instructions | 0 / 16,845 (2/2) | 16,640 / 16,845 (2/2) |

So the backend reuses a cached prefix only on the route the key picks, and tools precede
the instructions in that prefix. Sharing needs byte-identical tools: a tool whose
description carries something per session (a Slack MCP tool naming the thread it serves,
say) gives each session its own route — one cold start per session, never per
conversation. A per-conversation key starts every new conversation
cold; the per-process key put every request on one route. The native Responses frontend
keeps the client's own `prompt_cache_key` and routes the `session-id` header with it too.
Grok still sends no `prompt_cache_key`: its cache hit rate is already 97.4%
(2026-11-09), so there is no measured need.

### R5 — the report

```rust
CompatibilityReport { omitted_fields: Vec<&'static str>, warnings: Vec<&'static str> }
```

`omitted_fields` = fields NOT forwarded (`max_tokens`, `thinking`, `redacted_thinking`,
`thinking_config`).
`warnings` = that set plus caveats that omit nothing (`max_tokens_semantics`; the gemini
translator shares the type and adds `system_message_as_user`). Both
are sorted and deduped, so a transcript with twenty thinking blocks reports `thinking`
once and the header value is stable request to request.

The proxy turns the report into `X-Llmux-Omitted-Fields` / `X-Llmux-Compatibility-Warnings`
and a structured WARN log (`src/proxy/forward.rs`). Those are **machine-readable
warnings, not a user-visible error** — which is exactly why unsupported content is a 400
instead of a warning: a header cannot carry "your image never arrived".

### R6 — counting

`validate_request(…, count_tokens = true)` applies the same structural checks, refuses
images (no honest image-token estimate exists — base64 length is not a token count), and
skips the inference-only issues (`max_tokens`, `thinking_config`), since counting sends no
output budget or generation control anywhere. Their SHAPE is still validated: a misspelled
`thinking` config is a client bug worth reporting on either endpoint.

### R7 — generation controls

`temperature`, `top_p`, `top_k` and `stop_sequences` were read by nothing before this unit:
the turn ran at the backend default while the client believed it had set a value. Neither
subscription endpoint's acceptance of them is verified — the public vendor APIs are not
these endpoints — so a non-null sampling control or a non-empty `stop_sequences` is a local
400 rather than a forward-on-assumption or a silent ignore. llmux does not emulate stop
sequences locally either. An explicit `null` control and an EMPTY `stop_sequences` array
ask for nothing and are accepted; a `stop_sequences` that is not an array is malformed.

The top-level `thinking` **config** is a different object from the per-message `thinking`
history blocks of R4, hence its own report name `thinking_config`. Its shape is validated
(`enabled` requires a positive-integer `budget_tokens`; `adaptive` and `disabled` stand
alone) and then it is dropped and reported. It is never translated: mapping `budget_tokens`
onto a `reasoning.effort` would invent a correspondence neither backend documents.

**No budget and no disable guarantee.** `thinking: {"type": "disabled"}` does NOT stop the
backend from reasoning (grok-4.6 documents that reasoning cannot be turned off), and a
`budget_tokens` bounds nothing upstream. Reasoning effort on these backends comes from
llmux's own per-request resolution (`config.codex` / `config.grok`, `output_config.effort`),
not from this field; the proxy reads it only for an activity-log label.

This rule closes the controls known today. It is not a claim that every future Anthropic
request field is handled — a new one would land in whatever branch matches it, which is
why unknown CONTENT is refused rather than ignored.

### R8 — operator text (`system`)

Two channels, never mixed:

- Top-level `system` → `instructions`. It is the only input to `instructions`, so the
  value is byte-identical on every turn of a conversation. A leading
  `x-anthropic-billing-header: …` line (first line of a block, or a whole block) is left
  out: it is Anthropic billing metadata, not an instruction. The same words anywhere else
  stay.
- A mid-conversation `messages[].role:"system"` message → one
  `{"type":"message","role":"developer","content":[{"type":"input_text",…}]}` item **at
  its own position**. Its text blocks join with `\n`. Never merged with a neighbour,
  deduplicated or moved; an empty one produces no item; a non-text block is a 400.

Why. Claude Code (Agent SDK 2.1.28x, beta `mid-conversation-system-2026-04-07`) sends
operator text as system messages where it applies: environment and skill listings, agent
types, nested `CLAUDE.md`, background-task notifications, TodoWrite reminders. Until
2026-11-09 llmux folded all of them into `instructions`. Measured on owner-mac's
raw-io store, 2026-11-09 15:00–17:00:

| | codex (383 requests, 44 conversations) | grok (182 requests, 11 conversations) |
| --- | --- | --- |
| requests carrying mid-conversation system messages | 100% | 100% |
| share of `instructions` bytes from them | 87.6% (median 24.5K chars vs 2.8K top-level) | 89.3% |
| cache hit, next turn, `instructions` unchanged | median 97.7% | median 98.8% |
| cache hit, next turn, `instructions` grew | median 26.6% | median 1.9% |
| share of all uncached input tokens from those turns | 70% | 90% |

The prompt cache matches tools → `instructions` → `input` as one prefix, so every new
reminder turned the whole transcript behind it into a miss. Folding also lifted
time-bound notes ("task 7 completed", "TodoWrite has not been used recently" ×9) out of
time into standing instructions. Both backends' own clients append context changes as new
items instead: codex-rs never sends `role:"system"` and appends `developer`/`user`
context items (openai/codex `core/tests/suite/prompt_caching.rs`,
`overrides_turn_context_but_keeps_cached_prefix_and_key_constant`); xAI's caching guide
says "never edit, remove, or reorder earlier messages — only append new ones".

Receipts (live, through the daemon, 2026-11-09, synthetic fixtures):

| Probe | codex `gpt-6-astra` | grok `grok-4.7` |
| --- | --- | --- |
| `developer` item mid-conversation, rule "answer in ALL CAPS" | 200, followed | 200, followed |
| `developer` item as the LAST input item (after a `function_call_output`) | 200, followed | 200, followed |
| `developer` item between a `function_call` and its `function_call_output` | 200 | 200 |
| `role:"system"` input item | 400 "System messages are not allowed" (earlier receipt) | not probed |

Before/after on the wire (2026-11-09): one real Claude Code conversation (raw-io #12873)
replayed as two consecutive turns (`messages[:28]` then `messages[:31]`, a new system
message in between) straight to the codex gateway, `gpt-6-astra`, fresh cache key per
arm. The old arm's bodies are the pre-fix translation, byte-identical to the recorded
upstream body of #12873.

| | `instructions` turn 1 → 2 | turn 2 cached / input |
| --- | --- | --- |
| folded (before) | 26,524 → 26,910 chars | 22,016 / 73,303 (30.0%) |
| in place (after) | 2,756 → 2,756 chars | 72,704 / 73,295 (99.2%) |

Gemini carries the same messages as tagged user text (no operator role in `contents`):
[`gemini/spec.md`](../gemini/spec.md) §R3.

## 4. API

```rust
pub enum ResponsesFlavor { Codex, Grok }

pub fn validate_request(body: &Value, flavor: ResponsesFlavor, count_tokens: bool)
    -> Result<CompatibilityReport, ProviderError>;

pub fn build_responses_body(body: &Value, plan: &RequestPlan<'_>, flavor: ResponsesFlavor)
    -> Result<(Value, bool), ProviderError>;
```

Both run the SAME conversion, so a body that validates cannot fail to build and vice
versa; `build_responses_body` validates internally, so a direct caller cannot bypass the
checks. Re-exported through `provider::responses` (one import path for the shared
machinery); the adapters (`codex.rs`, `grok.rs`) pass their flavor at the one call site
each.

**Deviation from the shared-API sketch:** flavor is a third ARGUMENT rather than a
`RequestPlan` field. `RequestPlan` is adapter-resolved request shape (model, effort, tier);
the flavor is fixed per adapter and needed by validation, which has no plan. One argument
at one call site per adapter is the smaller surface, and it keeps `RequestPlan` unchanged
for the response-side owner.

## 5. Not in this unit

- **Terminal/output semantics** (`response.incomplete` → `max_tokens`, output identity,
  usage totals) — [`trace.md`](trace.md) and the converter owner. In particular
  `output_tokens` is the upstream total INCLUDING reasoning and is never reduced to the
  client's `max_tokens`.
- **The HTTP policy layer** — `X-Llmux-Compatibility: compat|strict`, header emission, the
  400 mapping for `InvalidRequest`, and the count endpoint's response — `src/proxy/forward.rs`.
- **Idle probe** — `src/proxy/idle_probe.rs` builds its codex probe through this translator
  and must construct a no-cap body rather than pretend a one-token budget.
- **Reasoning replay.** A provider-tagged encrypted-reasoning envelope (codex can carry
  one) would restore continuity; it needs its own provenance design and is deliberately
  absent here. No cross-provider crossover, ever.
- **Docs owned elsewhere:** `docs/README.md` (index row for this directory),
  `docs/operational-reference.md` (user-visible codex behavior), `docs/grok/spec.md`
  (records `prompt_cache_key` as sent — now codex-only). Owed by whoever lands the branch.
