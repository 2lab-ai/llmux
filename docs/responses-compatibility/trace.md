# Responses compatibility — vertical traces

> The Trace is the Source of Truth (STV). Code behaves as written here; divergence found
> during implementation updates THIS file first (Delta log at bottom). Line refs are
> `feat/responses-compatibility` @ 8df8b1b (pre-change) unless marked *(new)*.

Scope of this unit: two faithfulness gaps in the shared Responses machinery
(`src/provider/responses.rs`), shared by the codex and grok adapters.

- **U1 — counting.** `/v1/messages/count_tokens` under-reports because the estimate
  ignores `tools[]` entirely, and because its char atom counts only string *values*.
- **U2 — terminal output-limit semantics.** An upstream turn that stops on the output
  cap (`response.incomplete`) falls through every match arm and is reported to the
  client as a truncation error instead of a clean `stop_reason: "max_tokens"`.

Other Responses gaps (image blocks dropped, `max_tokens`/`tool_choice` ignored,
thinking blocks not replayed) are out of this unit.

---

## T1 — `POST /v1/messages/count_tokens` on a Responses-backed account

**0. Client surface** — Claude Code calls `count_tokens` before a turn to draw its
context bar. It sends the SAME Anthropic body it is about to POST to `/v1/messages`:
`system`, `messages[]`, and — always, in an agentic session — `tools[]` carrying the
full JSON Schema of every tool. The rendered number is `input_tokens` from the reply.

**1. API entry** — `POST /v1/messages/count_tokens`, proxy client auth as today.
No upstream call: Responses backends have no `count_tokens` equivalent
(`forward.rs:2258-2289`, `translate_count_tokens_response`).

**2. Input** — Anthropic Messages body.

> **Contract boundary.** Under the overall API contract §6, this endpoint VALIDATES:
> malformed JSON and a malformed `messages`/`system`/`tools` structure answer a local
> **HTTP 400**, never a fallback `1`, and an image-bearing body is a 400 (no reliable
> image token estimate exists — base64 must never be counted as characters). That
> validation lives in `validate_request` + the count handler, owned by the request /
> proxy units, NOT here.
>
> `estimate_input_tokens` is the **low-level numeric helper** underneath it: it is
> total-only, infallible, and keeps its `.max(1)` floor so it can never return 0 for a
> validated body. The pre-existing `forward.rs:2274-2276` behavior (parse failure →
> estimate `1` → HTTP 200) is the OLD unit behavior and is superseded by §6; changing
> it is outside this file's boundary.

**3. Layer flow (transformation arrows)** —

```
ctx.body (bytes)
  → serde_json::from_slice::<Value>                      forward.rs:2274
  → responses::estimate_input_tokens(&body)              responses.rs:332
      body.system   → section_chars(system)     (string VALUES only, recursive)
      body.messages → section_chars(messages)   (string VALUES only, recursive)
      body.tools    → serialized_chars(tools)   (new) — FULL serialized JSON
      → (sum / 4).max(1)
  → json!({"input_tokens": estimate})                    forward.rs:2282
```

The `tools` arrow is the unit's change. `section_chars` (responses.rs:313) recurses
into arrays and object *values* and sums `Value::String` lengths — object **keys**,
braces, and non-string scalars contribute 0. For prose (`system`, `messages`) that is
a fair chars/4 proxy. For a JSON Schema it is not: in
`{"type":"object","properties":{"file_path":{"type":"string","description":"…"}}}`
the property NAME `file_path`, the key `properties`, and every brace are real tokens
the model pays for, and `section_chars` counts none of them. A single Claude Code
tool set is tens of thousands of characters of schema, so omitting `tools` entirely
(today) under-reports the prompt by the dominant term.

`serialized_chars(value) = value.to_string().chars().count()` *(new,
responses.rs)* — the compact serde_json rendering, keys and delimiters included. It is
applied to `tools` only: `system`/`messages` keep `section_chars` so this unit does not
move numbers that existing callers and tests already pin
(`codex.rs:1163-1172`). Key ORDER does not affect the count, so the value is stable
whether or not serde_json preserves insertion order.

This stays a heuristic, not a tokenizer. It is exposed as a heuristic: the doc comment
and the `estimate_` prefix both say estimate, and `forward.rs:2277` logs it as
`count_tokens estimate: N`. No exact-tokenizer claim is made anywhere.

**4. Side effects** — none (no upstream call, no config write). `ctx.emit_finished`
records a 200 with no token counts; deliberately not codex-traced
(`forward.rs:2264-2267`).

**5. Error paths** — the helper has none by construction: it is infallible and
total. Every rejection (malformed JSON, malformed section structure, images) is a
local **HTTP 400** raised by the validating layer BEFORE this helper is reached, per
§6 — see the contract boundary in §2. Reaching the helper means the body already
validated, so the only degenerate input left is an empty/section-less body → `1`
(floor).

**6. Output** — helper: `u64 ≥ 1`. Endpoint (owned elsewhere): `200
{"input_tokens": n}` plus `X-Llmux-Token-Count: n`, or a local 400.

**7. Observability** — `=== RESPONSE ({group} count_tokens estimate: {n}) ===` in the
request log (`forward.rs:2277-2279`).

### T1 worked example (hand-derived; the contract test's literals)

```json
{"system":"abcd","messages":[{"role":"user","content":"efghijkl"}],"tools":[{"name":"ls"}]}
```

- `system` → `section_chars` → 4
- `messages` → `section_chars` → `"user"` 4 + `"efghijkl"` 8 = 12
- `tools` → `serialized_chars` → `[{"name":"ls"}]` = 15
- total 31 → `31 / 4` = **7**

Without the tools arrow the same body yields `16 / 4` = 4.

---

## T2 — Upstream turn hits the output cap (`response.incomplete`)

**0. Client surface** — Claude Code shows "response was truncated (max tokens)" and
may offer to continue. It reads `stop_reason` from `message_delta` (streaming) or from
the aggregate Messages document (non-streaming). An `event: error` instead makes the
turn look like a provider failure, and llmux additionally scores it as a provider
failure for scheduling (`forward.rs:2489-2494`, `provider_failure`).

**1. API entry** — no new HTTP entry. This is the SSE boundary
`ResponsesSseConverter::on_event` (responses.rs:761), fed complete upstream events by
`sse::transform_body` (streaming, `forward.rs:2472`) or by the aggregate loop
(`forward.rs:2622-2647`).

**2. Input** — one upstream Responses SSE event. The relevant envelope:

```json
{"type":"response.incomplete","sequence_number":N,
 "response":{"id":"resp_…","object":"response","status":"incomplete",
             "incomplete_details":{"reason":"max_output_tokens"},
             "model":"…","output":[…],
             "usage":{"input_tokens":120,
                      "input_tokens_details":{"cached_tokens":64},
                      "output_tokens":302,
                      "output_tokens_details":{"reasoning_tokens":286},
                      "total_tokens":422}}}
```

The output split is the live grok cap-16 receipt (terminal event
`response.incomplete`, reason `max_output_tokens`, 302 output tokens of which 286 are
reasoning). `output_tokens` is the upstream TOTAL, reasoning included: it is reported
as 302 and never subtracted, clamped to the client's `max_tokens`, or cosmetically
reconciled with the 16-token cap. This is accounting-token bookkeeping only — it is
not a claim about what anyone is billed, nor that reasoning tokens are free.

Validation: `response.incomplete_details.reason` is the discriminator. Absent or
unrecognized → error path (§5), never a synthesized success.

**3. Layer flow (transformation arrows)** —

```
"response.incomplete"                                   responses.rs on_event (new arm)
  → Self::incomplete(out, value.response)               (new)
      response.incomplete_details.reason
        == "max_output_tokens"  → complete(out, response, Some("max_tokens"))
        == <any other str>      → fail(out, "upstream response incomplete: {reason}")
        == absent               → fail(out, "upstream response incomplete: reason missing")

"response.completed"                                    responses.rs:924
  → if response.status == "incomplete" → Self::incomplete(…)   (new guard)
    else                               → complete(out, response, None)   (unchanged)

complete(out, response, forced_stop)                    responses.rs:585 (signature widened)
  response.usage                     → self.raw_usage (verbatim, for the trace)
  usage.input_tokens                 → total_input
  usage.input_tokens_details.cached_tokens → cached (clamped to total_input)
  total_input − cached               → StreamUsage.input_tokens
  usage.output_tokens                → StreamUsage.output_tokens
  forced_stop                        → stop_reason      ← takes precedence
    else saw_tool_use == true        → "tool_use"
    else                             → "end_turn"
  stop_reason                        → self.stop_reason (read by into_message_json:710)
                                     → message_delta.delta.stop_reason (SSE)
```

The forced stop reason is applied in ONE place (`complete`), so the streamed
`message_delta` and the non-stream aggregate are the same value by construction —
they cannot drift.

Anthropic-side mapping rationale: `max_output_tokens` is exactly Anthropic's
`stop_reason: "max_tokens"` — the turn ended for a known, benign, terminal reason and
the usage it reports is real. `content_filter` (the only other reason OpenAI documents
today) has **no** faithful Anthropic stop_reason; presenting it as a finished turn
would show a censored turn as a complete one, so it takes the error path with the
reason named in the message. Same for any future reason: unknown means error, not
success.

Note the interaction with `max_tokens` dropping (PROV-10, `codex.rs:501-514`): llmux
does not forward the client's `max_tokens` to `/responses`, so `max_output_tokens`
here reflects the BACKEND's output cap, not the client's. That is the only cap the
client can observe on this path — and it stays that way, because a live probe shows
Codex OAuth rejecting the parameter outright
(`400 {"detail":"Unsupported parameter: max_output_tokens"}`). Forwarding the client's
cap is therefore not a fix available on the codex backend; only grok honours it.

**3b. Tool arguments that are not an executable object** — the danger is
`into_message_json` (responses.rs) doing
`serde_json::from_str(&block.tool_args).unwrap_or_else(|_| json!({}))`, which turns a
truncated `{"file_path":"/et` into `{}` — a **syntactically valid, executable tool call
with the arguments erased**. A client that runs it runs the wrong operation. A scalar
or array (`42`, `[1,2]`) is the same class: valid JSON, not a valid Anthropic
`tool_use.input`, which must be an OBJECT.

The correct handling depends on WHY the turn ended, so the rule is split by terminal
reason (contract §9):

| `tool_args` | normal `completed` | `incomplete` / capped |
| --- | --- | --- |
| valid JSON **object** (`{"v":42}`) | `tool_use` with that input — unchanged | same (the cap landed after the JSON closed) |
| empty string | `tool_use` with `{}` — a genuinely completed no-arg call, still supported | tool **never executable**: block dropped from the aggregate, and the stream is given a truncation marker (below) |
| non-empty, unparseable | **protocol error** — `event: error` / aggregate `None` / 502 | block dropped from the aggregate |
| non-empty, parses to a non-object | **protocol error** | block dropped from the aggregate |

**Normal completion is an error, not a drop.** On a turn the upstream itself declares
complete, arguments that do not form an object mean the protocol was violated — nothing
truncated it. Dropping the block there would hand the client a successful-looking
`stop_reason: "tool_use"` turn with the tool silently missing, i.e. a lie about a
finished response. It fails the whole response instead, naming the tool, so both legs
agree (`event: error` on the stream, `None` → 502 on the aggregate).

**Capped completion drops the tool and keeps everything else.** Partial text is
preserved verbatim and `stop_reason: "max_tokens"` still reports why — the turn really
was cut, so a missing tool is the honest rendering, and the client is told to continue
rather than to run anything.

**The stream leg cannot retract, so it marks instead.** By the time a cap fires, the
tool's `content_block_start {"type":"tool_use", id, name, "input":{}}` is already on
the wire. Where partial arguments were streamed, the client's accumulated
`partial_json` is already unparseable and the block is inert. Where NO arguments
arrived (the interrupted no-arg case), the accumulation would parse as nothing at all
and a lenient client could materialize `{}` and execute it — so before closing an
interrupted tool block the converter emits one final
`input_json_delta {"partial_json": "{"}`. That single byte is a deliberate, documented
truncation marker: it makes the accumulated argument JSON unparseable for every client,
so an interrupted tool can never become an executable `{}` on either leg.

The marker is appended to the block's `tool_args` BUFFER (not emitted directly), so the
ordinary `pump` delivers it whenever that block reaches the wire. It therefore covers
tool items that were still BUFFERED when the cap fired as well as the one holding the
wire — a buffered call would otherwise be wired by the terminal flush and emitted as a
pristine `input: {}`. The condition is per tool item: marked when the item is NOT
`done` and its accumulated arguments would still parse into something (an empty buffer,
or a scalar/array that is valid JSON but not a valid `input`). A tool closed by its own
`output_item.done` before the cap is untouched and keeps its arguments — including a
genuinely argument-less one, which stays `{}` on both legs (T2 test 5).

This is the one place stream and aggregate legs differ in CONTENT; they never differ in
`stop_reason`. Surfacing the partial arguments as a *labelled* non-executable block has
no Anthropic wire representation and stays out of scope.

**4. Side effects** — `self.finished = true`, `self.stop_reason`, `self.usage`,
`self.cached_input_tokens`, `self.raw_usage` set exactly as on the completed path.
Downstream: `converter.usage()` feeds `state.totals.record`
(`forward.rs:2495`, `forward.rs:2649-2651`) — a capped turn records its full upstream
`output_tokens`, reasoning included (accounting bookkeeping, not a billing claim).
`converter.error_message()` stays `None`, so `provider_failure`
(`forward.rs:2489`) does NOT mark the account as failing for a cap hit.

**5. Error paths** —

| Condition | Emitted | `error_message()` | aggregate |
| --- | --- | --- | --- |
| reason `max_output_tokens` | `message_delta`(`max_tokens`) + `message_stop` | `None` | `Some` |
| reason `content_filter` / any other | `event: error`, `api_error`, message names the reason | `Some(msg)` | `None` |
| `incomplete_details` / `reason` absent | `event: error`, `…: reason missing` | `Some(msg)` | `None` |
| normal `completed`, tool args not an object | `event: error`, message names the tool | `Some(msg)` | `None` |
| already `finished` (error arrived first) | nothing (existing guard, responses.rs:586/570) | unchanged | unchanged |

Error legs produce no `message_delta`/`message_stop`, so a failed turn can never be
mistaken for a finished one. Aggregate `None` makes `forward.rs:2708-2715` return
`502` with the message — the existing contract.

**6. Output** —

Streaming: `…content_block_stop`, then
`message_delta {"delta":{"stop_reason":"max_tokens","stop_sequence":null},
"usage":{input_tokens, cache_read_input_tokens, cache_creation_input_tokens,
output_tokens}}`, then `message_stop`.

Non-streaming: `into_message_json()` → `{… "stop_reason":"max_tokens", "usage":{…}}`
with the same numbers.

**7. Observability** — `warn!` on a dropped truncated tool block (provider tag +
tool name + byte length; never the argument bytes). Everything else rides the
existing request log and codex trace: `raw_usage` is captured on the incomplete path
too, so the trace shows the real reasoning/total splits of a capped turn.

---

## T3 — Output identity: interleaved / out-of-order upstream items

**0. Client surface** — Claude Code keys every streamed delta by the Anthropic
`index` of its content block and concatenates text blocks in order. A tool call whose
`input_json_delta`s were dropped arrives as an empty-argument tool call: the client
either runs the wrong operation or stalls. This is the defect this trace closes.

**1. API entry** — same SSE boundary as T2, `ResponsesSseConverter::on_event`.

**2. Input** — Responses events carry the identity of the item they belong to.
Deltas carry top-level `item_id` + `output_index` + `content_index`; lifecycle events
carry `output_index` + `item.id`:

```json
{"type":"response.output_item.added","output_index":0,
 "item":{"id":"msg_1","type":"message","role":"assistant","content":[]}}
{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,
 "content_index":0,"delta":"I will "}
{"type":"response.output_item.added","output_index":1,
 "item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"Bash",
         "arguments":""}}
{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":1,
 "delta":"{\"command\":\"ls\"}"}
{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,
 "content_index":0,"delta":"check."}
{"type":"response.output_item.done","output_index":1,"item":{"id":"fc_1", …}}
{"type":"response.output_item.done","output_index":0,"item":{"id":"msg_1", …}}
```

Note both interleaving (a text delta for item 0 arriving after item 1 opened) and
**non-sequential `output_item.done`** (item 1 completes before item 0).

Older captures (and the existing codex tests) carry NEITHER `item_id` nor
`output_index`. Identity is therefore optional, and its absence must reproduce the
pre-existing single-active-block behavior exactly.

**3. Layer flow (transformation arrows)** —

Identity has TWO axes. One item can carry several CONTENT PARTS whose deltas
interleave with each other (`A1 B1 A2 B2` across `content_index` 0 and 1 of a single
message item), so keying on the item alone still scrambles them into one block as
`A1B1A2B2`. A block is therefore one (item, part) pair.

```
event                                  → item_key(event)                  (new)
  item_id                              → "id:{item_id}"
  else item.id                         → "id:{item.id}"
  else output_index                    → "idx:{output_index}"
  else                                 → None  (legacy: "whatever is active")

event                                  → part_index(event)                (new)
  content_index                        → that                (message content)
  else summary_index                   → that                (reasoning summaries)
  else                                 → 0   (item-level events address part 0,
                                              the part every item starts with)

part_key(event) = "{item_key}#{part_index}"   — None when item_key is None

part_key → route(key, item, kind)      → position in self.blocks          (new)
  existing block with same key+kind    → that block          (NEVER "the last block")
  key present, no match                → push a new block, key + item recorded
  key absent, active block same kind   → the active block    (legacy behavior)
  key absent, otherwise                → push a new block

response.output_text.delta             → blocks[pos].text     += delta
response.reasoning_summary_text.delta  → blocks[pos].text     += delta
response.function_call_arguments.delta → blocks[pos].tool_args += delta
                                         (routing is unconditional — accumulation
                                          NEVER depends on which block is on the wire)
  then                                 → pump(pos)
```

Accumulation and wire emission are now **two separate concerns**. `AggBlock` gains
`key` (part identity), `item` (the part-independent half, for item-level lifecycle
events), `wire_index: Option<usize>` (the Anthropic index while the block is open) and
`emitted: usize` (bytes already sent as deltas). `pump(pos)` emits the
un-emitted tail of block `pos` — but only when `pos` is the block currently open on
the wire, because Anthropic SSE allows exactly one open content block at a time.
Anything else stays buffered in the `AggBlock` and is flushed later. Nothing is ever
discarded.

**Wire arbitration (who holds the single open block):**

```
output_item.added(function_call)  → route_new(key, ToolUse), record call_id/name
                                  → pump+close whatever is open, then open the tool
                                    (a tool block takes and KEEPS the wire)
output_item.added(message)        → route_new(key, Text); open it only if the wire
                                    is free (a running tool block is not preempted)
text/thinking delta               → accumulate; open the wire for it only if free
function_call_arguments.delta     → accumulate; pump if that tool holds the wire
content_part.done(part_key)       → mark THAT part done; if it holds the wire:
  (+ reasoning_summary_part.done)   pump, close, release → flush_pending(), so a
                                    buffered sibling part starts streaming at once
                                    instead of draining at the terminal event
output_item.done(item_key)        → mark EVERY part of that item done; if one of
                                    them holds the wire: pump, close, release →
                                    flush_pending()
                                    (key absent → applies to the open block: legacy)
terminal (complete/incomplete)    → flush_all() to exhaustion, then the tail
flush_pending()                   → while the wire is free and some block is PENDING,
                                    open it in ITEM/PART ORDER, pump; close it if it
                                    is already done, else leave it open for deltas
```

**Pending means "never wired OR still holding bytes"** — not "holds bytes". A block
whose whole payload is empty (an argument-less `function_call` buffered behind another
live tool) has nothing to pump, so a bytes-only predicate skips it forever: it never
reaches the wire, yet `into_message_json` still lists it. That is a stream/aggregate
divergence in the worst direction — the non-streaming client is told about a tool call
the streaming client never saw. `AggBlock.wired` records that a block has held the wire
at least once; it is set in `start_wire` and NEVER reset, because `wire_index` is
cleared on close and therefore cannot distinguish "not yet opened" from "already
finished". Termination still holds: every flush iteration either returns or
wires-and-closes a block, which permanently removes it from the pending set.

Tool blocks hold the wire rather than yielding to interleaved text because a tool
block **cannot be split**: two Anthropic `tool_use` blocks for one upstream call would
be two half-written calls. A text block splits harmlessly — clients concatenate text
blocks — and the AGGREGATE merges by key, so the text stays whole and in item order
there regardless of how the wire sliced it.

**Upstream text is relayed verbatim.** Deltas are appended byte-for-byte; there is no
regex, no scrub, no filter. Literal XML, `<…>`-looking text, or anything else
that resembles an internal artifact is model output and ships unchanged.

**4. Side effects** — `self.blocks` gains one entry per upstream (item, part) pair,
not per wire block. `next_index` still allocates Anthropic indexes in the order blocks
reach the wire, so indexes stay dense and monotonic. `saw_tool_use` is unchanged. An
item announcement creates its part-0 block, so a delta for `content_index: 0` reuses
it and no duplicate empty block is produced.

**5. Error paths** — a `function_call_arguments.delta` whose key matches no known
tool item (arguments for an item never `added`) is dropped with a `debug!`: inventing
a `tool_use` with an empty id/name would be worse than losing it. Legacy keyless
argument deltas keep today's behavior (applied only when a tool block is active).
Unparseable/non-object accumulated arguments then meet the §3b rule at terminal time.

**6. Output** — every upstream item's content reaches the client: text verbatim, tool
arguments complete. Anthropic indexes remain dense, every opened block is closed
exactly once, and `content_block_stop` precedes `message_delta`.

**7. Observability** — `debug!` on an unroutable argument delta (provider tag + key).

---

## Contract tests (derived from the arrows above)

All in `src/provider/responses.rs::tests` *(new module)*; gate
`cargo test provider::responses`.

| # | Test | Source § | Catches |
| --- | --- | --- | --- |
| 1 | `estimate_input_tokens_counts_serialized_tool_schemas` | T1 §3 + worked example | `tools` omitted from the sum |
| 2 | `estimate_input_tokens_counts_tool_property_names` | T1 §3 | counting tools with the string-values-only atom (property names free) |
| 3 | `estimate_input_tokens_floor_and_toolless_bodies_unchanged` | T1 §5 | regression: existing callers' numbers moved |
| 4 | `response_incomplete_max_output_tokens_stops_cleanly` | T2 §6 | the fall-through: cap hit reported as `error` |
| 5 | `incomplete_max_tokens_overrides_tool_use_stop_reason` | T2 §3 arrows | `saw_tool_use` winning over the forced reason |
| 6 | `incomplete_retains_upstream_usage` | T2 §3/§4 | usage zeroed or cache split lost on the incomplete path |
| 7 | `completed_event_with_incomplete_status_uses_incomplete_contract` | T2 §3 guard | trusting the envelope over `status` |
| 8 | `unknown_incomplete_reason_is_an_error_not_a_success` | T2 §5 | `content_filter` silently presented as a finished turn |
| 9 | `incomplete_without_a_reason_is_an_error` | T2 §5 | missing discriminator defaulting to success |
| 10 | `ordinary_completed_keeps_end_turn_and_tool_use` | T2 §3 | the widened `complete` signature changing normal turns |
| 11 | `truncated_tool_arguments_never_become_an_executable_empty_input` | T2 §3b | the `{}` fabrication |
| 12 | `argumentless_tool_call_still_aggregates_to_empty_input` | T2 §3b table | over-correcting §3b into dropping real no-arg calls |
| 13 | `interleaved_text_and_tool_items_keep_every_argument_byte` | T3 §3 | argument deltas dropped while a text block is open |
| 14 | `interleaved_text_is_relayed_verbatim_and_whole` | T3 §3 | text lost or scrubbed while a tool holds the wire |
| 15 | `out_of_order_item_done_closes_the_right_block` | T3 §3 arbitration | `done` closing "whatever is last" |
| 16 | `two_concurrent_tool_items_do_not_merge_arguments` | T3 §3 routing | two open calls sharing one argument buffer |
| 17 | `malformed_tool_arguments_on_normal_completion_are_a_protocol_error` | §3b row 3 | drop+warn hiding a violated protocol as a finished turn |
| 18 | `scalar_tool_arguments_on_normal_completion_are_a_protocol_error` | §3b row 4 | `42` passing as a `tool_use.input` |
| 19 | `array_tool_arguments_on_normal_completion_are_a_protocol_error` | §3b row 4 | `[1,2]` passing as a `tool_use.input` |
| 20 | `interrupted_toolless_arguments_are_not_executable_on_the_stream` | §3b marker | an interrupted no-arg tool materializing as runnable `{}` |
| 21 | `anthropic_block_indexes_stay_dense_and_each_block_closes_once` | T3 §6 | index/close bookkeeping broken by buffering |
| 22 | `legacy_events_without_item_identity_keep_single_block_behavior` | T3 §2 | identity routing breaking pre-`output_index` captures |
| 23 | `interleaved_content_parts_stay_distinct_and_ordered` | T3 §3 part axis | `content_index` missing from the key → `A1B1A2B2` in one block |
| 24 | `content_part_done_releases_the_wire_for_the_next_part` | T3 §3 arbitration | part-level `done` ignored → siblings drain only at the tail |
| 25 | `a_buffered_argumentless_tool_call_still_reaches_the_stream` | T3 §3 pending | "pending = has bytes" → a zero-byte buffered call is in the aggregate but never on the wire |
| 26 | `a_buffered_tool_call_interrupted_by_the_cap_is_inert_on_the_stream` | §3b marker | marking only the wire holder → a buffered call is flushed as a pristine runnable `{}` |

## File map

| File | Change |
| --- | --- |
| `src/provider/responses.rs` | `serialized_chars` (new), `estimate_input_tokens` tools arrow, `complete(…, forced_stop)`, `incomplete` (new), `response.incomplete` arm, `response.completed` status guard, `item_key`/`route`/`pump`/`flush_pending` block registry (new), `AggBlock{key, wire_index, emitted, done}`, §3b terminal tool-argument rule, `mod tests` (new) |
| `docs/responses-compatibility/trace.md` | this file |

## Implementation status

| Unit | Status |
| --- | --- |
| U1 — tools-inclusive `count_tokens` estimate | **Verified** (tests 1-3 green) |
| U2 — `response.incomplete` terminal semantics | **Verified** (tests 4-12 green) |
| U3 — output identity / interleaving + §3b split rule | **Verified** (tests 13-22 green) |
| U4 — `content_index` part axis + part-level `done` | **Verified** (tests 23-24 green) |
| U5 — never-wired blocks are pending; marker covers buffered calls | **Verified** (tests 25-26 green) |

Gate: `CARGO_TARGET_DIR=…/llmux/target cargo test provider::responses::tests` → 26
passed, 0 failed; `cargo fmt --check` → clean; `cargo test --lib` → 1177 passed;
`cargo clippy --all-targets -- -D warnings` → clean.

## Trace deviations

- **MODIFIED** `usage_fixture` output split. *before*: `output_tokens: 1024`,
  `reasoning_tokens: 0` (invented round numbers). *after*: `output_tokens: 302`,
  `reasoning_tokens: 286`, `total_tokens: 422` — the live grok cap-16 receipt. Reason:
  the invented split hid the question the fixture exists to answer (whether
  `output_tokens` is the sum or the visible-only count). No contract change.
- **MODIFIED** §3b terminal tool-argument rule. *before*: unparseable arguments are
  always dropped with a `warn!`. *after*: dropping applies only to a CAPPED turn; on a
  normal `completed` turn, arguments that are not a JSON object are a protocol error
  (both legs), and the interrupted no-argument case gets an explicit stream truncation
  marker. Reason: dropping on a self-declared-complete turn ships a
  `stop_reason: "tool_use"` response with no tool in it — a lie about a finished turn
  — and an empty argument buffer was still materializable as an executable `{}` by a
  lenient client.
- **MODIFIED** request-side integration shape. *before* (coordinator's first sketch):
  `RequestPlan` gains a `flavor: ResponsesFlavor` field. *after*:
  `build_responses_body(body, plan, flavor)` takes the flavor as a third ARGUMENT and
  `RequestPlan` is unchanged. Reason: `responses_request.rs` was built that way, and a
  separate argument cannot be silently defaulted by a caller that forgets the field.
- **ADDED** the part axis to output identity. *before*: a block was one upstream ITEM
  (`item_id`/`output_index` only). *after*: a block is one (item, part) pair, keyed
  `"{item}#{content_index|summary_index}"`, plus `response.content_part.done` /
  `response.reasoning_summary_part.done` arms that finish a single part. Reason:
  contract §9 names `content_index`, and without it two interleaved content parts of
  ONE message item merge into `A1B1A2B2` (test 23 caught it as RED). Item-level
  `output_item.done` now finishes every part of the item, so no part is left open.
- **MODIFIED** the flush predicate and the truncation marker's reach (external review
  MUST-FIX). *before*: pending = "block holds un-emitted bytes"; the marker was applied
  only to the tool holding the wire. *after*: pending = "never wired OR holds bytes"
  (new `AggBlock.wired`), and the marker is written into the BUFFER of every un-`done`
  tool call so `pump` carries it wherever that block reaches the wire. Reason: two
  parallel calls where the second is argument-less left it with zero bytes to flush —
  it never reached the stream although the aggregate listed it; and once the fix wires
  it, a capped turn would have flushed it as a pristine runnable `input: {}`.
  Consequence: `aggregate_tool_input` no longer inspects `stop_reason` — an empty
  buffer now unambiguously means "upstream closed the item with no arguments".
- **REMOVED** the request translator from `responses.rs` (`build_responses_body`,
  `messages_to_input`, `tools_to_functions`, `build_instructions`, `system_text`,
  `input_role`, `message_text`, `tool_result_text` — 248 lines). Migration: the four
  request-side names are re-exported from `super::responses_request`, so
  `provider::responses::…` import paths are unchanged for every caller. Removed source
  archived at `scratchpad/removed-request-helpers.rs` for diff review.

## Out of scope (reported, not fixed)

- **Docs index.** `rules/documents.md` §3 wants a new `docs/` guide linked from
  `docs/README.md`. This is a design artifact (like `docs/grok/`), and
  `docs/README.md` is outside this unit's file boundary — the row and the link are
  owed by whoever lands the feature branch.
- **`docs/operational-reference.md`** owns user-visible Codex backend behavior; the
  `count_tokens` estimate becoming tools-inclusive is user-visible (the context bar
  moves). Outside this unit's file boundary.
- **Partial tool arguments have no Anthropic representation.** §3b drops them; a
  faithful "incomplete tool call" block would need a client-side contract that does
  not exist in the Anthropic wire format.
- **`content_filter`** is mapped to an error, not to a stop reason. If Anthropic's
  `refusal` stop reason is ever adopted by the clients llmux serves, this is the line
  to revisit.
- **Multimodal `count_tokens`.** Base64 image payloads counted as characters would
  inflate the chars/4 estimate wildly, so a multimodal body owes an explicit
  *unsupported* answer rather than a confidently wrong number. Contract pending; this
  unit's shared helper counts `tools` only.
- **Interleaved text ordering on the WIRE.** When a tool call holds the wire, text
  that arrives meanwhile is flushed after the tool block closes, so a streaming client
  renders it slightly later than upstream emitted it. Content is never lost and the
  aggregate keeps item order; splitting the tool call instead would produce two
  malformed calls, which is strictly worse. Revisit only if a client is observed
  caring about intra-turn text/tool ordering.
- **Reasoning `summary_index` is keyed but unproven against a live capture.** It is
  the documented analogue of `content_index` and is routed identically; no captured
  stream with two interleaved summary parts exists yet to confirm the shape.
